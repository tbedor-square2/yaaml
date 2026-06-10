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
    apply_project_bonus, build_recall_query, context_score, derive_project_descriptor,
    embedded_text_hash, embedding_text, infer_context_from_memory, infer_context_from_path,
    infer_context_from_text, merge_contexts, parse_eval_judge_response, parse_formulation_response,
    recall_file_path, render_recall_markdown, session_recall_file_path, write_recall_file, Config,
    EmbeddingRecord, RecallCandidate, RecallMemory, SourceTurnRef, TaskRecord, TaskStatus,
    VectorIndex,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::{ProviderError, ReqwestTransport};
use yaaml_store::{Database, SqliteExactVectorIndex};
use yaaml_transcript::codex::parse_codex_file_from_offset_with_session;
use yaaml_transcript::discovery::discover_codex_backlog;

pub const TASK_KIND_MEMORY_FORMULATION: &str = "memory_formulation";
pub const TASK_KIND_RECALL: &str = "recall";
pub const TASK_KIND_RECALL_EVAL: &str = "recall_eval";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialBatchPolicy {
    Include,
    IfSessionIdle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub session_id: String,
    pub inserted_turns: u64,
    pub last_inserted_ordinal: Option<u64>,
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
    let ordinal_base = if start_offset > 0 {
        db.next_turn_ordinal_for_session(&parsed.session.id)
            .context("failed to read next Codex turn ordinal")?
    } else {
        0
    };
    db.upsert_session(&parsed.session)
        .context("failed to persist Codex session")?;
    let mut inserted_turns = 0;
    let mut last_inserted_ordinal = None;
    for turn in &parsed.turns {
        let mut turn = turn.clone();
        turn.ordinal += ordinal_base;
        if db
            .insert_turn(&turn)
            .context("failed to persist Codex turn")?
        {
            inserted_turns += 1;
            last_inserted_ordinal = Some(turn.ordinal);
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
        last_inserted_ordinal,
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
                    if let Some(turn_ordinal) = ingested.last_inserted_ordinal {
                        queue_recall_after_turn(db, &ingested.session_id, turn_ordinal, 20)
                            .context("failed to queue Codex recall refresh")?;
                    }
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
            .next_queued_task(unix_timestamp_seconds())
            .context("failed to fetch queued task")?
        else {
            break;
        };
        let task_id = task.id.context("queued task missing id")?;
        db.mark_task_running(task_id, &unix_timestamp())
            .context("failed to mark task running")?;
        let result = match task.kind.as_str() {
            TASK_KIND_MEMORY_FORMULATION => run_memory_formulation_task(db, config, &task),
            TASK_KIND_RECALL => run_recall_task(db, config, &task),
            TASK_KIND_RECALL_EVAL => run_recall_eval_task(db, config, &task),
            _ => Ok(()),
        };
        match result {
            Ok(()) => {
                if task.kind == TASK_KIND_MEMORY_FORMULATION {
                    dedupe_active_memories(db, &unix_timestamp())
                        .context("failed to dedupe active memories")?;
                }
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
    let prompt = formulation_prompt(config, &project_descriptor, &turns);
    let summary_client = AnthropicMessageClient::new(
        AnthropicMessageConfig::summary_from_config(config),
        ReqwestTransport::default(),
    );
    let value = match summary_client.structured_json(formulation_system_prompt(), &prompt) {
        Ok(value) => value,
        Err(ProviderError::Parse(_)) => json!({"memories":[]}),
        Err(error) => return Err(error).context("failed to formulate memory"),
    };
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

fn run_recall_task(db: &Database, config: &Config, task: &TaskRecord) -> anyhow::Result<()> {
    let payload: serde_json::Value =
        serde_json::from_str(&task.payload_json).context("failed to parse recall payload")?;
    let session_id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .context("recall task missing session_id")?;
    let turn_ordinal = payload
        .get("turn_ordinal")
        .and_then(serde_json::Value::as_u64)
        .context("recall task missing turn_ordinal")?;
    let session = db
        .session_by_id(session_id)
        .context("failed to load recall task session")?
        .context("recall task references missing session")?;
    let window = u64::try_from(config.recall_live_turn_window).unwrap_or(u64::MAX);
    let start_ordinal = turn_ordinal.saturating_add(1).saturating_sub(window.max(1));
    let recent_turns = db
        .completed_turns_for_session_range(session_id, start_ordinal, turn_ordinal + 1)
        .context("failed to load recall task turns")?;
    if recent_turns.is_empty() {
        return Ok(());
    }
    let query_text = build_recall_query(
        &recent_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    if query_text.trim().is_empty() {
        return Ok(());
    }
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let query_embedding = embedding_client
        .embed(&query_text)
        .context("failed to embed recall task query")?;
    refresh_recall_with_embedding(
        db,
        config,
        Path::new(&session.project_id),
        &recent_turns,
        &query_embedding,
        "background completed turns",
    )
    .context("failed to refresh recall")
    .map(|_| ())
}

fn run_recall_eval_task(db: &Database, config: &Config, task: &TaskRecord) -> anyhow::Result<()> {
    let payload: serde_json::Value =
        serde_json::from_str(&task.payload_json).context("failed to parse recall eval payload")?;
    let session_id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .context("recall eval task missing session_id")?;
    let turn_ordinal = payload
        .get("turn_ordinal")
        .and_then(serde_json::Value::as_u64)
        .context("recall eval task missing turn_ordinal")?;
    let recall_text = payload
        .get("recall_text")
        .and_then(serde_json::Value::as_str)
        .context("recall eval task missing recall_text")?;
    let memory_ids = payload
        .get("memory_ids")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(turn_row_id) = db
        .turn_row_id_for_session_ordinal(session_id, turn_ordinal)
        .context("failed to load recall eval anchor turn")?
    else {
        defer_recall_eval_until_anchor_exists(db, task)
            .context("failed to defer recall eval task")?;
        return Ok(());
    };
    let later_turns = db
        .completed_turns_for_session_after_ordinal(session_id, turn_ordinal, 20)
        .context("failed to load turns after recall")?;
    let now = unix_timestamp();
    let run_id = db
        .insert_eval_run(
            "recall_1_to_5",
            &now,
            &json!({
                "session_id": session_id,
                "turn_ordinal": turn_ordinal,
                "memory_ids": memory_ids,
                "rating_delay_seconds": 600,
                "rubric": "1-5 recall relevance, concision, and actionability",
            })
            .to_string(),
        )
        .context("failed to create recall eval run")?;
    if later_turns.is_empty() {
        db.insert_eval_result(
            run_id,
            turn_row_id,
            memory_ids.first().copied(),
            "insufficient_context",
            "No subsequent completed turns were captured after recall, so recall usefulness cannot be scored.",
            &now,
        )
        .context("failed to insert insufficient-context recall eval result")?;
        db.complete_eval_run(run_id, &unix_timestamp())
            .context("failed to complete recall eval run")?;
        return Ok(());
    }
    let judge_client = AnthropicMessageClient::new(
        AnthropicMessageConfig::judge_from_config(config),
        ReqwestTransport::default(),
    );
    let prompt = recall_eval_prompt(recall_text, &later_turns);
    let outcome = judge_client
        .structured_json(recall_eval_system_prompt(), &prompt)
        .map(|value| parse_eval_judge_response(&value))
        .context("failed to rate recall")?;
    db.insert_eval_result(
        run_id,
        turn_row_id,
        memory_ids.first().copied(),
        &outcome.score,
        &outcome.rationale,
        &now,
    )
    .context("failed to insert recall eval result")?;
    db.complete_eval_run(run_id, &unix_timestamp())
        .context("failed to complete recall eval run")?;
    Ok(())
}

fn defer_recall_eval_until_anchor_exists(db: &Database, task: &TaskRecord) -> anyhow::Result<i64> {
    let next_run_seconds = unix_timestamp_seconds() + 60;
    let now = format!("unix:{}", unix_timestamp_seconds());
    let deferred = TaskRecord {
        id: None,
        kind: task.kind.clone(),
        status: TaskStatus::Queued,
        priority: task.priority,
        payload_json: task.payload_json.clone(),
        attempts: task.attempts.saturating_add(1),
        max_attempts: task.max_attempts,
        next_run_at: Some(format!("unix:{next_run_seconds}")),
        last_error: Some("waiting for recall eval anchor turn".to_string()),
        created_at: task.created_at.clone(),
        updated_at: now,
    };
    db.enqueue_task(&deferred)
        .context("failed to enqueue deferred recall eval task")
}

fn formulation_system_prompt() -> &'static str {
    concat!(
        "Create concise durable memories from coding-agent transcript turns. ",
        "Return only JSON shaped as {\"memories\":[{\"title\":\"...\",\"body\":\"...\",\"scope\":\"project\"|\"global\",\"project_descriptor\":\"...\"}]}. ",
        "Prefer small, granular memories. ",
        "Focus memories on insights gained while solving the problem and on redirection provided by the user. ",
        "Always capture repeated user corrections, preferences, and process guidance as their own concise memories, including coding style preferences such as functional vs imperative style. ",
        "Use project scope when the preference is tied to the current project or language; use global scope only for durable cross-project user preferences or agent workflow patterns."
    )
}

fn recall_eval_system_prompt() -> &'static str {
    concat!(
        "Rate whether recalled context helped an AI coding agent after it was incorporated into the conversation. ",
        "Return only JSON with fields score and rationale. ",
        "score must be a string from \"1\" to \"5\". ",
        "5: recalled context was relevant, concise, and actionable. ",
        "4: recalled context was relevant and concise, but not directly actionable. ",
        "3: recalled context was partially relevant, but also partially irrelevant or overly long. ",
        "2: recalled context had only weak relevance, was stale/misleading, or required substantial filtering before use. ",
        "1: recalled context was not relevant."
    )
}

fn formulation_prompt(
    config: &Config,
    project_descriptor: &str,
    turns: &[yaaml_core::TurnRecord],
) -> String {
    let mut prompt = format!("Project descriptor: {project_descriptor}\n\nTurns:\n");
    let max_prompt_chars = config.max_formulation_tokens.saturating_mul(3).min(80_000);
    for turn in turns {
        if prompt.chars().count() >= max_prompt_chars {
            prompt.push_str("\n[additional turns omitted due to prompt budget]\n");
            break;
        }
        let remaining = max_prompt_chars.saturating_sub(prompt.chars().count());
        let text = formulation_turn_text(
            turn.display_text.as_deref().unwrap_or(""),
            remaining,
            config.tool_call_truncation_chars,
        );
        prompt.push_str(&format!("\nTurn {}:\n{}\n", turn.ordinal, text));
    }
    prompt
}

fn formulation_turn_text(text: &str, max_chars: usize, tool_output_chars: usize) -> String {
    let mut output = String::new();
    for line in text.lines() {
        let line = if line.trim_start().starts_with("tool output:") {
            truncate_chars(line, tool_output_chars)
        } else {
            line.to_string()
        };
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
        if output.chars().count() >= max_chars {
            return truncate_chars(&output, max_chars);
        }
    }
    output
}

fn recall_eval_prompt(recall_text: &str, later_turns: &[yaaml_core::TurnRecord]) -> String {
    let later_text = if later_turns.is_empty() {
        "No subsequent turns were captured after recall.".to_string()
    } else {
        later_turns
            .iter()
            .map(|turn| {
                format!(
                    "Turn {}:\n{}",
                    turn.ordinal,
                    truncate_chars(turn.display_text.as_deref().unwrap_or(""), 2_000)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    format!(
        "Recalled context:\n{}\n\nSubsequent conversation after recall:\n{}\n\nRate the recalled context from 1 to 5 using the rubric.",
        truncate_chars(recall_text, 8_000),
        truncate_chars(&later_text, 12_000)
    )
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(max_chars.saturating_sub(15))
        .collect::<String>();
    truncated.push_str("[truncated]");
    truncated
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

pub fn dedupe_active_memories(db: &Database, updated_at: &str) -> anyhow::Result<u64> {
    let memories = db
        .list_memories()
        .context("failed to list memories for dedupe")?
        .into_iter()
        .filter(|memory| memory.is_active)
        .collect::<Vec<_>>();
    let mut inactive = HashSet::new();
    for left_index in 0..memories.len() {
        let left = &memories[left_index];
        let Some(left_id) = left.id else {
            continue;
        };
        if inactive.contains(&left_id) {
            continue;
        }
        for right in memories.iter().skip(left_index + 1) {
            let Some(right_id) = right.id else {
                continue;
            };
            if inactive.contains(&right_id) || !same_recall_scope(left, right) {
                continue;
            }
            if !memories_are_duplicates(left, right) {
                continue;
            }
            let deactivate_id = if memory_information_score(left) >= memory_information_score(right)
            {
                right_id
            } else {
                left_id
            };
            inactive.insert(deactivate_id);
            if deactivate_id == left_id {
                break;
            }
        }
    }
    for memory_id in &inactive {
        db.deactivate_memory(*memory_id, updated_at)
            .context("failed to deactivate duplicate memory")?;
    }
    Ok(inactive.len() as u64)
}

fn same_recall_scope(left: &yaaml_core::MemoryRecord, right: &yaaml_core::MemoryRecord) -> bool {
    left.scope == right.scope && left.project_id == right.project_id
}

fn memories_are_duplicates(
    left: &yaaml_core::MemoryRecord,
    right: &yaaml_core::MemoryRecord,
) -> bool {
    let title_similarity = token_jaccard(&left.title, &right.title);
    let body_similarity = token_jaccard(&left.body, &right.body);
    let text_similarity = token_jaccard(
        &format!("{} {}", left.title, left.body),
        &format!("{} {}", right.title, right.body),
    );
    (title_similarity >= 0.65 && (body_similarity >= 0.3 || text_similarity >= 0.4))
        || text_similarity >= 0.72
}

fn memory_information_score(memory: &yaaml_core::MemoryRecord) -> usize {
    let token_count = tokens_for_similarity(&format!("{} {}", memory.title, memory.body)).len();
    token_count * 8 + memory.body.chars().count()
}

fn token_jaccard(left: &str, right: &str) -> f32 {
    let left_tokens = tokens_for_similarity(left);
    let right_tokens = tokens_for_similarity(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let intersection = left_tokens.intersection(&right_tokens).count();
    let union = left_tokens.union(&right_tokens).count();
    intersection as f32 / union as f32
}

fn tokens_for_similarity(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() > 1 && !SIMILARITY_STOP_WORDS.contains(&token.as_str()))
        .collect()
}

const SIMILARITY_STOP_WORDS: &[&str] = &[
    "and", "are", "but", "for", "from", "has", "have", "into", "not", "the", "this", "that", "use",
    "uses", "with",
];

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
    let payload_json = payload.to_string();
    if db
        .task_payload_exists(TASK_KIND_RECALL, &payload_json)
        .context("failed to check existing recall task")?
    {
        return Ok(0);
    }
    let now = unix_timestamp();
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL.to_string(),
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
        .context("failed to enqueue recall task")
}

pub fn queue_recall_eval_after_turn(
    db: &Database,
    session_id: &str,
    turn_ordinal: u64,
    recall_text: &str,
    memory_ids: &[i64],
    priority: i64,
) -> anyhow::Result<i64> {
    let now_seconds = unix_timestamp_seconds();
    let payload = json!({
        "session_id": session_id,
        "turn_ordinal": turn_ordinal,
        "recall_text": recall_text,
        "memory_ids": memory_ids,
        "recall_at": format!("unix:{now_seconds}"),
        "eval_after": format!("unix:{}", now_seconds + 600),
    });
    let now = format!("unix:{now_seconds}");
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json: payload.to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: Some(format!("unix:{}", now_seconds + 600)),
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    db.enqueue_task(&task)
        .context("failed to enqueue recall eval task")
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
    let mut query_context = infer_context_from_path(project_id);
    let query_text = build_recall_query(
        recent_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    merge_contexts(&mut query_context, infer_context_from_text(&query_text));
    let mut candidates = hits
        .iter()
        .filter_map(|hit| {
            memories
                .iter()
                .find(|memory| memory.id == Some(hit.memory_id))
                .map(|memory| {
                    let memory_context = infer_context_from_memory(memory);
                    RecallCandidate {
                        memory_id: hit.memory_id,
                        similarity: hit.similarity,
                        score: hit.similarity + context_score(&query_context, &memory_context),
                        project_id: memory.project_id.clone(),
                    }
                })
        })
        .collect::<Vec<_>>();
    candidates = if config.recall_project_tiebreaker {
        apply_project_bonus(
            candidates,
            &project_id_string,
            config.recall_project_score_bonus,
        )
    } else {
        candidates
    };
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
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
    let recall_dir = config.recall_dir()?;
    let path = recent_turns
        .last()
        .map(|turn| session_recall_file_path(&recall_dir, &turn.session_id))
        .unwrap_or_else(|| recall_file_path(&recall_dir, project_id));
    let write = write_recall_file(&path, &rendered, &selected_ids)
        .context("failed to write recall file")?;
    if !selected_ids.is_empty() {
        if let Some(turn) = recent_turns.last() {
            queue_recall_eval_after_turn(
                db,
                &turn.session_id,
                turn.ordinal,
                &rendered,
                &selected_ids,
                0,
            )?;
        }
    }
    Ok(write)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulation_prompt_truncates_large_tool_output() {
        let mut config = Config::default();
        config.max_formulation_tokens = 100;
        config.tool_call_truncation_chars = 40;
        let turns = vec![yaaml_core::TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 0,
            byte_end: 10,
            observed_at: None,
            status: yaaml_core::TurnStatus::Completed,
            display_text: Some(format!("user text\ntool output: {}", "x".repeat(10_000))),
        }];

        let prompt = formulation_prompt(&config, "yaaml", &turns);

        assert!(prompt.chars().count() <= 360);
        assert!(prompt.contains("[truncated]"));
    }

    #[test]
    fn formulation_system_prompt_mentions_user_preferences() {
        let prompt = formulation_system_prompt();

        assert!(prompt.contains("insights gained while solving the problem"));
        assert!(prompt.contains("redirection provided by the user"));
        assert!(prompt.contains("repeated user corrections"));
        assert!(prompt.contains("coding style preferences"));
        assert!(prompt.contains("functional vs imperative"));
    }

    #[test]
    fn recall_eval_system_prompt_uses_one_to_five_rubric() {
        let prompt = recall_eval_system_prompt();

        assert!(prompt.contains("\"1\" to \"5\""));
        assert!(prompt.contains("relevant, concise, and actionable"));
        assert!(prompt.contains("weak relevance"));
        assert!(prompt.contains("not relevant"));
    }
}
