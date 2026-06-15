use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use thiserror::Error;
use yaaml_core::status::{BacklogStatus, Status, TaskFailure, WorkerStatus};
use yaaml_core::{
    extract_task_keys, infer_context_from_memory, infer_context_from_path, ContextMetadata,
    EmbeddingRecord, MemoryKind, MemoryRecord, MemoryScope, SessionRecord, SourceTurnRef,
    TaskRecord, TaskStatus, TurnRecord,
};

use crate::migrations::MIGRATIONS;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("failed to create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize JSON field: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub struct Database {
    conn: Connection,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvalRunRecord {
    pub id: i64,
    pub strategy: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub config_json: String,
    pub result_count: u64,
    pub session_id: Option<String>,
    pub turn_ordinal: Option<u64>,
    pub score: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvalResultRecord {
    pub id: i64,
    pub eval_run_id: i64,
    pub turn_id: i64,
    pub memory_id: Option<i64>,
    pub memory_title: Option<String>,
    pub judge_score: Option<String>,
    pub rationale: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecallEvalTaskRecord {
    pub id: i64,
    pub status: String,
    pub attempts: u64,
    pub max_attempts: u64,
    pub next_run_at: Option<String>,
    pub last_error: Option<String>,
    pub session_id: Option<String>,
    pub turn_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskListRecord {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub display_status: String,
    pub priority: i64,
    pub attempts: u64,
    pub max_attempts: u64,
    pub next_run_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub payload_json: String,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| DatabaseError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let conn = Connection::open(&path)?;
        configure_connection(&conn, true)?;
        Ok(Self { conn, path })
    }

    pub fn in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn, false)?;
        Ok(Self {
            conn,
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn migrate(&mut self) -> Result<(), DatabaseError> {
        let tx = self.conn.transaction()?;
        for migration in MIGRATIONS {
            tx.execute_batch(migration)?;
        }
        tx.commit()?;
        self.ensure_column("turns", "cwd", "ALTER TABLE turns ADD COLUMN cwd TEXT")?;
        self.ensure_column(
            "turns",
            "context_json",
            "ALTER TABLE turns ADD COLUMN context_json TEXT",
        )?;
        self.ensure_column(
            "memories",
            "memory_kind",
            "ALTER TABLE memories ADD COLUMN memory_kind TEXT NOT NULL DEFAULT 'lesson'",
        )?;
        self.ensure_column(
            "memories",
            "task_keys",
            "ALTER TABLE memories ADD COLUMN task_keys TEXT NOT NULL DEFAULT '[]'",
        )?;
        Ok(())
    }

    fn ensure_column(
        &self,
        table: &str,
        column: &str,
        alter_sql: &str,
    ) -> Result<(), DatabaseError> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(());
            }
        }
        self.conn.execute_batch(alter_sql)?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64, DatabaseError> {
        Ok(self
            .conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })?)
    }

    pub fn status(&self) -> Result<Status, DatabaseError> {
        let memory_count = self.count("SELECT COUNT(*) FROM memories")?;
        let active_memory_count =
            self.count("SELECT COUNT(*) FROM memories WHERE is_active = 1")?;
        let last_creation_at = self
            .conn
            .query_row("SELECT MAX(created_at) FROM memories", [], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        let last_recall_at = None;
        let backlog = self.backlog_status()?;
        let now = unix_now_seconds();
        let queued_jobs = self.count_with_param(
            "SELECT COUNT(*)
             FROM tasks
             WHERE status = 'queued'
               AND (
                   next_run_at IS NULL
                   OR next_run_at NOT LIKE 'unix:%'
                   OR CAST(substr(next_run_at, 6) AS INTEGER) <= ?1
               )",
            now,
        )?;
        let scheduled_jobs = self.count_with_param(
            "SELECT COUNT(*)
             FROM tasks
             WHERE status = 'queued'
               AND next_run_at LIKE 'unix:%'
               AND CAST(substr(next_run_at, 6) AS INTEGER) > ?1",
            now,
        )?;
        let running_jobs = self.count("SELECT COUNT(*) FROM tasks WHERE status = 'running'")?;
        let parked_jobs = self.count("SELECT COUNT(*) FROM tasks WHERE status = 'parked'")?;
        let recent_failures = self.recent_failures()?;

        Ok(Status {
            db_path: self.path.display().to_string(),
            memory_count,
            active_memory_count,
            last_creation_at,
            last_recall_at,
            backlog,
            workers: WorkerStatus {
                active_workers: running_jobs,
                queued_jobs,
                scheduled_jobs,
                running_jobs,
            },
            recent_failures,
            parked_jobs,
        })
    }

    pub fn table_names(&self) -> Result<Vec<String>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn upsert_session(&self, session: &SessionRecord) -> Result<(), DatabaseError> {
        self.conn.execute(
            "INSERT INTO sessions (
                id, agent_type, project_id, transcript_file_path, started_at, last_seen_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                agent_type = excluded.agent_type,
                project_id = excluded.project_id,
                transcript_file_path = excluded.transcript_file_path,
                started_at = COALESCE(sessions.started_at, excluded.started_at),
                last_seen_at = excluded.last_seen_at",
            params![
                session.id,
                session.agent_type.as_str(),
                session.project_id,
                session.transcript_file_path,
                session.started_at,
                session.last_seen_at
            ],
        )?;
        let context = infer_context_from_path(Path::new(&session.project_id));
        self.upsert_context_metadata(
            "session",
            &session.id,
            &context,
            session.last_seen_at.as_deref().unwrap_or("unknown"),
        )?;
        Ok(())
    }

    pub fn session_by_transcript_path(
        &self,
        transcript_path: &str,
    ) -> Result<Option<SessionRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id, agent_type, project_id, transcript_file_path, started_at, last_seen_at
                 FROM sessions
                 WHERE transcript_file_path = ?1",
                params![transcript_path],
                read_session_record,
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn session_by_id(&self, session_id: &str) -> Result<Option<SessionRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id, agent_type, project_id, transcript_file_path, started_at, last_seen_at
                 FROM sessions
                 WHERE id = ?1",
                params![session_id],
                read_session_record,
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn latest_session_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<SessionRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id, agent_type, project_id, transcript_file_path, started_at, last_seen_at
                 FROM sessions
                 WHERE project_id = ?1
                 ORDER BY COALESCE(last_seen_at, started_at) DESC, id DESC
                 LIMIT 1",
                params![project_id],
                read_session_record,
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn sessions_with_completed_turn_counts(
        &self,
    ) -> Result<Vec<(SessionRecord, u64)>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.agent_type, s.project_id, s.transcript_file_path, s.started_at,
                    s.last_seen_at, COUNT(t.id)
             FROM sessions s
             JOIN turns t ON t.session_id = s.id
             WHERE t.status = 'completed'
             GROUP BY s.id, s.agent_type, s.project_id, s.transcript_file_path, s.started_at,
                      s.last_seen_at
             ORDER BY COALESCE(s.last_seen_at, s.started_at), s.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((read_session_record(row)?, i64_to_u64(row.get(6)?)))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    pub fn insert_turn(&self, turn: &TurnRecord) -> Result<bool, DatabaseError> {
        let context_json = turn
            .context
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO turns (
                session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status,
                display_text, cwd, context_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                turn.session_id,
                turn.turn_id,
                u64_to_i64(turn.ordinal),
                u64_to_i64(turn.byte_start),
                u64_to_i64(turn.byte_end),
                turn.observed_at,
                turn.status.as_str(),
                turn.display_text,
                turn.cwd,
                context_json,
            ],
        )?;
        Ok(inserted > 0)
    }

    pub fn turns_for_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<TurnRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
             FROM turns
             WHERE session_id = ?1
             ORDER BY ordinal DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, u64_to_i64(limit as u64)],
            read_turn_record,
        )?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        turns.reverse();
        Ok(turns)
    }

    pub fn completed_turns_for_session_range(
        &self,
        session_id: &str,
        start_ordinal: u64,
        end_ordinal: u64,
    ) -> Result<Vec<TurnRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
             FROM turns
             WHERE session_id = ?1
               AND status = 'completed'
               AND ordinal >= ?2
               AND ordinal < ?3
             ORDER BY ordinal ASC",
        )?;
        let rows = stmt.query_map(
            params![
                session_id,
                u64_to_i64(start_ordinal),
                u64_to_i64(end_ordinal)
            ],
            read_turn_record,
        )?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        Ok(turns)
    }

    pub fn completed_turns_for_source_refs(
        &self,
        refs: &[SourceTurnRef],
    ) -> Result<Vec<TurnRecord>, DatabaseError> {
        let mut turns = Vec::new();
        for source_ref in refs {
            let turn = self
                .conn
                .query_row(
                    "SELECT session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
                     FROM turns
                     WHERE session_id = ?1
                       AND status = 'completed'
                       AND ordinal = ?2",
                    params![source_ref.session_id, u64_to_i64(source_ref.ordinal)],
                    read_turn_record,
                )
                .optional()?;
            if let Some(turn) = turn {
                turns.push(turn);
            }
        }
        turns.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then(left.ordinal.cmp(&right.ordinal))
        });
        Ok(turns)
    }

    pub fn completed_turn_count_for_session(&self, session_id: &str) -> Result<u64, DatabaseError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND status = 'completed'",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(i64_to_u64(count))
    }

    pub fn turn_row_id_for_session_ordinal(
        &self,
        session_id: &str,
        ordinal: u64,
    ) -> Result<Option<i64>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id FROM turns WHERE session_id = ?1 AND ordinal = ?2",
                params![session_id, u64_to_i64(ordinal)],
                |row| row.get(0),
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn completed_turns_for_session_after_ordinal(
        &self,
        session_id: &str,
        ordinal: u64,
        limit: usize,
    ) -> Result<Vec<TurnRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
             FROM turns
             WHERE session_id = ?1
               AND status = 'completed'
               AND ordinal > ?2
             ORDER BY ordinal ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session_id, u64_to_i64(ordinal), u64_to_i64(limit as u64)],
            read_turn_record,
        )?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        Ok(turns)
    }

    pub fn next_turn_ordinal_for_session(&self, session_id: &str) -> Result<u64, DatabaseError> {
        let max_ordinal: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(ordinal) FROM turns WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(max_ordinal.map(i64_to_u64).unwrap_or(0) + u64::from(max_ordinal.is_some()))
    }

    pub fn task_payload_exists(
        &self,
        kind: &str,
        payload_json: &str,
    ) -> Result<bool, DatabaseError> {
        let exists: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE kind = ?1 AND payload_json = ?2)",
            params![kind, payload_json],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn recall_eval_exists_for_anchor(
        &self,
        session_id: &str,
        turn_ordinal: u64,
    ) -> Result<bool, DatabaseError> {
        let turn_ordinal = u64_to_i64(turn_ordinal);
        let task_exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tasks
                WHERE kind = 'recall_eval'
                  AND json_extract(payload_json, '$.session_id') = ?1
                  AND CAST(json_extract(payload_json, '$.turn_ordinal') AS INTEGER) = ?2
             )",
            params![session_id, turn_ordinal],
            |row| row.get(0),
        )?;
        if task_exists != 0 {
            return Ok(true);
        }
        let eval_exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM eval_runs
                WHERE strategy = 'recall_1_to_5'
                  AND json_extract(config_json, '$.session_id') = ?1
                  AND CAST(json_extract(config_json, '$.turn_ordinal') AS INTEGER) = ?2
             )",
            params![session_id, turn_ordinal],
            |row| row.get(0),
        )?;
        Ok(eval_exists != 0)
    }

    pub fn recall_eval_rerun_exists(&self, source_eval_run_id: i64) -> Result<bool, DatabaseError> {
        let task_exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tasks
                WHERE kind = 'recall_eval'
                  AND CAST(json_extract(payload_json, '$.rerun_for_eval_run_id') AS INTEGER) = ?1
                  AND status IN ('queued', 'running', 'completed')
             )",
            params![source_eval_run_id],
            |row| row.get(0),
        )?;
        if task_exists != 0 {
            return Ok(true);
        }
        let eval_exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM eval_runs
                WHERE strategy = 'recall_1_to_5'
                  AND CAST(json_extract(config_json, '$.rerun_for_eval_run_id') AS INTEGER) = ?1
             )",
            params![source_eval_run_id],
            |row| row.get(0),
        )?;
        Ok(eval_exists != 0)
    }

    pub fn insert_memory(&self, memory: &MemoryRecord) -> Result<i64, DatabaseError> {
        let source_turn_refs = serde_json::to_string(&memory.source_turn_refs)?;
        let lineage_refs = serde_json::to_string(&memory.lineage_refs)?;
        let task_keys = serde_json::to_string(&memory.task_keys)?;
        self.conn.execute(
            "INSERT INTO memories (
                title, body, scope, memory_kind, task_keys, source_turn_refs, created_at,
                updated_at, is_active, session_id, project_id, project_descriptor, lineage_refs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                memory.title,
                memory.body,
                memory.scope.as_str(),
                memory.kind.as_str(),
                task_keys,
                source_turn_refs,
                memory.created_at,
                memory.updated_at,
                if memory.is_active { 1 } else { 0 },
                memory.session_id,
                memory.project_id,
                memory.project_descriptor,
                lineage_refs
            ],
        )?;
        let memory_id = self.conn.last_insert_rowid();
        let context = infer_context_from_memory(memory);
        self.upsert_context_metadata(
            "memory",
            &memory_id.to_string(),
            &context,
            &memory.updated_at,
        )?;
        Ok(memory_id)
    }

    pub fn upsert_context_metadata(
        &self,
        entity_type: &str,
        entity_key: &str,
        context: &ContextMetadata,
        updated_at: &str,
    ) -> Result<(), DatabaseError> {
        let context_json = serde_json::to_string(context)?;
        self.conn.execute(
            "INSERT INTO context_metadata (entity_type, entity_key, context_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(entity_type, entity_key) DO UPDATE SET
                context_json = excluded.context_json,
                updated_at = excluded.updated_at",
            params![entity_type, entity_key, context_json, updated_at],
        )?;
        Ok(())
    }

    pub fn context_metadata(
        &self,
        entity_type: &str,
        entity_key: &str,
    ) -> Result<Option<ContextMetadata>, DatabaseError> {
        let json = self
            .conn
            .query_row(
                "SELECT context_json FROM context_metadata
                 WHERE entity_type = ?1 AND entity_key = ?2",
                params![entity_type, entity_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(DatabaseError::from))
            .transpose()
    }

    pub fn consolidate_memories(
        &self,
        source_memory_ids: &[i64],
        consolidated: &MemoryRecord,
        updated_at: &str,
    ) -> Result<i64, DatabaseError> {
        let consolidated_id = self.insert_memory(consolidated)?;
        for source_id in source_memory_ids {
            self.conn.execute(
                "UPDATE memories
                 SET is_active = 0,
                     updated_at = ?1
                 WHERE id = ?2",
                params![updated_at, source_id],
            )?;
        }
        Ok(consolidated_id)
    }

    pub fn deactivate_memory(&self, memory_id: i64, updated_at: &str) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE memories
             SET is_active = 0,
                 updated_at = ?1
             WHERE id = ?2",
            params![updated_at, memory_id],
        )?;
        Ok(())
    }

    pub fn deactivate_active_memories(&self, updated_at: &str) -> Result<u64, DatabaseError> {
        let updated = self.conn.execute(
            "UPDATE memories
             SET is_active = 0,
                 updated_at = ?1
             WHERE is_active = 1",
            params![updated_at],
        )?;
        Ok(updated as u64)
    }

    pub fn list_memories(&self) -> Result<Vec<MemoryRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, scope, memory_kind, task_keys, source_turn_refs,
                    created_at, updated_at, is_active, session_id, project_id,
                    project_descriptor, lineage_refs
             FROM memories
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], read_memory_record)?;
        let mut memories = Vec::new();
        for row in rows {
            memories.push(row?);
        }
        Ok(memories)
    }

    pub fn list_active_memories_by_ids(
        &self,
        memory_ids: &[i64],
    ) -> Result<Vec<MemoryRecord>, DatabaseError> {
        let mut memories = Vec::new();
        for memory_id in memory_ids {
            let memory = self
                .conn
                .query_row(
                    "SELECT id, title, body, scope, memory_kind, task_keys, source_turn_refs,
                            created_at, updated_at, is_active, session_id, project_id,
                            project_descriptor, lineage_refs
                     FROM memories
                     WHERE id = ?1 AND is_active = 1",
                    params![memory_id],
                    read_memory_record,
                )
                .optional()?;
            if let Some(memory) = memory {
                memories.push(memory);
            }
        }
        Ok(memories)
    }

    pub fn list_memories_by_ids(
        &self,
        memory_ids: &[i64],
    ) -> Result<Vec<MemoryRecord>, DatabaseError> {
        let mut memories = Vec::new();
        for memory_id in memory_ids {
            let memory = self
                .conn
                .query_row(
                    "SELECT id, title, body, scope, memory_kind, task_keys, source_turn_refs,
                            created_at, updated_at, is_active, session_id, project_id,
                            project_descriptor, lineage_refs
                     FROM memories
                     WHERE id = ?1",
                    params![memory_id],
                    read_memory_record,
                )
                .optional()?;
            if let Some(memory) = memory {
                memories.push(memory);
            }
        }
        Ok(memories)
    }

    pub fn list_active_memories_created_before(
        &self,
        observed_at: &str,
    ) -> Result<Vec<MemoryRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, scope, memory_kind, task_keys, source_turn_refs,
                    created_at, updated_at, is_active, session_id, project_id,
                    project_descriptor, lineage_refs
             FROM memories
             WHERE is_active = 1 AND created_at < ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![observed_at], read_memory_record)?;
        let mut memories = Vec::new();
        for row in rows {
            memories.push(row?);
        }
        Ok(memories)
    }

    pub fn list_turns(&self, limit: usize) -> Result<Vec<TurnRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
             FROM turns
             ORDER BY observed_at, id
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![u64_to_i64(limit as u64)], read_turn_record)?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        Ok(turns)
    }

    pub fn list_turns_with_ids(
        &self,
        limit: usize,
    ) -> Result<Vec<(i64, TurnRecord)>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text, cwd, context_json
             FROM turns
             ORDER BY observed_at, id
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![u64_to_i64(limit as u64)], |row| {
            Ok((row.get(0)?, read_turn_record_from_offset(row, 1)?))
        })?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        Ok(turns)
    }

    pub fn upsert_embedding(&self, embedding: &EmbeddingRecord) -> Result<(), DatabaseError> {
        self.conn.execute(
            "INSERT INTO embeddings (
                memory_id, embedding_model, dimensions, embedding_blob, embedded_text_hash, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(memory_id) DO UPDATE SET
                embedding_model = excluded.embedding_model,
                dimensions = excluded.dimensions,
                embedding_blob = excluded.embedding_blob,
                embedded_text_hash = excluded.embedded_text_hash,
                updated_at = excluded.updated_at",
            params![
                embedding.memory_id,
                embedding.embedding_model,
                u64_to_i64(embedding.dimensions),
                embedding.embedding_blob,
                embedding.embedded_text_hash,
                embedding.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn get_embedding(&self, memory_id: i64) -> Result<Option<EmbeddingRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT memory_id, embedding_model, dimensions, embedding_blob, embedded_text_hash, updated_at
                 FROM embeddings
                 WHERE memory_id = ?1",
                params![memory_id],
                |row| {
                    Ok(EmbeddingRecord {
                        memory_id: row.get(0)?,
                        embedding_model: row.get(1)?,
                        dimensions: i64_to_u64(row.get(2)?),
                        embedding_blob: row.get(3)?,
                        embedded_text_hash: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn get_cursor(&self, file_path: &str) -> Result<u64, DatabaseError> {
        let offset = self
            .conn
            .query_row(
                "SELECT last_byte_offset FROM file_cursors WHERE file_path = ?1",
                params![file_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(i64_to_u64(offset))
    }

    pub fn update_cursor(
        &self,
        file_path: &str,
        last_byte_offset: u64,
        last_processed_at: Option<&str>,
    ) -> Result<(), DatabaseError> {
        self.conn.execute(
            "INSERT INTO file_cursors (file_path, last_byte_offset, last_processed_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(file_path) DO UPDATE SET
                last_byte_offset = excluded.last_byte_offset,
                last_processed_at = excluded.last_processed_at",
            params![file_path, u64_to_i64(last_byte_offset), last_processed_at],
        )?;
        Ok(())
    }

    pub fn cursor_exists(&self, file_path: &str) -> Result<bool, DatabaseError> {
        let exists: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_cursors WHERE file_path = ?1)",
            params![file_path],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn cursor_paths(&self) -> Result<Vec<PathBuf>, DatabaseError> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path FROM file_cursors ORDER BY file_path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(PathBuf::from(row?));
        }
        Ok(paths)
    }

    pub fn enqueue_task(&self, task: &TaskRecord) -> Result<i64, DatabaseError> {
        self.conn.execute(
            "INSERT INTO tasks (
                kind, status, priority, payload_json, attempts, max_attempts,
                next_run_at, last_error, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.kind,
                task.status.as_str(),
                task.priority,
                task.payload_json,
                u64_to_i64(task.attempts),
                u64_to_i64(task.max_attempts),
                task.next_run_at,
                task.last_error,
                task.created_at,
                task.updated_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn next_queued_task(&self, now_seconds: i64) -> Result<Option<TaskRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id, kind, status, priority, payload_json, attempts, max_attempts,
                        next_run_at, last_error, created_at, updated_at
                 FROM tasks
                 WHERE status = 'queued'
                   AND (
                       next_run_at IS NULL
                       OR next_run_at NOT LIKE 'unix:%'
                       OR CAST(substr(next_run_at, 6) AS INTEGER) <= ?1
                   )
                 ORDER BY priority DESC, id ASC
                 LIMIT 1",
                params![now_seconds],
                read_task_record,
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn count_tasks_by_status(
        &self,
        kind: &str,
        status: TaskStatus,
    ) -> Result<u64, DatabaseError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE kind = ?1 AND status = ?2",
            params![kind, status.as_str()],
            |row| row.get(0),
        )?;
        Ok(i64_to_u64(count))
    }

    pub fn latest_task_updated_at(
        &self,
        kind: &str,
        status: TaskStatus,
    ) -> Result<Option<String>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT updated_at
                 FROM tasks
                 WHERE kind = ?1 AND status = ?2
                 ORDER BY id DESC
                 LIMIT 1",
                params![kind, status.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn latest_active_memory_created_at(&self) -> Result<Option<String>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT created_at
                 FROM memories
                 WHERE is_active = 1
                 ORDER BY id DESC
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn list_recall_eval_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<RecallEvalTaskRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, status, attempts, max_attempts, next_run_at, last_error,
                    json_extract(payload_json, '$.session_id') AS session_id,
                    CAST(json_extract(payload_json, '$.turn_ordinal') AS INTEGER) AS turn_ordinal
             FROM tasks
             WHERE kind = 'recall_eval'
               AND status IN ('queued', 'running')
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![u64_to_i64(limit as u64)], |row| {
            let turn_ordinal: Option<i64> = row.get(7)?;
            Ok(RecallEvalTaskRecord {
                id: row.get(0)?,
                status: row.get(1)?,
                attempts: i64_to_u64(row.get(2)?),
                max_attempts: i64_to_u64(row.get(3)?),
                next_run_at: row.get(4)?,
                last_error: row.get(5)?,
                session_id: row.get(6)?,
                turn_ordinal: turn_ordinal.map(i64_to_u64),
            })
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    pub fn list_tasks(
        &self,
        display_status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskListRecord>, DatabaseError> {
        let now = unix_now_seconds();
        let status_clause = match display_status {
            Some("queued") => {
                "status = 'queued'
                 AND (
                     next_run_at IS NULL
                     OR next_run_at NOT LIKE 'unix:%'
                     OR CAST(substr(next_run_at, 6) AS INTEGER) <= ?1
                 )"
            }
            Some("scheduled") => {
                "status = 'queued'
                 AND next_run_at LIKE 'unix:%'
                 AND CAST(substr(next_run_at, 6) AS INTEGER) > ?1"
            }
            Some("running") => "?1 = ?1 AND status = 'running'",
            Some("parked") => "?1 = ?1 AND status = 'parked'",
            Some("completed") => "?1 = ?1 AND status = 'completed'",
            Some(_) | None => "?1 = ?1",
        };
        let sql = format!(
            "SELECT id, kind, status, priority, payload_json, attempts, max_attempts,
                    next_run_at, last_error, created_at, updated_at
             FROM tasks
             WHERE {status_clause}
             ORDER BY id DESC
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![now, u64_to_i64(limit as u64)], |row| {
            read_task_list_record(row, now)
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    pub fn mark_task_running(&self, task_id: i64, updated_at: &str) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE tasks
             SET status = 'running',
                 updated_at = ?1
             WHERE id = ?2",
            params![updated_at, task_id],
        )?;
        Ok(())
    }

    pub fn complete_task(&self, task_id: i64, updated_at: &str) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE tasks
             SET status = 'completed',
                 updated_at = ?1
             WHERE id = ?2",
            params![updated_at, task_id],
        )?;
        Ok(())
    }

    pub fn retry_task(&self, task_id: i64, updated_at: &str) -> Result<bool, DatabaseError> {
        let updated = self.conn.execute(
            "UPDATE tasks
             SET status = 'queued',
                 attempts = 0,
                 next_run_at = NULL,
                 last_error = NULL,
                 updated_at = ?1
             WHERE id = ?2
               AND status IN ('queued', 'running', 'parked')",
            params![updated_at, task_id],
        )?;
        Ok(updated > 0)
    }

    pub fn clear_task(&self, task_id: i64) -> Result<bool, DatabaseError> {
        let deleted = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
        Ok(deleted > 0)
    }

    pub fn clear_tasks_by_display_status(
        &self,
        display_status: &str,
        kind: Option<&str>,
    ) -> Result<u64, DatabaseError> {
        let now = unix_now_seconds();
        let status_clause = match display_status {
            "queued" => {
                "status = 'queued'
                 AND (
                     next_run_at IS NULL
                     OR next_run_at NOT LIKE 'unix:%'
                     OR CAST(substr(next_run_at, 6) AS INTEGER) <= ?1
                 )"
            }
            "scheduled" => {
                "status = 'queued'
                 AND next_run_at LIKE 'unix:%'
                 AND CAST(substr(next_run_at, 6) AS INTEGER) > ?1"
            }
            "running" => "?1 = ?1 AND status = 'running'",
            "parked" => "?1 = ?1 AND status = 'parked'",
            "completed" => "?1 = ?1 AND status = 'completed'",
            _ => return Ok(0),
        };
        let deleted = if let Some(kind) = kind {
            let sql = format!("DELETE FROM tasks WHERE {status_clause} AND kind = ?2");
            self.conn.execute(&sql, params![now, kind])?
        } else {
            let sql = format!("DELETE FROM tasks WHERE {status_clause}");
            self.conn.execute(&sql, params![now])?
        };
        Ok(deleted as u64)
    }

    pub fn requeue_running_tasks(&self, updated_at: &str) -> Result<u64, DatabaseError> {
        let updated = self.conn.execute(
            "UPDATE tasks
             SET status = 'queued',
                 updated_at = ?1
             WHERE status = 'running'",
            params![updated_at],
        )?;
        Ok(updated as u64)
    }

    pub fn park_task(
        &self,
        task_id: i64,
        error: &str,
        updated_at: &str,
    ) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE tasks
             SET status = 'parked',
                 last_error = ?1,
                 updated_at = ?2
             WHERE id = ?3",
            params![error, updated_at, task_id],
        )?;
        Ok(())
    }

    pub fn add_backlog_progress(
        &self,
        discovered_files: u64,
        processed_files: u64,
        processed_turns: u64,
        queued_memory_jobs: u64,
        failures: u64,
        last_activity_at: Option<&str>,
    ) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE backlog_progress
             SET discovered_files = discovered_files + ?1,
                 processed_files = processed_files + ?2,
                 processed_turns = processed_turns + ?3,
                 queued_memory_jobs = queued_memory_jobs + ?4,
                 failures = failures + ?5,
                 last_activity_at = COALESCE(?6, last_activity_at)
             WHERE id = 1",
            params![
                u64_to_i64(discovered_files),
                u64_to_i64(processed_files),
                u64_to_i64(processed_turns),
                u64_to_i64(queued_memory_jobs),
                u64_to_i64(failures),
                last_activity_at
            ],
        )?;
        Ok(())
    }

    pub fn insert_eval_run(
        &self,
        strategy: &str,
        started_at: &str,
        config_json: &str,
    ) -> Result<i64, DatabaseError> {
        self.conn.execute(
            "INSERT INTO eval_runs (strategy, started_at, completed_at, config_json)
             VALUES (?1, ?2, NULL, ?3)",
            params![strategy, started_at, config_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn complete_eval_run(
        &self,
        eval_run_id: i64,
        completed_at: &str,
    ) -> Result<(), DatabaseError> {
        self.conn.execute(
            "UPDATE eval_runs SET completed_at = ?1 WHERE id = ?2",
            params![completed_at, eval_run_id],
        )?;
        Ok(())
    }

    pub fn insert_eval_result(
        &self,
        eval_run_id: i64,
        turn_id: i64,
        memory_id: Option<i64>,
        judge_score: &str,
        rationale: &str,
        created_at: &str,
    ) -> Result<i64, DatabaseError> {
        self.conn.execute(
            "INSERT INTO eval_results (
                eval_run_id, turn_id, memory_id, judge_score, rationale, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                eval_run_id,
                turn_id,
                memory_id,
                judge_score,
                rationale,
                created_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn eval_scores_for_run(&self, eval_run_id: i64) -> Result<Vec<String>, DatabaseError> {
        let mut stmt = self
            .conn
            .prepare("SELECT judge_score FROM eval_results WHERE eval_run_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![eval_run_id], |row| row.get(0))?;
        let mut scores = Vec::new();
        for row in rows {
            scores.push(row?);
        }
        Ok(scores)
    }

    pub fn list_eval_runs(&self, limit: usize) -> Result<Vec<EvalRunRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.strategy, r.started_at, r.completed_at, r.config_json,
                    COUNT(er.id) AS result_count,
                    json_extract(r.config_json, '$.session_id') AS session_id,
                    CAST(json_extract(r.config_json, '$.turn_ordinal') AS INTEGER) AS turn_ordinal,
                    CASE
                        WHEN COUNT(er.judge_score) = 0 THEN NULL
                        WHEN COUNT(DISTINCT er.judge_score) = 1 THEN MAX(er.judge_score)
                        ELSE group_concat(DISTINCT er.judge_score)
                    END AS score
             FROM eval_runs r
             LEFT JOIN eval_results er ON er.eval_run_id = r.id
             GROUP BY r.id, r.strategy, r.started_at, r.completed_at, r.config_json
             ORDER BY r.id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![u64_to_i64(limit as u64)], read_eval_run_record)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    pub fn eval_run_by_id(&self, eval_run_id: i64) -> Result<Option<EvalRunRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT r.id, r.strategy, r.started_at, r.completed_at, r.config_json,
                        COUNT(er.id) AS result_count,
                        json_extract(r.config_json, '$.session_id') AS session_id,
                        CAST(json_extract(r.config_json, '$.turn_ordinal') AS INTEGER) AS turn_ordinal,
                        CASE
                            WHEN COUNT(er.judge_score) = 0 THEN NULL
                            WHEN COUNT(DISTINCT er.judge_score) = 1 THEN MAX(er.judge_score)
                            ELSE group_concat(DISTINCT er.judge_score)
                        END AS score
                 FROM eval_runs r
                 LEFT JOIN eval_results er ON er.eval_run_id = r.id
                 WHERE r.id = ?1
                 GROUP BY r.id, r.strategy, r.started_at, r.completed_at, r.config_json",
                params![eval_run_id],
                read_eval_run_record,
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn eval_results_for_run(
        &self,
        eval_run_id: i64,
    ) -> Result<Vec<EvalResultRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT er.id, er.eval_run_id, er.turn_id, er.memory_id, m.title,
                    er.judge_score, er.rationale, er.created_at
             FROM eval_results er
             LEFT JOIN memories m ON m.id = er.memory_id
             WHERE er.eval_run_id = ?1
             ORDER BY er.id",
        )?;
        let rows = stmt.query_map(params![eval_run_id], read_eval_result_record)?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    fn count(&self, sql: &str) -> Result<u64, DatabaseError> {
        let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
        Ok(count.try_into().unwrap_or(0))
    }

    fn count_with_param(&self, sql: &str, value: i64) -> Result<u64, DatabaseError> {
        let count: i64 = self.conn.query_row(sql, params![value], |row| row.get(0))?;
        Ok(count.try_into().unwrap_or(0))
    }

    fn backlog_status(&self) -> Result<BacklogStatus, DatabaseError> {
        let mut backlog = self.conn.query_row(
            "SELECT discovered_files, processed_files, processed_turns, queued_memory_jobs, failures, last_activity_at
             FROM backlog_progress WHERE id = 1",
            [],
            |row| {
                Ok(BacklogStatus {
                    discovered_files: i64_to_u64(row.get(0)?),
                    processed_files: i64_to_u64(row.get(1)?),
                    processed_turns: i64_to_u64(row.get(2)?),
                    transcript_files: 0,
                    sessions: 0,
                    stored_turns: 0,
                    queued_memory_jobs: i64_to_u64(row.get(3)?),
                    failures: i64_to_u64(row.get(4)?),
                    last_activity_at: row.get(5)?,
                })
            },
        )?;
        backlog.transcript_files = self.count("SELECT COUNT(*) FROM file_cursors")?;
        backlog.sessions = self.count("SELECT COUNT(*) FROM sessions")?;
        backlog.stored_turns = self.count("SELECT COUNT(*) FROM turns")?;
        Ok(backlog)
    }

    fn recent_failures(&self) -> Result<Vec<TaskFailure>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, COALESCE(last_error, ''), updated_at
             FROM tasks
             WHERE last_error IS NOT NULL
             ORDER BY updated_at DESC, id DESC
             LIMIT 5",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok(TaskFailure {
                task_kind: row.get(0)?,
                error: row.get(1)?,
                failed_at: row.get(2)?,
            })
        })?;
        let mut failures = Vec::new();
        for row in rows {
            failures.push(row?);
        }
        Ok(failures)
    }
}

fn configure_connection(conn: &Connection, enable_wal: bool) -> Result<(), DatabaseError> {
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if enable_wal {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

fn i64_to_u64(value: i64) -> u64 {
    value.try_into().unwrap_or(0)
}

fn u64_to_i64(value: u64) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn task_display_status(status: &str, next_run_at: Option<&str>, now_seconds: i64) -> String {
    if status == "queued"
        && next_run_at
            .and_then(|timestamp| timestamp.strip_prefix("unix:"))
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .is_some_and(|seconds| seconds > now_seconds)
    {
        "scheduled".to_string()
    } else {
        status.to_string()
    }
}

fn read_task_list_record(
    row: &rusqlite::Row<'_>,
    now_seconds: i64,
) -> rusqlite::Result<TaskListRecord> {
    let status: String = row.get(2)?;
    let next_run_at: Option<String> = row.get(7)?;
    Ok(TaskListRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        display_status: task_display_status(&status, next_run_at.as_deref(), now_seconds),
        status,
        priority: row.get(3)?,
        payload_json: row.get(4)?,
        attempts: i64_to_u64(row.get(5)?),
        max_attempts: i64_to_u64(row.get(6)?),
        next_run_at,
        last_error: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn read_task_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    let status: String = row.get(2)?;
    Ok(TaskRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: match status.as_str() {
            "running" => TaskStatus::Running,
            "parked" => TaskStatus::Parked,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Queued,
        },
        priority: row.get(3)?,
        payload_json: row.get(4)?,
        attempts: i64_to_u64(row.get(5)?),
        max_attempts: i64_to_u64(row.get(6)?),
        next_run_at: row.get(7)?,
        last_error: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn read_session_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let agent_type: String = row.get(1)?;
    Ok(SessionRecord {
        id: row.get(0)?,
        agent_type: match agent_type.as_str() {
            "claude-code" => yaaml_core::AgentType::ClaudeCode,
            _ => yaaml_core::AgentType::Codex,
        },
        project_id: row.get(2)?,
        transcript_file_path: row.get(3)?,
        started_at: row.get(4)?,
        last_seen_at: row.get(5)?,
    })
}

fn read_eval_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvalRunRecord> {
    let turn_ordinal: Option<i64> = row.get(7)?;
    Ok(EvalRunRecord {
        id: row.get(0)?,
        strategy: row.get(1)?,
        started_at: row.get(2)?,
        completed_at: row.get(3)?,
        config_json: row.get(4)?,
        result_count: i64_to_u64(row.get(5)?),
        session_id: row.get(6)?,
        turn_ordinal: turn_ordinal.map(i64_to_u64),
        score: row.get(8)?,
    })
}

fn read_eval_result_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvalResultRecord> {
    Ok(EvalResultRecord {
        id: row.get(0)?,
        eval_run_id: row.get(1)?,
        turn_id: row.get(2)?,
        memory_id: row.get(3)?,
        memory_title: row.get(4)?,
        judge_score: row.get(5)?,
        rationale: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn read_turn_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRecord> {
    read_turn_record_from_offset(row, 0)
}

fn read_turn_record_from_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<TurnRecord> {
    let status: String = row.get(offset + 6)?;
    let context_json: Option<String> = row.get(offset + 9)?;
    let context = match context_json {
        Some(json) => Some(serde_json::from_str(&json).map_err(json_decode_error)?),
        None => None,
    };
    Ok(TurnRecord {
        session_id: row.get(offset)?,
        turn_id: row.get(offset + 1)?,
        ordinal: i64_to_u64(row.get(offset + 2)?),
        byte_start: i64_to_u64(row.get(offset + 3)?),
        byte_end: i64_to_u64(row.get(offset + 4)?),
        observed_at: row.get(offset + 5)?,
        status: match status.as_str() {
            "aborted" => yaaml_core::TurnStatus::Aborted,
            _ => yaaml_core::TurnStatus::Completed,
        },
        display_text: row.get(offset + 7)?,
        cwd: row.get(offset + 8)?,
        context,
    })
}

fn read_memory_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let scope: String = row.get(3)?;
    let memory_kind: String = row.get(4)?;
    let task_keys_json: String = row.get(5)?;
    let source_turn_refs_json: String = row.get(6)?;
    let lineage_refs_json: String = row.get(13)?;
    let title: String = row.get(1)?;
    let body: String = row.get(2)?;
    let scope = match scope.as_str() {
        "global" => MemoryScope::Global,
        _ => MemoryScope::Project,
    };
    let source_turn_refs: Vec<SourceTurnRef> =
        serde_json::from_str(&source_turn_refs_json).map_err(json_decode_error)?;
    let mut task_keys: Vec<String> =
        serde_json::from_str(&task_keys_json).map_err(json_decode_error)?;
    if task_keys.is_empty() {
        task_keys = extract_task_keys(&format!("{title}\n{body}"));
    }
    let lineage_refs: Vec<i64> =
        serde_json::from_str(&lineage_refs_json).map_err(json_decode_error)?;
    let is_active: i64 = row.get(9)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        title,
        body,
        scope,
        kind: match memory_kind.as_str() {
            "preference" => MemoryKind::Preference,
            "workflow" => MemoryKind::Workflow,
            "project_fact" => MemoryKind::ProjectFact,
            "task_state" => MemoryKind::TaskState,
            _ => MemoryKind::Lesson,
        },
        task_keys,
        source_turn_refs,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        is_active: is_active != 0,
        session_id: row.get(10)?,
        project_id: row.get(11)?,
        project_descriptor: row.get(12)?,
        lineage_refs,
    })
}

fn json_decode_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

pub fn encode_f32_embedding(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

pub fn decode_f32_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if !blob.len().is_multiple_of(std::mem::size_of::<f32>()) {
        return None;
    }
    let mut values = Vec::with_capacity(blob.len() / std::mem::size_of::<f32>());
    for chunk in blob.chunks_exact(std::mem::size_of::<f32>()) {
        values.push(f32::from_le_bytes(chunk.try_into().ok()?));
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::EXPECTED_SCHEMA_VERSION;

    #[test]
    fn migrations_create_expected_tables() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        assert_eq!(db.schema_version().unwrap(), EXPECTED_SCHEMA_VERSION);
        let tables = db.table_names().unwrap();
        for expected in [
            "backlog_progress",
            "embeddings",
            "eval_results",
            "eval_runs",
            "file_cursors",
            "memories",
            "schema_version",
            "sessions",
            "tasks",
            "turns",
        ] {
            assert!(
                tables.iter().any(|table| table == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn file_database_uses_wal_and_busy_timeout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::open(tmp.path().join("yaaml.db")).unwrap();

        let busy_timeout_ms: i64 = db
            .conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        let journal_mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = db
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();

        assert_eq!(busy_timeout_ms, 30_000);
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 1);
    }

    #[test]
    fn empty_status_has_zero_counts() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        let status = db.status().unwrap();

        assert_eq!(status.memory_count, 0);
        assert_eq!(status.active_memory_count, 0);
        assert_eq!(status.backlog.discovered_files, 0);
        assert_eq!(status.workers.queued_jobs, 0);
        assert_eq!(status.parked_jobs, 0);
    }

    #[test]
    fn stores_session_turn_and_cursor() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        let session = SessionRecord {
            id: "session-1".to_string(),
            agent_type: yaaml_core::AgentType::Codex,
            project_id: "/tmp/project".to_string(),
            transcript_file_path: "/tmp/session.jsonl".to_string(),
            started_at: Some("2026-06-08T00:00:00Z".to_string()),
            last_seen_at: Some("2026-06-08T00:01:00Z".to_string()),
        };
        db.upsert_session(&session).unwrap();

        let turn = TurnRecord {
            session_id: session.id.clone(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 10,
            byte_end: 100,
            observed_at: Some("2026-06-08T00:01:00Z".to_string()),
            status: yaaml_core::TurnStatus::Completed,
            display_text: Some("hello".to_string()),
            cwd: None,
            context: None,
        };

        assert!(db.insert_turn(&turn).unwrap());
        assert!(!db.insert_turn(&turn).unwrap());

        db.update_cursor("/tmp/session.jsonl", 100, Some("2026-06-08T00:01:00Z"))
            .unwrap();
        assert_eq!(db.get_cursor("/tmp/session.jsonl").unwrap(), 100);
        assert!(db.cursor_exists("/tmp/session.jsonl").unwrap());
        assert!(!db.cursor_exists("/tmp/other.jsonl").unwrap());
        assert_eq!(
            db.cursor_paths().unwrap(),
            vec![PathBuf::from("/tmp/session.jsonl")]
        );
    }

    #[test]
    fn latest_session_for_project_prefers_most_recent_last_seen() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        for session in [
            SessionRecord {
                id: "old-session".to_string(),
                agent_type: yaaml_core::AgentType::Codex,
                project_id: "/tmp/project".to_string(),
                transcript_file_path: "/tmp/old.jsonl".to_string(),
                started_at: Some("2026-06-08T00:00:00Z".to_string()),
                last_seen_at: Some("2026-06-08T00:01:00Z".to_string()),
            },
            SessionRecord {
                id: "new-session".to_string(),
                agent_type: yaaml_core::AgentType::Codex,
                project_id: "/tmp/project".to_string(),
                transcript_file_path: "/tmp/new.jsonl".to_string(),
                started_at: Some("2026-06-08T00:00:00Z".to_string()),
                last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
            },
            SessionRecord {
                id: "other-project".to_string(),
                agent_type: yaaml_core::AgentType::Codex,
                project_id: "/tmp/other".to_string(),
                transcript_file_path: "/tmp/other.jsonl".to_string(),
                started_at: Some("2026-06-08T00:00:00Z".to_string()),
                last_seen_at: Some("2026-06-08T00:03:00Z".to_string()),
            },
        ] {
            db.upsert_session(&session).unwrap();
        }

        let session = db
            .latest_session_for_project("/tmp/project")
            .unwrap()
            .unwrap();

        assert_eq!(session.id, "new-session");
        assert!(db
            .latest_session_for_project("/tmp/missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn task_queue_returns_highest_priority_then_oldest() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        let task = |kind: &str, priority: i64| TaskRecord {
            id: None,
            kind: kind.to_string(),
            status: TaskStatus::Queued,
            priority,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        };

        db.enqueue_task(&task("backlog", 0)).unwrap();
        db.enqueue_task(&task("live", 10)).unwrap();
        db.enqueue_task(&task("backlog-2", 0)).unwrap();

        let next = db.next_queued_task(0).unwrap().unwrap();

        assert_eq!(next.kind, "live");
        assert_eq!(next.priority, 10);
    }

    #[test]
    fn task_queue_ignores_future_next_run_at() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        let task = |kind: &str, next_run_at: Option<&str>| TaskRecord {
            id: None,
            kind: kind.to_string(),
            status: TaskStatus::Queued,
            priority: 0,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: next_run_at.map(str::to_string),
            last_error: None,
            created_at: "unix:100".to_string(),
            updated_at: "unix:100".to_string(),
        };

        db.enqueue_task(&task("future", Some("unix:200"))).unwrap();
        assert!(db.next_queued_task(100).unwrap().is_none());

        db.enqueue_task(&task("due", Some("unix:99"))).unwrap();
        let next = db.next_queued_task(100).unwrap().unwrap();

        assert_eq!(next.kind, "due");
    }

    #[test]
    fn updates_backlog_progress_counters() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();

        db.add_backlog_progress(2, 1, 3, 4, 0, Some("2026-06-08T00:00:00Z"))
            .unwrap();
        db.add_backlog_progress(1, 2, 3, 4, 1, None).unwrap();

        let status = db.status().unwrap();

        assert_eq!(status.backlog.discovered_files, 3);
        assert_eq!(status.backlog.processed_files, 3);
        assert_eq!(status.backlog.processed_turns, 6);
        assert_eq!(status.backlog.queued_memory_jobs, 8);
        assert_eq!(status.backlog.failures, 1);
        assert_eq!(
            status.backlog.last_activity_at.as_deref(),
            Some("2026-06-08T00:00:00Z")
        );
    }

    #[test]
    fn parked_task_appears_in_status() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let task = TaskRecord {
            id: None,
            kind: "embedding".to_string(),
            status: TaskStatus::Queued,
            priority: 0,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        };
        let id = db.enqueue_task(&task).unwrap();

        db.park_task(id, "missing API key", "2026-06-08T00:00:01Z")
            .unwrap();
        let status = db.status().unwrap();

        assert_eq!(status.parked_jobs, 1);
        assert_eq!(status.recent_failures.len(), 1);
        assert_eq!(status.recent_failures[0].task_kind, "embedding");
        assert_eq!(status.recent_failures[0].error, "missing API key");
    }

    #[test]
    fn multiple_formulated_memories_persist_with_shared_source_refs() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let source_refs = vec![SourceTurnRef {
            session_id: "session-1".to_string(),
            ordinal: 3,
            byte_start: 100,
            byte_end: 250,
        }];
        let drafts = yaaml_core::parse_formulation_response(
            &serde_json::json!({
                "memories": [
                    {"title":"Recall files","body":"Skills read daemon-owned recall files.","project_descriptor":"yaaml, Rust"},
                    {"title":"Commit checkpoints","body":"Commit implementation milestones as progress is made.","scope":"global","project_descriptor":"agent workflow"}
                ]
            }),
            "yaaml, Rust",
            12_000,
        )
        .unwrap();

        for draft in drafts {
            let record = draft.into_record(
                source_refs.clone(),
                "2026-06-08T00:00:00Z".to_string(),
                Some("session-1".to_string()),
                Some("/tmp/yaaml".to_string()),
            );
            db.insert_memory(&record).unwrap();
        }
        let memories = db.list_memories().unwrap();

        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].source_turn_refs, source_refs);
        assert_eq!(memories[1].source_turn_refs, source_refs);
        assert_eq!(memories[0].scope, MemoryScope::Project);
        assert_eq!(memories[1].scope, MemoryScope::Global);
    }

    #[test]
    fn memory_insert_roundtrips_kind_and_task_keys() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = MemoryRecord {
            id: None,
            title: "PR memory".to_string(),
            body: "PR 481245 needs task-key aware recall.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: vec!["pr:481245".to_string(), "tool:yaaml".to_string()],
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: Vec::new(),
        };

        db.insert_memory(&memory).unwrap();
        let memories = db.list_memories().unwrap();

        assert_eq!(memories[0].kind, MemoryKind::TaskState);
        assert_eq!(
            memories[0].task_keys,
            vec!["pr:481245".to_string(), "tool:yaaml".to_string()]
        );
    }

    #[test]
    fn empty_persisted_task_keys_are_derived_on_read() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = MemoryRecord {
            id: None,
            title: "PR 481245".to_string(),
            body: "Risk Arbiter recall should prefer the matching PR.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: Vec::new(),
        };

        db.insert_memory(&memory).unwrap();
        let memories = db.list_memories().unwrap();

        assert!(memories[0].task_keys.contains(&"pr:481245".to_string()));
    }

    #[test]
    fn memory_insert_persists_context_metadata() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = MemoryRecord {
            id: None,
            title: "Sad Sack Signals".to_string(),
            body: "forge-signalsmith lifecycle job context.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/Users/tbedor".to_string()),
            project_descriptor: Some("tbedor, Node".to_string()),
            lineage_refs: Vec::new(),
        };

        let memory_id = db.insert_memory(&memory).unwrap();
        let context = db
            .context_metadata("memory", &memory_id.to_string())
            .unwrap()
            .unwrap();

        assert_eq!(context.work_area.as_deref(), Some("sad-sack-signals"));
        assert!(context
            .subject_tags
            .contains(&"forge-signalsmith".to_string()));
    }

    #[test]
    fn embedding_persists_as_f32_blob() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = MemoryRecord {
            id: None,
            title: "Recall files".to_string(),
            body: "Skills read recall markdown.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: Vec::new(),
        };
        let memory_id = db.insert_memory(&memory).unwrap();
        let embedded_text = yaaml_core::embedding_text(&memory);
        let embedding = EmbeddingRecord {
            memory_id,
            embedding_model: "text-embedding-3-small".to_string(),
            dimensions: 3,
            embedding_blob: encode_f32_embedding(&[0.1, 0.2, 0.3]),
            embedded_text_hash: yaaml_core::embedded_text_hash(&embedded_text),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
        };

        db.upsert_embedding(&embedding).unwrap();
        let stored = db.get_embedding(memory_id).unwrap().unwrap();

        assert_eq!(stored.dimensions, 3);
        assert_eq!(
            decode_f32_embedding(&stored.embedding_blob).unwrap(),
            vec![0.1, 0.2, 0.3]
        );
        assert_eq!(stored.embedded_text_hash, embedding.embedded_text_hash);
    }

    #[test]
    fn consolidation_creates_new_memory_and_marks_sources_inactive() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let source = |title: &str| MemoryRecord {
            id: None,
            title: title.to_string(),
            body: title.to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: Vec::new(),
        };
        let first = db.insert_memory(&source("first")).unwrap();
        let second = db.insert_memory(&source("second")).unwrap();
        let consolidated = MemoryRecord {
            id: None,
            title: "merged".to_string(),
            body: "merged body".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: vec![first, second],
        };

        let merged_id = db
            .consolidate_memories(&[first, second], &consolidated, "2026-06-08T00:00:01Z")
            .unwrap();
        let memories = db.list_memories().unwrap();

        assert_eq!(memories.iter().filter(|memory| memory.is_active).count(), 1);
        let merged = memories
            .iter()
            .find(|memory| memory.id == Some(merged_id))
            .unwrap();
        assert_eq!(merged.lineage_refs, vec![first, second]);
    }

    #[test]
    fn mock_judge_scores_persist_for_eval_run() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let run_id = db
            .insert_eval_run("default", "2026-06-08T00:00:00Z", "{}")
            .unwrap();

        db.insert_eval_result(
            run_id,
            1,
            Some(10),
            "useful",
            "helped",
            "2026-06-08T00:00:01Z",
        )
        .unwrap();
        db.insert_eval_result(
            run_id,
            2,
            Some(11),
            "neutral",
            "unused",
            "2026-06-08T00:00:02Z",
        )
        .unwrap();
        db.insert_eval_result(
            run_id,
            3,
            Some(12),
            "distracting",
            "wrong context",
            "2026-06-08T00:00:03Z",
        )
        .unwrap();
        db.complete_eval_run(run_id, "2026-06-08T00:00:04Z")
            .unwrap();

        assert_eq!(
            db.eval_scores_for_run(run_id).unwrap(),
            vec!["useful", "neutral", "distracting"]
        );
        let runs = db.list_eval_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].result_count, 3);
        assert_eq!(
            runs[0].completed_at.as_deref(),
            Some("2026-06-08T00:00:04Z")
        );

        let run = db.eval_run_by_id(run_id).unwrap().unwrap();
        assert_eq!(run.result_count, 3);
        assert!(db.eval_run_by_id(run_id + 1).unwrap().is_none());

        let results = db.eval_results_for_run(run_id).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].judge_score.as_deref(), Some("useful"));
        assert_eq!(results[0].rationale.as_deref(), Some("helped"));
    }
}
