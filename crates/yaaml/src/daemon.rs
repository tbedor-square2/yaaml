use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use yaaml_core::{
    apply_project_bonus, build_recall_query, recall_file_path, render_recall_markdown,
    write_recall_file, Config, RecallCandidate, RecallMemory, SourceTurnRef, TaskRecord,
    TaskStatus, VectorIndex,
};
use yaaml_store::{Database, SqliteExactVectorIndex};
use yaaml_transcript::codex::parse_codex_file_from_offset_with_session;

pub const TASK_KIND_MEMORY_FORMULATION: &str = "memory_formulation";
pub const TASK_KIND_RECALL: &str = "recall";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted_turns: u64,
    pub next_offset: u64,
}

#[derive(Debug, Clone)]
pub struct DaemonShutdown {
    requested: Arc<AtomicBool>,
}

impl Default for DaemonShutdown {
    fn default() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DaemonShutdown {
    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

pub fn ingest_codex_file(db: &Database, transcript_path: &Path) -> anyhow::Result<IngestReport> {
    let transcript_path_string = transcript_path.display().to_string();
    let offset = db
        .get_cursor(&transcript_path_string)
        .context("failed to read transcript cursor")?;
    let fallback_session = if offset > 0 {
        db.session_by_transcript_path(&transcript_path_string)
            .context("failed to read persisted session")?
    } else {
        None
    };
    let parsed =
        parse_codex_file_from_offset_with_session(transcript_path, offset, fallback_session)
            .context("failed to parse Codex transcript")?;
    db.upsert_session(&parsed.session)
        .context("failed to persist Codex session")?;
    let mut inserted_turns = 0;
    for turn in &parsed.turns {
        if db
            .insert_turn(turn)
            .context("failed to persist Codex turn")?
        {
            inserted_turns += 1;
        }
    }
    let last_observed_at = parsed
        .turns
        .last()
        .and_then(|turn| turn.observed_at.as_deref());
    db.update_cursor(
        &transcript_path_string,
        parsed.next_offset,
        last_observed_at,
    )
    .context("failed to update transcript cursor")?;

    Ok(IngestReport {
        session_id: parsed.session.id,
        inserted_turns,
        next_offset: parsed.next_offset,
    })
}

pub fn queue_memory_formulation_if_due(
    db: &Database,
    config: &Config,
    session_id: &str,
    priority: i64,
) -> anyhow::Result<Option<i64>> {
    let completed = db
        .completed_turn_count_for_session(session_id)
        .context("failed to count completed turns")?;
    if completed < config.turns_between_memory {
        return Ok(None);
    }
    if db
        .count_tasks_by_status(TASK_KIND_MEMORY_FORMULATION, TaskStatus::Queued)
        .context("failed to count queued formulation tasks")?
        > 0
    {
        return Ok(None);
    }
    let turns = db
        .turns_for_session(session_id, config.turns_between_memory as usize)
        .context("failed to load formulation turns")?;
    let source_turn_refs = turns
        .iter()
        .map(|turn| SourceTurnRef {
            session_id: turn.session_id.clone(),
            ordinal: turn.ordinal,
            byte_start: turn.byte_start,
            byte_end: turn.byte_end,
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "session_id": session_id,
        "source_turn_refs": source_turn_refs,
    });
    let now = unix_timestamp();
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_MEMORY_FORMULATION.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json: payload.to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    db.enqueue_task(&task)
        .map(Some)
        .context("failed to enqueue formulation task")
}

pub fn queue_recall_after_turn(
    db: &Database,
    session_id: &str,
    turn_ordinal: u64,
    priority: i64,
) -> anyhow::Result<i64> {
    let payload = json!({
        "session_id": session_id,
        "turn_ordinal": turn_ordinal,
    });
    let now = unix_timestamp();
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json: payload.to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    db.enqueue_task(&task)
        .context("failed to enqueue recall task")
}

pub fn refresh_recall_with_embedding(
    db: &Database,
    config: &Config,
    project_id: &Path,
    recent_turns: &[yaaml_core::TurnRecord],
    query_embedding: &[f32],
    query_source: &str,
) -> anyhow::Result<yaaml_core::RecallWrite> {
    let now = unix_timestamp();
    let index = SqliteExactVectorIndex::new(db, config.embedding_model.clone(), now.clone());
    let hits = index
        .search(
            query_embedding,
            config.recall_candidate_pool,
            config.recall_similarity_threshold,
        )
        .context("failed to search vector index")?;
    let hit_ids = hits.iter().map(|hit| hit.memory_id).collect::<Vec<_>>();
    let memories = db
        .list_active_memories_by_ids(&hit_ids)
        .context("failed to load active memories")?;
    let project_id_string = project_id.display().to_string();
    let candidates = hits
        .iter()
        .filter_map(|hit| {
            memories
                .iter()
                .find(|memory| memory.id == Some(hit.memory_id))
                .map(|memory| RecallCandidate {
                    memory_id: hit.memory_id,
                    similarity: hit.similarity,
                    score: hit.similarity,
                    project_id: memory.project_id.clone(),
                })
        })
        .collect::<Vec<_>>();
    let candidates = if config.recall_project_tiebreaker {
        apply_project_bonus(
            candidates,
            &project_id_string,
            config.recall_project_score_bonus,
        )
    } else {
        candidates
    };
    let selected = candidates
        .into_iter()
        .take(config.recall_result_limit)
        .collect::<Vec<_>>();
    let selected_ids = selected
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<Vec<_>>();
    let selected_memories = db
        .list_active_memories_by_ids(&selected_ids)
        .context("failed to load selected memories")?;
    let recall_memories = selected
        .iter()
        .filter_map(|candidate| {
            selected_memories
                .iter()
                .find(|memory| memory.id == Some(candidate.memory_id))
                .map(|memory| RecallMemory {
                    memory_id: candidate.memory_id,
                    title: memory.title.clone(),
                    body: memory.body.clone(),
                    created_at: memory.created_at.clone(),
                    project_id: memory.project_id.clone(),
                    project_descriptor: memory.project_descriptor.clone(),
                    score: candidate.score,
                })
        })
        .collect::<Vec<_>>();
    let query_text = build_recall_query(
        recent_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    let source = if query_text.is_empty() {
        query_source.to_string()
    } else {
        format!("{query_source}: {} chars", query_text.len())
    };
    let rendered = render_recall_markdown(&now, &source, &project_id_string, &recall_memories);
    let path = recall_file_path(&config.recall_dir()?, project_id);
    write_recall_file(&path, &rendered, &selected_ids).context("failed to write recall file")
}

pub fn recover_running_tasks(db: &Database) -> anyhow::Result<u64> {
    db.requeue_running_tasks(&unix_timestamp())
        .context("failed to requeue running tasks")
}

pub fn watch_codex_sessions(
    root: &Path,
    sender: mpsc::Sender<PathBuf>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                    let _ = sender.send(path);
                }
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

pub fn start_signal_socket(
    socket_path: &Path,
    shutdown: DaemonShutdown,
) -> anyhow::Result<thread::JoinHandle<anyhow::Result<()>>> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).context("failed to create socket directory")?;
    }
    if socket_path.exists() {
        fs::remove_file(socket_path).context("failed to remove stale socket")?;
    }
    let listener = UnixListener::bind(socket_path).context("failed to bind signal socket")?;
    listener
        .set_nonblocking(true)
        .context("failed to make signal socket nonblocking")?;
    let socket_path = socket_path.to_path_buf();
    let handle = thread::spawn(move || {
        while !shutdown.is_requested() {
            match listener.accept() {
                Ok((mut stream, _)) => handle_signal_stream(&mut stream, &shutdown)?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error).context("failed to accept signal connection"),
            }
        }
        let _ = fs::remove_file(socket_path);
        Ok(())
    });
    Ok(handle)
}

fn handle_signal_stream(stream: &mut UnixStream, shutdown: &DaemonShutdown) -> anyhow::Result<()> {
    let mut buffer = [0_u8; 128];
    let bytes = stream
        .read(&mut buffer)
        .context("failed to read signal command")?;
    let command = String::from_utf8_lossy(&buffer[..bytes]);
    if command.trim() == "shutdown" {
        shutdown.request();
        stream
            .write_all(b"ok\n")
            .context("failed to write signal response")?;
    } else {
        stream
            .write_all(b"unknown\n")
            .context("failed to write signal response")?;
    }
    Ok(())
}

fn unix_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix:{seconds}")
}
