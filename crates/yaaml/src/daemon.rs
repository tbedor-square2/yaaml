use std::collections::HashSet;
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
    apply_project_bonus, build_recall_query, derive_project_descriptor, embedded_text_hash,
    embedding_text, parse_formulation_response, recall_file_path, render_recall_markdown,
    write_recall_file, Config, EmbeddingRecord, RecallCandidate, RecallMemory, SourceTurnRef,
    TaskRecord, TaskStatus, VectorIndex,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::ReqwestTransport;
use yaaml_store::{Database, SqliteExactVectorIndex};
use yaaml_transcript::codex::parse_codex_file_from_offset_with_session;
use yaaml_transcript::discovery::discover_codex_backlog;

pub const TASK_KIND_MEMORY_FORMULATION: &str = "memory_formulation";
pub const TASK_KIND_RECALL: &str = "recall";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialBatchPolicy {
    Include,
    IfSessionIdle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted_turns: u64,
    pub next_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogIngestReport {
    pub discovered_files: u64,
    pub processed_files: u64,
    pub processed_turns: u64,
    pub queued_memory_jobs: u64,
    pub failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeIngestReport {
    pub scanned_files: u64,
    pub changed_files: u64,
    pub processed_turns: u64,
    pub queued_memory_jobs: u64,
    pub failures: u64,
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
    let start_offset = if offset > 0 && fallback_session.is_none() {
        0
    } else {
        offset
    };
    let parsed =
        parse_codex_file_from_offset_with_session(transcript_path, start_offset, fallback_session)
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

pub fn process_codex_backlog(
    db: &Database,
    config: &Config,
    sessions_root: &Path,
) -> anyhow::Result<BacklogIngestReport> {
    let already_cursored = db
        .cursor_paths()
        .context("failed to load transcript cursors")?
        .into_iter()
        .collect::<HashSet<_>>();
    let files = discover_codex_backlog(sessions_root, &already_cursored)
        .context("failed to discover Codex backlog")?;
    let mut report = BacklogIngestReport {
        discovered_files: files.len() as u64,
        processed_files: 0,
        processed_turns: 0,
        queued_memory_jobs: 0,
        failures: 0,
    };
    if report.discovered_files > 0 {
        db.add_backlog_progress(report.discovered_files, 0, 0, 0, 0, Some(&unix_timestamp()))
            .context("failed to record backlog discovery")?;
    }

    for file in files {
        match ingest_codex_file(db, &file.path) {
            Ok(ingested) => {
                report.processed_files += 1;
                report.processed_turns += ingested.inserted_turns;
                db.add_backlog_progress(
                    0,
                    1,
                    ingested.inserted_turns,
                    0,
                    0,
                    Some(&unix_timestamp()),
                )
                .context("failed to record backlog progress")?;
            }
            Err(_) => {
                report.failures += 1;
                db.add_backlog_progress(0, 0, 0, 0, 1, Some(&unix_timestamp()))
                    .context("failed to record backlog failure")?;
            }
        }
    }
    let queued_memory_jobs =
        queue_missing_memory_formulation_tasks(db, config, 0, PartialBatchPolicy::Include)
            .context("failed to queue backlog memory formulation tasks")?;
    if queued_memory_jobs > 0 {
        report.queued_memory_jobs += queued_memory_jobs;
        db.add_backlog_progress(0, 0, 0, queued_memory_jobs, 0, Some(&unix_timestamp()))
            .context("failed to record queued backlog memory tasks")?;
    }

    Ok(report)
}

pub fn process_codex_changes(
    db: &Database,
    config: &Config,
    sessions_root: &Path,
) -> anyhow::Result<ChangeIngestReport> {
    let files = discover_codex_backlog(sessions_root, &HashSet::new())
        .context("failed to discover Codex transcript changes")?;
    let mut report = ChangeIngestReport {
        scanned_files: files.len() as u64,
        changed_files: 0,
        processed_turns: 0,
        queued_memory_jobs: 0,
        failures: 0,
    };

    for file in files {
        let previous_offset = db
            .get_cursor(&file.path.display().to_string())
            .context("failed to read transcript cursor")?;
        match ingest_codex_file(db, &file.path) {
            Ok(ingested) => {
                if ingested.next_offset > previous_offset || ingested.inserted_turns > 0 {
                    report.changed_files += 1;
                    report.processed_turns += ingested.inserted_turns;
                    db.add_backlog_progress(
                        0,
                        1,
                        ingested.inserted_turns,
                        0,
                        0,
                        Some(&unix_timestamp()),
                    )
                    .context("failed to record Codex change progress")?;
                }
            }
            Err(_) => {
                report.failures += 1;
                db.add_backlog_progress(0, 0, 0, 0, 1, Some(&unix_timestamp()))
                    .context("failed to record Codex change failure")?;
            }
        }
    }
    let queued_memory_jobs =
        queue_missing_memory_formulation_tasks(db, config, 10, PartialBatchPolicy::IfSessionIdle)
            .context("failed to queue Codex change memory formulation tasks")?;
    if queued_memory_jobs > 0 {
        report.queued_memory_jobs += queued_memory_jobs;
        db.add_backlog_progress(0, 0, 0, queued_memory_jobs, 0, Some(&unix_timestamp()))
            .context("failed to record queued Codex change memory tasks")?;
    }

    Ok(report)
}

pub fn run_queued_tasks(db: &Database, config: &Config, limit: usize) -> anyhow::Result<usize> {
    let mut completed = 0;
    for _ in 0..limit {
        let Some(task) = db
            .next_queued_task()
            .context("failed to fetch queued task")?
        else {
            break;
        };
        let task_id = task.id.context("queued task missing id")?;
        db.mark_task_running(task_id, &unix_timestamp())
            .context("failed to mark task running")?;
        let result = match task.kind.as_str() {
            TASK_KIND_MEMORY_FORMULATION => run_memory_formulation_task(db, config, &task),
            _ => Ok(()),
        };
        match result {
            Ok(()) => {
                db.complete_task(task_id, &unix_timestamp())
                    .context("failed to complete task")?;
                completed += 1;
            }
            Err(error) => {
                db.park_task(
                    task_id,
                    &format_error_chain(error.as_ref()),
                    &unix_timestamp(),
                )
                .context("failed to park task")?;
            }
        }
    }
    Ok(completed)
}

fn format_error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

fn run_memory_formulation_task(
    db: &Database,
    config: &Config,
    task: &TaskRecord,
) -> anyhow::Result<()> {
    let payload: serde_json::Value =
        serde_json::from_str(&task.payload_json).context("failed to parse task payload")?;
    let session_id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .context("memory formulation task missing session_id")?;
    let session = db
        .session_by_id(session_id)
        .context("failed to load task session")?
        .context("memory formulation task references missing session")?;
    let requested_source_turn_refs = source_turn_refs_from_payload(&payload)?;
    let turns = if requested_source_turn_refs.is_empty() {
        db.turns_for_session(session_id, config.turns_between_memory as usize)
            .context("failed to load task turns")?
    } else {
        db.completed_turns_for_source_refs(&requested_source_turn_refs)
            .context("failed to load task source turns")?
    };
    if turns.is_empty() {
        return Ok(());
    }
    let source_turn_refs = turns
        .iter()
        .map(|turn| SourceTurnRef {
            session_id: turn.session_id.clone(),
            ordinal: turn.ordinal,
            byte_start: turn.byte_start,
            byte_end: turn.byte_end,
        })
        .collect::<Vec<_>>();
    let project_descriptor = derive_project_descriptor(Path::new(&session.project_id), None);
    let prompt = formulation_prompt(&project_descriptor, &turns);
    let summary_client = AnthropicMessageClient::new(
        AnthropicMessageConfig::summary_from_config(config),
        ReqwestTransport::default(),
    );
    let value = summary_client
        .structured_json(formulation_system_prompt(), &prompt)
        .context("failed to formulate memory")?;
    let drafts = parse_formulation_response(&value, &project_descriptor, config.max_memory_length)
        .context("failed to parse memory formulation")?;
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let now = unix_timestamp();
    for draft in drafts {
        let memory = draft.into_record(
            source_turn_refs.clone(),
            now.clone(),
            Some(session.id.clone()),
            Some(session.project_id.clone()),
        );
        let text = embedding_text(&memory);
        let vector = embedding_client
            .embed(&text)
            .context("failed to embed memory")?;
        let memory_id = db
            .insert_memory(&memory)
            .context("failed to insert memory")?;
        db.upsert_embedding(&EmbeddingRecord {
            memory_id,
            embedding_model: config.embedding_model.clone(),
            dimensions: vector.len() as u64,
            embedding_blob: yaaml_store::database::encode_f32_embedding(&vector),
            embedded_text_hash: embedded_text_hash(&text),
            updated_at: now.clone(),
        })
        .context("failed to persist embedding")?;
    }
    Ok(())
}

fn formulation_system_prompt() -> &'static str {
    "Create concise durable memories from coding-agent transcript turns. Return only JSON shaped as {\"memories\":[{\"title\":\"...\",\"body\":\"...\",\"scope\":\"project\"|\"global\",\"project_descriptor\":\"...\"}]}. Prefer small, granular memories. Use global scope only for durable cross-project user preferences or agent workflow patterns."
}

fn formulation_prompt(project_descriptor: &str, turns: &[yaaml_core::TurnRecord]) -> String {
    let mut prompt = format!("Project descriptor: {project_descriptor}\n\nTurns:\n");
    for turn in turns {
        prompt.push_str(&format!(
            "\nTurn {}:\n{}\n",
            turn.ordinal,
            turn.display_text.as_deref().unwrap_or("")
        ));
    }
    prompt
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
    let turns = db
        .turns_for_session(session_id, config.turns_between_memory as usize)
        .context("failed to load formulation turns")?;
    if turns.is_empty() {
        return Ok(None);
    }
    enqueue_memory_formulation_task(db, session_id, &turns, priority)
}

pub fn queue_missing_memory_formulation_tasks(
    db: &Database,
    config: &Config,
    priority: i64,
    partial_policy: PartialBatchPolicy,
) -> anyhow::Result<u64> {
    let window = config.backlog_formulation_turn_window.max(1);
    let covered = covered_memory_turn_refs(db).context("failed to load covered memory refs")?;
    let mut queued = 0;
    for (session, _completed_turns) in db
        .sessions_with_completed_turn_counts()
        .context("failed to load sessions with turns")?
    {
        let turns = db
            .completed_turns_for_session_range(&session.id, 0, u64::MAX)
            .context("failed to load session turns")?;
        let include_partial = match partial_policy {
            PartialBatchPolicy::Include => true,
            PartialBatchPolicy::IfSessionIdle => session_is_idle(&session, config),
        };
        for chunk in turns.chunks(window) {
            if chunk.len() < window && !include_partial {
                continue;
            }
            if chunk
                .iter()
                .all(|turn| covered.contains(&(turn.session_id.clone(), turn.ordinal)))
            {
                continue;
            }
            if enqueue_memory_formulation_task(db, &session.id, chunk, priority)
                .context("failed to enqueue missing formulation task")?
                .is_some()
            {
                queued += 1;
            }
        }
    }
    Ok(queued)
}

fn enqueue_memory_formulation_task(
    db: &Database,
    session_id: &str,
    turns: &[yaaml_core::TurnRecord],
    priority: i64,
) -> anyhow::Result<Option<i64>> {
    let source_turn_refs = source_turn_refs_for_turns(turns);
    if source_turn_refs.is_empty() {
        return Ok(None);
    }
    let start_ordinal = source_turn_refs
        .first()
        .map(|source_ref| source_ref.ordinal);
    let end_ordinal = source_turn_refs
        .last()
        .map(|source_ref| source_ref.ordinal.saturating_add(1));
    let payload = json!({
        "session_id": session_id,
        "start_ordinal": start_ordinal,
        "end_ordinal": end_ordinal,
        "source_turn_refs": source_turn_refs,
    });
    let payload_json = payload.to_string();
    if db
        .task_payload_exists(TASK_KIND_MEMORY_FORMULATION, &payload_json)
        .context("failed to check existing formulation task")?
    {
        return Ok(None);
    }
    let now = unix_timestamp();
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_MEMORY_FORMULATION.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json,
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

fn source_turn_refs_from_payload(
    payload: &serde_json::Value,
) -> anyhow::Result<Vec<SourceTurnRef>> {
    match payload.get("source_turn_refs") {
        Some(value) => serde_json::from_value(value.clone())
            .context("failed to parse formulation source_turn_refs"),
        None => Ok(Vec::new()),
    }
}

fn source_turn_refs_for_turns(turns: &[yaaml_core::TurnRecord]) -> Vec<SourceTurnRef> {
    turns
        .iter()
        .map(|turn| SourceTurnRef {
            session_id: turn.session_id.clone(),
            ordinal: turn.ordinal,
            byte_start: turn.byte_start,
            byte_end: turn.byte_end,
        })
        .collect()
}

fn covered_memory_turn_refs(db: &Database) -> anyhow::Result<HashSet<(String, u64)>> {
    let mut covered = HashSet::new();
    for memory in db.list_memories().context("failed to list memories")? {
        for source_ref in memory.source_turn_refs {
            covered.insert((source_ref.session_id, source_ref.ordinal));
        }
    }
    Ok(covered)
}

fn session_is_idle(session: &yaaml_core::SessionRecord, config: &Config) -> bool {
    let Some(last_seen_at) = session.last_seen_at.as_deref() else {
        return true;
    };
    let Some(last_seen_seconds) = timestamp_seconds(last_seen_at) else {
        return false;
    };
    unix_timestamp_seconds().saturating_sub(last_seen_seconds) as u64
        >= config.session_idle_memory_seconds
}

fn timestamp_seconds(timestamp: &str) -> Option<i64> {
    if let Some(value) = timestamp.strip_prefix("unix:") {
        return value.parse().ok();
    }
    let timestamp = timestamp.strip_suffix('Z')?;
    let (date, time) = timestamp.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second_part = time_parts.next()?;
    let second_text = second_part.split('.').next().unwrap_or(second_part);
    let second: u32 = second_text.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day = day as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some((era * 146_097 + day_of_era - 719_468) as i64)
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
    let seconds = unix_timestamp_seconds();
    format!("unix:{seconds}")
}

fn unix_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64
}
