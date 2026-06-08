use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;
use yaaml_core::status::{BacklogStatus, Status, TaskFailure, WorkerStatus};
use yaaml_core::{
    EmbeddingRecord, MemoryRecord, MemoryScope, SessionRecord, SourceTurnRef, TaskRecord,
    TaskStatus, TurnRecord,
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
        Ok(Self { conn, path })
    }

    pub fn in_memory() -> Result<Self, DatabaseError> {
        Ok(Self {
            conn: Connection::open_in_memory()?,
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn migrate(&mut self) -> Result<(), DatabaseError> {
        let tx = self.conn.transaction()?;
        for migration in MIGRATIONS {
            tx.execute_batch(migration)?;
        }
        tx.commit()?;
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
        let queued_jobs = self.count("SELECT COUNT(*) FROM tasks WHERE status = 'queued'")?;
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
        Ok(())
    }

    pub fn insert_turn(&self, turn: &TurnRecord) -> Result<bool, DatabaseError> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO turns (
                session_id, turn_id, ordinal, byte_start, byte_end, observed_at, status, display_text
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                turn.session_id,
                turn.turn_id,
                u64_to_i64(turn.ordinal),
                u64_to_i64(turn.byte_start),
                u64_to_i64(turn.byte_end),
                turn.observed_at,
                turn.status.as_str(),
                turn.display_text
            ],
        )?;
        Ok(inserted > 0)
    }

    pub fn insert_memory(&self, memory: &MemoryRecord) -> Result<i64, DatabaseError> {
        let source_turn_refs = serde_json::to_string(&memory.source_turn_refs)?;
        let lineage_refs = serde_json::to_string(&memory.lineage_refs)?;
        self.conn.execute(
            "INSERT INTO memories (
                title, body, scope, source_turn_refs, created_at, updated_at, is_active,
                session_id, project_id, project_descriptor, lineage_refs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                memory.title,
                memory.body,
                memory.scope.as_str(),
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
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_memories(&self) -> Result<Vec<MemoryRecord>, DatabaseError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, scope, source_turn_refs, created_at, updated_at,
                    is_active, session_id, project_id, project_descriptor, lineage_refs
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
                    "SELECT id, title, body, scope, source_turn_refs, created_at, updated_at,
                            is_active, session_id, project_id, project_descriptor, lineage_refs
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

    pub fn next_queued_task(&self) -> Result<Option<TaskRecord>, DatabaseError> {
        self.conn
            .query_row(
                "SELECT id, kind, status, priority, payload_json, attempts, max_attempts,
                        next_run_at, last_error, created_at, updated_at
                 FROM tasks
                 WHERE status = 'queued'
                 ORDER BY priority DESC, id ASC
                 LIMIT 1",
                [],
                read_task_record,
            )
            .optional()
            .map_err(DatabaseError::from)
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

    fn count(&self, sql: &str) -> Result<u64, DatabaseError> {
        let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
        Ok(count.try_into().unwrap_or(0))
    }

    fn backlog_status(&self) -> Result<BacklogStatus, DatabaseError> {
        Ok(self.conn.query_row(
            "SELECT discovered_files, processed_files, processed_turns, queued_memory_jobs, failures, last_activity_at
             FROM backlog_progress WHERE id = 1",
            [],
            |row| {
                Ok(BacklogStatus {
                    discovered_files: i64_to_u64(row.get(0)?),
                    processed_files: i64_to_u64(row.get(1)?),
                    processed_turns: i64_to_u64(row.get(2)?),
                    queued_memory_jobs: i64_to_u64(row.get(3)?),
                    failures: i64_to_u64(row.get(4)?),
                    last_activity_at: row.get(5)?,
                })
            },
        )?)
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

fn i64_to_u64(value: i64) -> u64 {
    value.try_into().unwrap_or(0)
}

fn u64_to_i64(value: u64) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
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

fn read_memory_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let scope: String = row.get(3)?;
    let source_turn_refs_json: String = row.get(4)?;
    let lineage_refs_json: String = row.get(11)?;
    let source_turn_refs: Vec<SourceTurnRef> =
        serde_json::from_str(&source_turn_refs_json).map_err(json_decode_error)?;
    let lineage_refs: Vec<i64> =
        serde_json::from_str(&lineage_refs_json).map_err(json_decode_error)?;
    let is_active: i64 = row.get(7)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        scope: match scope.as_str() {
            "global" => MemoryScope::Global,
            _ => MemoryScope::Project,
        },
        source_turn_refs,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        is_active: is_active != 0,
        session_id: row.get(8)?,
        project_id: row.get(9)?,
        project_descriptor: row.get(10)?,
        lineage_refs,
    })
}

fn json_decode_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

pub fn encode_f32_embedding(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(values.len() * std::mem::size_of::<f32>());
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

pub fn decode_f32_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % std::mem::size_of::<f32>() != 0 {
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
        };

        assert!(db.insert_turn(&turn).unwrap());
        assert!(!db.insert_turn(&turn).unwrap());

        db.update_cursor("/tmp/session.jsonl", 100, Some("2026-06-08T00:01:00Z"))
            .unwrap();
        assert_eq!(db.get_cursor("/tmp/session.jsonl").unwrap(), 100);
        assert!(db.cursor_exists("/tmp/session.jsonl").unwrap());
        assert!(!db.cursor_exists("/tmp/other.jsonl").unwrap());
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

        let next = db.next_queued_task().unwrap().unwrap();

        assert_eq!(next.kind, "live");
        assert_eq!(next.priority, 10);
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
    fn embedding_persists_as_f32_blob() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = MemoryRecord {
            id: None,
            title: "Recall files".to_string(),
            body: "Skills read recall markdown.".to_string(),
            scope: MemoryScope::Project,
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
}
