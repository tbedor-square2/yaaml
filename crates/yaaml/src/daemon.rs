use std::collections::{HashMap, HashSet};
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
use serde::{Deserialize, Serialize};
use serde_json::json;
use yaaml_core::{
    active_segment_recall_turns, build_active_segment_recall_query, build_conversation_segments,
    build_recall_query, derive_project_descriptor, embedded_text_hash, embedding_text,
    find_consolidation_clusters, merge_task_keys, parse_eval_judge_response,
    parse_formulation_response, rank_recall_candidates, recall_file_path, render_recall_markdown,
    segment_task_keys, session_recall_file_path, write_recall_file, ClusterMemory, Config,
    ConversationSegmentStatus, EmbeddingRecord, MemoryKind, MemoryRecord, MemoryScope,
    MemoryValidity, RecallMemory, RecallRankingOptions, SourceTurnRef, TaskRecord, TaskStatus,
    TurnRecord, VectorIndex,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::{ProviderError, ReqwestTransport};
use yaaml_store::database::EvalRunMetadata;
use yaaml_store::{Database, SqliteExactVectorIndex};
use yaaml_transcript::codex::parse_codex_file_from_offset_with_session;
use yaaml_transcript::discovery::discover_codex_backlog;

use crate::llm_judge::JudgeClient;
use crate::memory_health::{apply_health_action_rerank, build_memory_health_summaries};
use crate::recall_filter::{
    select_recall_candidates_with_llm_filter, suppress_recently_recalled_candidates,
    suppress_source_overlapping_candidates, RecallFilterRequest, RecallFilterTelemetry,
};
use crate::turn_hydration::{context_from_turns, hydrate_turns};

pub const TASK_KIND_MEMORY_FORMULATION: &str = "memory_formulation";
pub const TASK_KIND_MEMORY_CONSOLIDATION: &str = "memory_consolidation";
pub const TASK_KIND_RECALL: &str = "recall";
pub const TASK_KIND_RECALL_EVAL: &str = "recall_eval";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryFormulationTaskPayload {
    session_id: String,
    #[serde(default)]
    start_ordinal: Option<u64>,
    #[serde(default)]
    end_ordinal: Option<u64>,
    #[serde(default)]
    source_turn_refs: Vec<SourceTurnRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryConsolidationTaskPayload {
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecallTaskPayload {
    session_id: String,
    turn_ordinal: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecallEvalTaskPayload {
    session_id: String,
    turn_ordinal: Option<u64>,
    turn_id: Option<String>,
    recall_text: String,
    memory_ids: Vec<i64>,
    recall_at: Option<String>,
    eval_after: Option<String>,
    rerun_for_eval_run_id: Option<i64>,
    filter_telemetry: Option<RecallFilterTelemetry>,
    recall_origin: String,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input_summary: Option<String>,
    injected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallEvalMetadata {
    pub recall_origin: String,
    pub turn_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_input_summary: Option<String>,
    pub injected: Option<bool>,
}

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
    ingest_codex_file_with_config(db, &Config::default(), transcript_path)
}

pub fn ingest_codex_file_with_config(
    db: &Database,
    config: &Config,
    transcript_path: &Path,
) -> anyhow::Result<IngestReport> {
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
        turn.display_text = None;
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
    if inserted_turns > 0 {
        refresh_conversation_segments_for_session(db, config, &parsed.session.id)
            .context("failed to refresh conversation segments")?;
    }

    Ok(IngestReport {
        session_id: parsed.session.id,
        inserted_turns,
        last_inserted_ordinal,
        next_offset: parsed.next_offset,
    })
}

pub fn refresh_conversation_segments_for_session(
    db: &Database,
    config: &Config,
    session_id: &str,
) -> anyhow::Result<u64> {
    let session = db
        .session_by_id(session_id)
        .context("failed to load session for segment refresh")?
        .with_context(|| format!("session {session_id} not found"))?;
    let turns = db
        .completed_turns_for_session_range(session_id, 0, u64::MAX)
        .context("failed to load session turns for segment refresh")?;
    let turns = hydrate_turns(db, &turns).context("failed to hydrate segment turns")?;
    let mut segments = build_conversation_segments(session_id, &turns, &unix_timestamp());
    if session_is_idle(&session, config) {
        if let Some(segment) = segments.last_mut() {
            segment.status = ConversationSegmentStatus::Abandoned;
        }
    }
    let written = db
        .replace_conversation_segments_for_session(session_id, &segments)
        .context("failed to persist conversation segments")?;
    db.refresh_memory_segment_metadata_for_session(session_id, &unix_timestamp())
        .context("failed to refresh memory segment metadata")?;
    db.remove_placeholder_task_keys_from_memories(&unix_timestamp())
        .context("failed to remove placeholder task keys from memories")?;
    db.deactivate_stale_task_state_memories(&unix_timestamp())
        .context("failed to deactivate stale task-state memories")?;
    Ok(written)
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
        match ingest_codex_file_with_config(db, config, &file.path) {
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
    expire_idle_conversation_segments(db, config).context("failed to expire idle segments")?;

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
        match ingest_codex_file_with_config(db, config, &file.path) {
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
    expire_idle_conversation_segments(db, config).context("failed to expire idle segments")?;
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
        let result =
            match task.kind.as_str() {
                TASK_KIND_MEMORY_FORMULATION => run_memory_formulation_task(db, config, &task)
                    .map(|()| TaskRunOutcome::Complete),
                TASK_KIND_MEMORY_CONSOLIDATION => run_memory_consolidation_task(db, config, &task)
                    .map(|()| TaskRunOutcome::Complete),
                TASK_KIND_RECALL => {
                    run_recall_task(db, config, &task).map(|()| TaskRunOutcome::Complete)
                }
                TASK_KIND_RECALL_EVAL => run_recall_eval_task(db, config, &task),
                _ => Ok(TaskRunOutcome::Complete),
            };
        match result {
            Ok(TaskRunOutcome::Complete) => {
                if task.kind == TASK_KIND_MEMORY_FORMULATION {
                    dedupe_active_memories(db, &unix_timestamp())
                        .context("failed to dedupe active memories")?;
                    queue_memory_consolidation_if_due(db, config)
                        .context("failed to queue memory consolidation")?;
                }
                db.complete_task(task_id, &unix_timestamp())
                    .context("failed to complete task")?;
                if task.kind == TASK_KIND_MEMORY_CONSOLIDATION {
                    queue_memory_consolidation_if_due(db, config)
                        .context("failed to queue next memory consolidation")?;
                }
                completed += 1;
            }
            Ok(TaskRunOutcome::Deferred) => {}
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

enum TaskRunOutcome {
    Complete,
    Deferred,
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
    let payload: MemoryFormulationTaskPayload =
        serde_json::from_str(&task.payload_json).context("failed to parse task payload")?;
    let session_id = payload.session_id.as_str();
    let session = db
        .session_by_id(session_id)
        .context("failed to load task session")?
        .context("memory formulation task references missing session")?;
    let requested_source_turn_refs = payload.source_turn_refs;
    let turns = if requested_source_turn_refs.is_empty() {
        db.turns_for_session(session_id, config.turns_between_memory as usize)
            .context("failed to load task turns")?
    } else {
        db.completed_turns_for_source_refs(&requested_source_turn_refs)
            .context("failed to load task source turns")?
    };
    let turns = hydrate_turns(db, &turns).context("failed to hydrate formulation turns")?;
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
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let refinement_candidates = formulation_refinement_candidates(
        db,
        config,
        &embedding_client,
        &project_descriptor,
        &turns,
    )
    .context("failed to load formulation refinement candidates")?;
    let refinement_candidate_by_id = refinement_candidates
        .iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory.clone())))
        .collect::<HashMap<_, _>>();
    let prompt = formulation_prompt(config, &project_descriptor, &turns, &refinement_candidates);
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
    let now = unix_timestamp();
    for draft in drafts {
        let refine_memory_id = draft.refine_memory_id;
        let mut memory = draft.into_record(
            source_turn_refs.clone(),
            now.clone(),
            Some(session.id.clone()),
            Some(session.project_id.clone()),
        );
        attach_origin_segment_metadata(db, &mut memory, session_id)?;
        if memory.scope == MemoryScope::Global {
            memory.project_id = None;
        }
        let refinement_target = refine_memory_id
            .and_then(|memory_id| refinement_candidate_by_id.get(&memory_id).cloned())
            .filter(|existing| valid_refinement_target(existing, &memory, &session.project_id));
        if let Some(existing) = &refinement_target {
            memory.source_turn_refs =
                merge_existing_and_new_source_refs(existing, memory.source_turn_refs.clone());
            memory.lineage_refs = refinement_lineage(existing);
        }
        let text = embedding_text(&memory);
        let vector = embedding_client
            .embed(&text)
            .context("failed to embed memory")?;
        let memory_id = if let Some(existing) = &refinement_target {
            let existing_id = existing.id.context("refinement candidate missing id")?;
            db.consolidate_memories(&[existing_id], &memory, &now)
                .context("failed to insert refined memory")?
        } else {
            db.insert_memory(&memory)
                .context("failed to insert memory")?
        };
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

fn attach_origin_segment_metadata(
    db: &Database,
    memory: &mut MemoryRecord,
    session_id: &str,
) -> anyhow::Result<()> {
    if memory.kind == MemoryKind::TaskState {
        memory.validity = MemoryValidity::ValidWhileSegmentActive;
    }
    let origin_ordinal = memory
        .source_turn_refs
        .iter()
        .filter(|source_ref| source_ref.session_id == session_id)
        .map(|source_ref| source_ref.ordinal)
        .max();
    let Some(origin_ordinal) = origin_ordinal else {
        return Ok(());
    };
    let Some(segment) = db
        .conversation_segment_for_turn(session_id, origin_ordinal)
        .context("failed to load memory origin segment")?
    else {
        return Ok(());
    };
    memory.origin_segment_id = segment.id;
    memory.origin_segment_status = Some(segment.status);
    Ok(())
}

fn run_memory_consolidation_task(
    db: &Database,
    config: &Config,
    task: &TaskRecord,
) -> anyhow::Result<()> {
    let _payload: MemoryConsolidationTaskPayload = serde_json::from_str(&task.payload_json)
        .context("failed to parse consolidation payload")?;
    if should_defer_memory_consolidation(db, config, task)? {
        defer_memory_consolidation(db, task).context("failed to defer memory consolidation")?;
        return Ok(());
    }

    let cluster_memories = consolidation_cluster_memories(db, config)?;
    let clusters = find_consolidation_clusters(
        &cluster_memories,
        config.memory_cluster_distance_threshold,
        config.memory_cluster_min_size,
        config.memory_cluster_max_size,
    );
    let Some(cluster) = clusters.first() else {
        return Ok(());
    };
    let source_memories = db
        .list_memories_by_ids(&cluster.memory_ids)
        .context("failed to load consolidation source memories")?
        .into_iter()
        .filter(|memory| memory.is_active)
        .collect::<Vec<_>>();
    if source_memories.len() < config.memory_cluster_min_size {
        return Ok(());
    }

    let scope = source_memories[0].scope;
    let project_id = source_memories[0].project_id.clone();
    let default_project_descriptor = source_memories[0]
        .project_descriptor
        .as_deref()
        .unwrap_or("consolidated memory")
        .to_string();
    let prompt = consolidation_prompt(&source_memories);
    let consolidation_client = AnthropicMessageClient::new(
        AnthropicMessageConfig::consolidation_from_config(config),
        ReqwestTransport::default(),
    );
    let value = consolidation_client
        .structured_json(consolidation_system_prompt(), &prompt)
        .context("failed to consolidate memories")?;
    let mut drafts = parse_formulation_response(
        &value,
        &default_project_descriptor,
        config.max_memory_length,
    )
    .context("failed to parse consolidated memory")?;
    let Some(draft) = drafts.pop() else {
        return Ok(());
    };

    let now = unix_timestamp();
    let source_turn_refs = merged_source_turn_refs(&source_memories);
    let mut consolidated = draft.into_record(source_turn_refs, now.clone(), None, project_id);
    consolidated.scope = scope;
    if consolidated.scope == MemoryScope::Global {
        consolidated.project_id = None;
    }
    consolidated.lineage_refs = cluster.memory_ids.clone();
    if let Some(origin_session_id) = consolidated
        .source_turn_refs
        .iter()
        .max_by_key(|source_ref| source_ref.ordinal)
        .map(|source_ref| source_ref.session_id.clone())
    {
        if consolidated.session_id.is_none() {
            consolidated.session_id = Some(origin_session_id.clone());
        }
        attach_origin_segment_metadata(db, &mut consolidated, &origin_session_id)?;
    }

    let text = embedding_text(&consolidated);
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let vector = embedding_client
        .embed(&text)
        .context("failed to embed consolidated memory")?;
    let consolidated_id = db
        .consolidate_memories(&cluster.memory_ids, &consolidated, &now)
        .context("failed to persist consolidated memory")?;
    db.upsert_embedding(&EmbeddingRecord {
        memory_id: consolidated_id,
        embedding_model: config.embedding_model.clone(),
        dimensions: vector.len() as u64,
        embedding_blob: yaaml_store::database::encode_f32_embedding(&vector),
        embedded_text_hash: embedded_text_hash(&text),
        updated_at: now,
    })
    .context("failed to persist consolidated memory embedding")?;
    Ok(())
}

fn run_recall_task(db: &Database, config: &Config, task: &TaskRecord) -> anyhow::Result<()> {
    let payload: RecallTaskPayload =
        serde_json::from_str(&task.payload_json).context("failed to parse recall payload")?;
    let session_id = payload.session_id.as_str();
    let turn_ordinal = payload.turn_ordinal;
    let session = db
        .session_by_id(session_id)
        .context("failed to load recall task session")?
        .context("recall task references missing session")?;
    let window = u64::try_from(config.recall_live_turn_window).unwrap_or(u64::MAX);
    let start_ordinal = turn_ordinal.saturating_add(1).saturating_sub(window.max(1));
    let recent_turns = db
        .completed_turns_for_session_range(session_id, start_ordinal, turn_ordinal + 1)
        .context("failed to load recall task turns")?;
    let recent_turns =
        hydrate_turns(db, &recent_turns).context("failed to hydrate recall turns")?;
    if recent_turns.is_empty() {
        return Ok(());
    }
    let query_text = build_active_segment_recall_query(
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
        "background active segment",
    )
    .context("failed to refresh recall")
    .map(|_| ())
}

fn run_recall_eval_task(
    db: &Database,
    config: &Config,
    task: &TaskRecord,
) -> anyhow::Result<TaskRunOutcome> {
    let payload: RecallEvalTaskPayload =
        serde_json::from_str(&task.payload_json).context("failed to parse recall eval payload")?;
    let session_id = payload.session_id.as_str();
    let recall_text = payload.recall_text.as_str();
    let memory_ids = payload.memory_ids.clone();
    let rerun_for_eval_run_id = payload.rerun_for_eval_run_id;
    let Some(anchor) =
        recall_eval_task_anchor(db, &payload).context("failed to load recall eval anchor turn")?
    else {
        defer_recall_eval_until_anchor_exists(db, task)
            .context("failed to defer recall eval task")?;
        return Ok(TaskRunOutcome::Deferred);
    };
    let turn_row_id = anchor.turn_row_id;
    let turn_ordinal = anchor.turn_ordinal;
    let later_turns = recall_eval_context_turns(db, &payload, session_id, turn_ordinal)
        .context("failed to load turns after recall")?;
    let later_turns =
        hydrate_turns(db, &later_turns).context("failed to hydrate recall eval turns")?;
    let now = unix_timestamp();
    if later_turns.is_empty()
        && should_defer_recall_eval_for_later_turns(db, config, task, session_id)?
    {
        defer_recall_eval_until_later_turns_exist(db, task)
            .context("failed to defer recall eval task")?;
        return Ok(TaskRunOutcome::Deferred);
    }
    let segment = db
        .conversation_segment_for_turn(session_id, turn_ordinal)
        .context("failed to load recall eval conversation segment")?;
    let run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            &now,
            &json!({
                "session_id": session_id,
                "turn_id": payload.turn_id,
                "turn_ordinal": turn_ordinal,
                "memory_ids": memory_ids,
                "rerun_for_eval_run_id": rerun_for_eval_run_id,
                "rating_delay_seconds": 600,
                "rubric": "1-5 recall relevance, concision, and actionability",
                "recall_origin": payload.recall_origin,
                "tool_name": payload.tool_name,
                "tool_use_id": payload.tool_use_id,
                "tool_input_summary": payload.tool_input_summary,
                "injected": payload.injected,
            })
            .to_string(),
            EvalRunMetadata {
                session_id: Some(session_id.to_string()),
                turn_ordinal: Some(turn_ordinal),
                agent_turn_id: payload.turn_id.clone(),
                recall_origin: payload.recall_origin.clone(),
                tool_name: payload.tool_name.clone(),
                tool_use_id: payload.tool_use_id.clone(),
                tool_input_summary: payload.tool_input_summary.clone(),
                injected: payload.injected,
                segment_start_turn_ordinal: segment
                    .as_ref()
                    .map(|segment| segment.start_turn_ordinal),
                segment_end_turn_ordinal: segment.as_ref().map(|segment| segment.end_turn_ordinal),
                segment_summary: segment.as_ref().map(|segment| segment.summary.clone()),
                segment_task_keys: segment
                    .as_ref()
                    .map(|segment| segment.task_keys.clone())
                    .unwrap_or_default(),
            },
        )
        .context("failed to create recall eval run")?;
    let eval_targets = recall_eval_targets(db, &memory_ids, recall_text)
        .context("failed to build recall eval targets")?;
    if later_turns.is_empty() {
        for target in &eval_targets {
            db.insert_eval_result(
                run_id,
                turn_row_id,
                target.memory_id,
                "insufficient_context",
                "No subsequent completed turns were captured after recall, so recall usefulness cannot be scored.",
                &now,
            )
            .context("failed to insert insufficient-context recall eval result")?;
        }
        db.complete_eval_run(run_id, &unix_timestamp())
            .context("failed to complete recall eval run")?;
        return Ok(TaskRunOutcome::Complete);
    }
    let judge_client = JudgeClient::from_config(config, false)
        .context("recall eval judge provider is unavailable")?;
    for target in &eval_targets {
        let prompt = recall_eval_prompt(&target.recall_text, &later_turns);
        let outcome = judge_client
            .structured_json(recall_eval_system_prompt(), &prompt)
            .map(|value| parse_eval_judge_response(&value))
            .context("failed to rate recall")?;
        let score = if memory_ids.is_empty() {
            abstention_eval_score(&outcome.score)
        } else {
            outcome.score.as_str()
        };
        db.insert_eval_result(
            run_id,
            turn_row_id,
            target.memory_id,
            score,
            &outcome.rationale,
            &now,
        )
        .context("failed to insert recall eval result")?;
    }
    db.complete_eval_run(run_id, &unix_timestamp())
        .context("failed to complete recall eval run")?;
    Ok(TaskRunOutcome::Complete)
}

fn abstention_eval_score(score: &str) -> &'static str {
    match numeric_eval_score(score) {
        Some(score) if score >= 4 => "missed_useful_abstention",
        _ => "clean_abstention",
    }
}

fn numeric_eval_score(score: &str) -> Option<u8> {
    match score.trim() {
        "1" => Some(1),
        "2" => Some(2),
        "3" => Some(3),
        "4" => Some(4),
        "5" => Some(5),
        _ => None,
    }
}

fn recall_eval_context_turns(
    db: &Database,
    _payload: &RecallEvalTaskPayload,
    session_id: &str,
    turn_ordinal: u64,
) -> anyhow::Result<Vec<TurnRecord>> {
    db.completed_turns_for_session_after_ordinal(session_id, turn_ordinal, 20)
        .context("failed to load turns after recall")
}

struct RecallEvalResolvedAnchor {
    turn_row_id: i64,
    turn_ordinal: u64,
}

fn recall_eval_task_anchor(
    db: &Database,
    payload: &RecallEvalTaskPayload,
) -> anyhow::Result<Option<RecallEvalResolvedAnchor>> {
    if let Some(turn_ordinal) = payload.turn_ordinal {
        return Ok(db
            .turn_row_id_for_session_ordinal(&payload.session_id, turn_ordinal)?
            .map(|turn_row_id| RecallEvalResolvedAnchor {
                turn_row_id,
                turn_ordinal,
            }));
    }
    let Some(turn_id) = payload.turn_id.as_deref() else {
        return Ok(None);
    };
    Ok(db
        .turn_row_for_session_turn_id(&payload.session_id, turn_id)?
        .map(|(turn_row_id, turn_ordinal)| RecallEvalResolvedAnchor {
            turn_row_id,
            turn_ordinal,
        }))
}

fn defer_recall_eval_until_anchor_exists(db: &Database, task: &TaskRecord) -> anyhow::Result<i64> {
    let task_id = task.id.context("recall eval task missing id")?;
    let next_run_seconds = unix_timestamp_seconds() + 60;
    let now = format!("unix:{}", unix_timestamp_seconds());
    db.reschedule_task(
        task_id,
        task.attempts.saturating_add(1),
        &format!("unix:{next_run_seconds}"),
        "waiting for recall eval anchor turn",
        &now,
    )
    .context("failed to reschedule deferred recall eval task")?;
    Ok(task_id)
}

fn should_defer_recall_eval_for_later_turns(
    db: &Database,
    config: &Config,
    task: &TaskRecord,
    session_id: &str,
) -> anyhow::Result<bool> {
    if task.attempts.saturating_add(1) >= task.max_attempts {
        return Ok(false);
    }
    let Some(session) = db
        .session_by_id(session_id)
        .context("failed to load recall eval session")?
    else {
        return Ok(false);
    };
    Ok(!session_is_idle(&session, config))
}

fn defer_recall_eval_until_later_turns_exist(
    db: &Database,
    task: &TaskRecord,
) -> anyhow::Result<i64> {
    let task_id = task.id.context("recall eval task missing id")?;
    let next_run_seconds = unix_timestamp_seconds() + 600;
    let now = format!("unix:{}", unix_timestamp_seconds());
    db.reschedule_task(
        task_id,
        task.attempts.saturating_add(1),
        &format!("unix:{next_run_seconds}"),
        "waiting for subsequent turns before recall eval",
        &now,
    )
    .context("failed to reschedule deferred recall eval task")?;
    Ok(task_id)
}

struct RecallEvalTarget {
    memory_id: Option<i64>,
    recall_text: String,
}

fn recall_eval_targets(
    db: &Database,
    memory_ids: &[i64],
    fallback_recall_text: &str,
) -> anyhow::Result<Vec<RecallEvalTarget>> {
    if memory_ids.is_empty() {
        return Ok(vec![RecallEvalTarget {
            memory_id: None,
            recall_text: fallback_recall_text.to_string(),
        }]);
    }
    let memories = db
        .list_memories_by_ids(memory_ids)
        .context("failed to load recall eval memories")?;
    let mut targets = Vec::new();
    for memory_id in memory_ids {
        if let Some(memory) = memories.iter().find(|memory| memory.id == Some(*memory_id)) {
            targets.push(RecallEvalTarget {
                memory_id: Some(*memory_id),
                recall_text: recall_eval_memory_text(memory),
            });
        }
    }
    if targets.is_empty() {
        targets.push(RecallEvalTarget {
            memory_id: None,
            recall_text: fallback_recall_text.to_string(),
        });
    }
    Ok(targets)
}

fn reconstruct_recall_eval_text(db: &Database, memory_ids: &[i64]) -> anyhow::Result<String> {
    let memories = db
        .list_memories_by_ids(memory_ids)
        .context("failed to load stale recall eval memories")?;
    let mut text = String::new();
    for memory_id in memory_ids {
        if let Some(memory) = memories.iter().find(|memory| memory.id == Some(*memory_id)) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&recall_eval_memory_text(memory));
        }
    }
    Ok(text)
}

fn recall_eval_memory_text(memory: &MemoryRecord) -> String {
    format!(
        "## {}\n\n{}\n\ncreated_at: {}\noriginating_project: {}\n",
        memory.title,
        memory.body,
        memory.created_at,
        memory.project_descriptor.as_deref().unwrap_or("unknown")
    )
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StaleRecallEvalQueueReport {
    pub scanned_runs: u64,
    pub stale_runs: u64,
    pub queued: u64,
    pub skipped_already_scored: u64,
    pub skipped_already_pending: u64,
    pub skipped_rerun: u64,
    pub skipped_missing_anchor: u64,
    pub skipped_no_later_turns: u64,
    pub skipped_empty_recall_text: u64,
}

pub fn queue_stale_recall_eval_tasks(db: &Database, limit: usize) -> anyhow::Result<u64> {
    Ok(queue_stale_recall_eval_tasks_report(db, limit)?.queued)
}

pub fn queue_stale_recall_eval_tasks_report(
    db: &Database,
    limit: usize,
) -> anyhow::Result<StaleRecallEvalQueueReport> {
    let runs = db
        .list_eval_runs(200)
        .context("failed to list eval runs for stale recall eval queue")?;
    let mut report = StaleRecallEvalQueueReport::default();
    for run in runs {
        if report.queued as usize >= limit {
            break;
        }
        report.scanned_runs += 1;
        if run.score.as_deref() != Some("insufficient_context") {
            continue;
        }
        report.stale_runs += 1;
        if db
            .recall_eval_scored_rerun_exists(run.id)
            .context("failed to check scored recall eval rerun state")?
        {
            report.skipped_already_scored += 1;
            continue;
        }
        if db
            .recall_eval_pending_rerun_exists(run.id)
            .context("failed to check pending recall eval rerun state")?
        {
            report.skipped_already_pending += 1;
            continue;
        }
        let config_json = serde_json::from_str::<serde_json::Value>(&run.config_json)
            .context("failed to parse eval run config")?;
        if config_json
            .get("rerun_for_eval_run_id")
            .and_then(serde_json::Value::as_i64)
            .is_some()
        {
            report.skipped_rerun += 1;
            continue;
        }
        let Some(session_id) = run.session_id.as_deref() else {
            report.skipped_missing_anchor += 1;
            continue;
        };
        let Some(turn_ordinal) = run.turn_ordinal else {
            report.skipped_missing_anchor += 1;
            continue;
        };
        let later_turns = db
            .completed_turns_for_session_after_ordinal(session_id, turn_ordinal, 1)
            .context("failed to check later turns for stale recall eval")?;
        if later_turns.is_empty() {
            report.skipped_no_later_turns += 1;
            continue;
        }
        let memory_ids = config_json
            .get("memory_ids")
            .and_then(serde_json::Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(serde_json::Value::as_i64)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let recall_text = reconstruct_recall_eval_text(db, &memory_ids)
            .context("failed to reconstruct stale recall eval text")?;
        if recall_text.trim().is_empty() {
            report.skipped_empty_recall_text += 1;
            continue;
        }
        let payload_json = serde_json::to_string(&RecallEvalTaskPayload {
            session_id: session_id.to_string(),
            turn_ordinal: Some(turn_ordinal),
            turn_id: None,
            recall_text,
            memory_ids,
            recall_at: None,
            eval_after: None,
            rerun_for_eval_run_id: Some(run.id),
            filter_telemetry: None,
            recall_origin: run.recall_origin,
            tool_name: run.tool_name,
            tool_use_id: run.tool_use_id,
            tool_input_summary: run.tool_input_summary,
            injected: run.injected,
        })
        .context("failed to serialize stale recall eval task payload")?;
        let now = unix_timestamp();
        db.enqueue_task(&TaskRecord {
            id: None,
            kind: TASK_KIND_RECALL_EVAL.to_string(),
            status: TaskStatus::Queued,
            priority: 0,
            payload_json,
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .context("failed to enqueue stale recall eval task")?;
        report.queued += 1;
    }
    Ok(report)
}

pub fn queue_memory_consolidation_if_due(
    db: &Database,
    config: &Config,
) -> anyhow::Result<Option<i64>> {
    let active_tasks = db
        .count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Queued)
        .context("failed to count queued consolidation tasks")?
        + db.count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Running)
            .context("failed to count running consolidation tasks")?
        + db.count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Parked)
            .context("failed to count parked consolidation tasks")?;
    if active_tasks > 0 {
        return Ok(None);
    }

    let Some(latest_memory_created_at) =
        latest_top_consolidation_cluster_memory_created_at(db, config)
            .context("failed to load latest consolidation cluster memory timestamp")?
    else {
        return Ok(None);
    };
    if let Some(latest_completed_at) = db
        .latest_task_updated_at(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Completed)
        .context("failed to load latest consolidation timestamp")?
    {
        if let (Some(memory_seconds), Some(completed_seconds)) = (
            timestamp_seconds(&latest_memory_created_at),
            timestamp_seconds(&latest_completed_at),
        ) {
            if completed_seconds >= memory_seconds
                && !active_consolidation_cluster_exists(db, config)
                    .context("failed to check active consolidation clusters")?
            {
                return Ok(None);
            }
        }
    }

    let latest_memory_seconds =
        timestamp_seconds(&latest_memory_created_at).unwrap_or_else(unix_timestamp_seconds);
    let next_run_seconds =
        latest_memory_seconds.saturating_add(config.consolidation_dark_period_seconds as i64);
    let now = unix_timestamp();
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_MEMORY_CONSOLIDATION.to_string(),
        status: TaskStatus::Queued,
        priority: -10,
        payload_json: serde_json::to_string(&MemoryConsolidationTaskPayload {
            reason: "memory_dark_period".to_string(),
        })
        .context("failed to serialize consolidation task payload")?,
        attempts: 0,
        max_attempts: 20,
        next_run_at: Some(format!("unix:{next_run_seconds}")),
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    db.enqueue_task(&task)
        .map(Some)
        .context("failed to enqueue memory consolidation task")
}

fn active_consolidation_cluster_exists(db: &Database, config: &Config) -> anyhow::Result<bool> {
    Ok(top_consolidation_cluster(db, config)?.is_some())
}

fn top_consolidation_cluster(
    db: &Database,
    config: &Config,
) -> anyhow::Result<Option<yaaml_core::MemoryCluster>> {
    let cluster_memories = consolidation_cluster_memories(db, config)?;
    let clusters = find_consolidation_clusters(
        &cluster_memories,
        config.memory_cluster_distance_threshold,
        config.memory_cluster_min_size,
        config.memory_cluster_max_size,
    );
    Ok(clusters.into_iter().next())
}

fn latest_top_consolidation_cluster_memory_created_at(
    db: &Database,
    config: &Config,
) -> anyhow::Result<Option<String>> {
    let Some(cluster) = top_consolidation_cluster(db, config)? else {
        return Ok(None);
    };
    let memories = db
        .list_memories_by_ids(&cluster.memory_ids)
        .context("failed to load top consolidation cluster memories")?;
    Ok(memories
        .into_iter()
        .filter(|memory| memory.is_active)
        .max_by_key(|memory| timestamp_seconds(&memory.created_at).unwrap_or(i64::MIN))
        .map(|memory| memory.created_at))
}

fn should_defer_memory_consolidation(
    db: &Database,
    config: &Config,
    task: &TaskRecord,
) -> anyhow::Result<bool> {
    if task.attempts.saturating_add(1) >= task.max_attempts {
        return Ok(false);
    }
    let Some(latest_memory_created_at) =
        latest_top_consolidation_cluster_memory_created_at(db, config)
            .context("failed to load latest consolidation cluster memory timestamp")?
    else {
        return Ok(false);
    };
    let Some(latest_memory_seconds) = timestamp_seconds(&latest_memory_created_at) else {
        return Ok(false);
    };
    let elapsed = unix_timestamp_seconds().saturating_sub(latest_memory_seconds) as u64;
    Ok(elapsed < config.consolidation_dark_period_seconds)
}

fn defer_memory_consolidation(db: &Database, task: &TaskRecord) -> anyhow::Result<i64> {
    let next_run_seconds = unix_timestamp_seconds() + 60;
    let now = unix_timestamp();
    let deferred = TaskRecord {
        id: None,
        kind: task.kind.clone(),
        status: TaskStatus::Queued,
        priority: task.priority,
        payload_json: task.payload_json.clone(),
        attempts: task.attempts.saturating_add(1),
        max_attempts: task.max_attempts,
        next_run_at: Some(format!("unix:{next_run_seconds}")),
        last_error: Some("waiting for memory consolidation dark period".to_string()),
        created_at: task.created_at.clone(),
        updated_at: now,
    };
    db.enqueue_task(&deferred)
        .context("failed to enqueue deferred memory consolidation task")
}

fn consolidation_cluster_memories(
    db: &Database,
    config: &Config,
) -> anyhow::Result<Vec<ClusterMemory>> {
    let memories = db.list_memories().context("failed to list memories")?;
    let mut cluster_memories = Vec::new();
    for memory in memories.into_iter().filter(|memory| memory.is_active) {
        let Some(memory_id) = memory.id else {
            continue;
        };
        let Some(embedding) = db
            .get_embedding(memory_id)
            .context("failed to load memory embedding")?
        else {
            continue;
        };
        if embedding.embedding_model != config.embedding_model {
            continue;
        }
        let Some(vector) = yaaml_store::database::decode_f32_embedding(&embedding.embedding_blob)
        else {
            continue;
        };
        cluster_memories.push(ClusterMemory {
            memory_id,
            scope: memory.scope,
            project_id: memory.project_id,
            title: memory.title,
            body: memory.body,
            task_keys: memory.task_keys,
            lineage_refs: memory.lineage_refs,
            embedding: vector,
        });
    }
    Ok(cluster_memories)
}

fn merged_source_turn_refs(memories: &[MemoryRecord]) -> Vec<SourceTurnRef> {
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for memory in memories {
        for source_ref in &memory.source_turn_refs {
            let key = (source_ref.session_id.clone(), source_ref.ordinal);
            if seen.insert(key) {
                refs.push(source_ref.clone());
            }
        }
    }
    refs
}

fn merge_existing_and_new_source_refs(
    existing: &MemoryRecord,
    new_refs: Vec<SourceTurnRef>,
) -> Vec<SourceTurnRef> {
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for source_ref in existing.source_turn_refs.iter().chain(new_refs.iter()) {
        let key = (source_ref.session_id.clone(), source_ref.ordinal);
        if seen.insert(key) {
            refs.push(source_ref.clone());
        }
    }
    refs
}

fn refinement_lineage(existing: &MemoryRecord) -> Vec<i64> {
    let mut refs = Vec::new();
    if let Some(id) = existing.id {
        refs.push(id);
    }
    for lineage_ref in &existing.lineage_refs {
        if !refs.contains(lineage_ref) {
            refs.push(*lineage_ref);
        }
    }
    refs
}

fn valid_refinement_target(
    existing: &MemoryRecord,
    replacement: &MemoryRecord,
    current_project_id: &str,
) -> bool {
    if !existing.is_active || existing.scope != replacement.scope {
        return false;
    }
    match replacement.scope {
        MemoryScope::Global => existing.project_id.is_none(),
        MemoryScope::Project => {
            existing.project_id.as_deref() == Some(current_project_id)
                && replacement.project_id.as_deref() == Some(current_project_id)
        }
    }
}

fn formulation_system_prompt() -> &'static str {
    concat!(
        "Create concise durable memories from coding-agent transcript turns. ",
        "Return only JSON shaped as {\"memories\":[{\"title\":\"...\",\"body\":\"...\",\"scope\":\"project\"|\"global\",\"kind\":\"preference\"|\"lesson\"|\"workflow\"|\"project_fact\"|\"task_checkpoint\"|\"task_state\",\"task_keys\":[\"type:value\"],\"project_descriptor\":\"...\",\"refine_memory_id\":123|null}]}. ",
        "Return {\"memories\":[]} when the turns contain only ordinary progress updates, one-off command output, transient narration, or no durable lesson. ",
        "Prefer zero or one small, granular memory per batch; create multiple memories only when the turns contain distinct durable lessons or preferences. ",
        "If new turns correct, extend, or make more specific one of the provided existing candidate memories, return a full replacement memory and set refine_memory_id to that candidate id. ",
        "If the insight is distinct from the candidates, omit refine_memory_id or set it to null. ",
        "Do not refine a candidate unless the replacement preserves still-true durable details from the existing memory. ",
        "Focus memories on insights gained while solving the problem and on redirection provided by the user. ",
        "Always capture repeated user corrections, preferences, and process guidance as their own concise memories, including coding style preferences such as functional vs imperative style. ",
        "Use project scope when the preference is tied to the current project or language; use global scope only for durable cross-project user preferences or agent workflow patterns. ",
        "Use task_state only for segment-specific or short-lived state that should expire when the current conversation topic moves on: local status facts, proposed fixes, implementation order, unresolved next steps, open questions, blockers, and follow-up work. ",
        "Use task_checkpoint for resumable PR, ticket, branch, or explicitly named task state that should return only when that same identity is mentioned again; include only strong task keys copied from the transcript, and do not invent placeholder keys. ",
        "Do not encode completed implementation plans as durable workflow or lesson memories; return no memory unless there is a reusable lesson. ",
        "Use workflow only for reusable procedures that should remain useful after the current task is complete."
    )
}

fn consolidation_system_prompt() -> &'static str {
    concat!(
        "Merge overlapping coding-agent memories into one concise durable memory. ",
        "Return only JSON shaped as {\"memories\":[{\"title\":\"...\",\"body\":\"...\",\"scope\":\"project\"|\"global\",\"kind\":\"preference\"|\"lesson\"|\"workflow\"|\"project_fact\"|\"task_checkpoint\"|\"task_state\",\"task_keys\":[\"type:value\"],\"project_descriptor\":\"...\"}]}. ",
        "Preserve concrete facts, durable user preferences, commands, file paths, project state, and unresolved follow-up context. ",
        "Keep segment-local plans, implementation order, blockers, and next steps as task_state rather than durable workflow. ",
        "Keep resumable PR/ticket/branch status as task_checkpoint only when the source memories include a strong task identity key. ",
        "Remove repetition and transient narration. ",
        "Do not invent facts not present in the source memories. ",
        "Return exactly one memory."
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
        "1: recalled context was not relevant. ",
        "For scores 1 or 2, name the main failure mode in the rationale when possible: stale task state, wrong context, noisy metadata, too generic, or too long."
    )
}

fn formulation_prompt(
    config: &Config,
    project_descriptor: &str,
    turns: &[yaaml_core::TurnRecord],
    refinement_candidates: &[MemoryRecord],
) -> String {
    let mut prompt = format!("Project descriptor: {project_descriptor}\n\n");
    if refinement_candidates.is_empty() {
        prompt.push_str("Existing candidate memories: none\n\n");
    } else {
        prompt.push_str(
            "Existing candidate memories that may be refined. Only use these ids for refine_memory_id:\n",
        );
        for memory in refinement_candidates {
            prompt.push_str(&format!(
                "\nMemory {}\nTitle: {}\nScope: {}\nKind: {}\nProject: {}\nBody:\n{}\n",
                memory.id.unwrap_or_default(),
                truncate_chars(&memory.title, 240),
                memory.scope.as_str(),
                memory.kind.as_str(),
                memory.project_descriptor.as_deref().unwrap_or("unknown"),
                truncate_chars(&memory.body, 1_200)
            ));
        }
        prompt.push('\n');
    }
    prompt.push_str("Turns:\n");
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

fn formulation_similarity_query(
    config: &Config,
    project_descriptor: &str,
    turns: &[yaaml_core::TurnRecord],
) -> String {
    let mut query = format!("Project descriptor: {project_descriptor}\n\nTurns:\n");
    let max_chars = 12_000;
    for turn in turns {
        if query.chars().count() >= max_chars {
            break;
        }
        let remaining = max_chars.saturating_sub(query.chars().count());
        let text = formulation_turn_text(
            turn.display_text.as_deref().unwrap_or(""),
            remaining,
            config.tool_call_truncation_chars,
        );
        query.push_str(&format!("\nTurn {}:\n{}\n", turn.ordinal, text));
    }
    query
}

fn formulation_refinement_candidates(
    db: &Database,
    config: &Config,
    embedding_client: &OpenAiEmbeddingClient<ReqwestTransport>,
    project_descriptor: &str,
    turns: &[yaaml_core::TurnRecord],
) -> anyhow::Result<Vec<MemoryRecord>> {
    let active_memories = db
        .list_memories()
        .context("failed to list active memories for formulation refinement")?
        .into_iter()
        .filter(|memory| memory.is_active)
        .collect::<Vec<_>>();
    if active_memories.is_empty() {
        return Ok(Vec::new());
    }

    let query = formulation_similarity_query(config, project_descriptor, turns);
    let query_embedding = match embedding_client.embed(&query) {
        Ok(embedding) => embedding,
        Err(_) => return Ok(Vec::new()),
    };
    let index = SqliteExactVectorIndex::new(db, config.embedding_model.clone(), unix_timestamp());
    let hits = index
        .search(&query_embedding, 5, 0.70)
        .context("failed to search candidate memories for formulation refinement")?;
    let memory_ids = hits.iter().map(|hit| hit.memory_id).collect::<Vec<_>>();
    db.list_active_memories_by_ids(&memory_ids)
        .context("failed to load formulation refinement candidates")
}

fn consolidation_prompt(memories: &[MemoryRecord]) -> String {
    let mut prompt = String::from(
        "Consolidate these overlapping memories into exactly one replacement memory. Keep it specific and actionable.\n\n",
    );
    for memory in memories {
        prompt.push_str(&format!(
            "Memory {}\nTitle: {}\nScope: {}\nProject: {}\nBody:\n{}\n\n",
            memory.id.unwrap_or_default(),
            truncate_chars(&memory.title, 300),
            memory.scope.as_str(),
            memory.project_descriptor.as_deref().unwrap_or("unknown"),
            truncate_chars(&memory.body, 3_000)
        ));
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
    let payload_json = serde_json::to_string(&MemoryFormulationTaskPayload {
        session_id: session_id.to_string(),
        start_ordinal,
        end_ordinal,
        source_turn_refs,
    })
    .context("failed to serialize formulation task payload")?;
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
    for memory in db
        .list_memories()
        .context("failed to list memories")?
        .into_iter()
        .filter(|memory| memory.is_active)
    {
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

pub fn expire_idle_conversation_segments(db: &Database, config: &Config) -> anyhow::Result<u64> {
    let now = unix_timestamp();
    let mut expired = 0_u64;
    for (session, _completed_turns) in db
        .sessions_with_completed_turn_counts()
        .context("failed to load sessions for segment expiry")?
    {
        if session_is_idle(&session, config) {
            expired += db
                .abandon_active_conversation_segments_for_session(&session.id, &now)
                .context("failed to abandon idle session segments")?;
        }
    }
    if expired > 0 {
        db.remove_placeholder_task_keys_from_memories(&now)
            .context("failed to remove placeholder task keys from memories")?;
        db.deactivate_stale_task_state_memories(&now)
            .context("failed to deactivate stale task-state memories")?;
    }
    Ok(expired)
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
    let payload_json = serde_json::to_string(&RecallTaskPayload {
        session_id: session_id.to_string(),
        turn_ordinal,
    })
    .context("failed to serialize recall task payload")?;
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
    filter_telemetry: Option<&RecallFilterTelemetry>,
    priority: i64,
) -> anyhow::Result<i64> {
    queue_recall_eval_after_turn_with_metadata(
        db,
        session_id,
        Some(turn_ordinal),
        None,
        recall_text,
        memory_ids,
        filter_telemetry,
        RecallEvalMetadata {
            recall_origin: "session_background".to_string(),
            turn_id: None,
            tool_name: None,
            tool_use_id: None,
            tool_input_summary: None,
            injected: None,
        },
        priority,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn queue_recall_eval_after_turn_with_metadata(
    db: &Database,
    session_id: &str,
    turn_ordinal: Option<u64>,
    turn_id: Option<String>,
    recall_text: &str,
    memory_ids: &[i64],
    filter_telemetry: Option<&RecallFilterTelemetry>,
    metadata: RecallEvalMetadata,
    priority: i64,
) -> anyhow::Result<i64> {
    let now_seconds = unix_timestamp_seconds();
    if metadata.recall_origin == "session_background" {
        if let Some(turn_ordinal) = turn_ordinal {
            if db
                .recall_eval_exists_for_anchor(session_id, turn_ordinal)
                .context("failed to check existing recall eval task")?
            {
                return Ok(0);
            }
        }
    }
    let payload_json = serde_json::to_string(&RecallEvalTaskPayload {
        session_id: session_id.to_string(),
        turn_ordinal,
        turn_id,
        recall_text: recall_text.to_string(),
        memory_ids: memory_ids.to_vec(),
        recall_at: Some(format!("unix:{now_seconds}")),
        eval_after: Some(format!("unix:{}", now_seconds + 600)),
        rerun_for_eval_run_id: None,
        filter_telemetry: filter_telemetry.cloned(),
        recall_origin: metadata.recall_origin,
        tool_name: metadata.tool_name,
        tool_use_id: metadata.tool_use_id,
        tool_input_summary: metadata.tool_input_summary,
        injected: metadata.injected,
    })
    .context("failed to serialize recall eval task payload")?;
    let now = format!("unix:{now_seconds}");
    let task = TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json,
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
    let recall_turns = active_segment_recall_turns(recent_turns);
    let query_text = build_recall_query(
        recall_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    let query_context = context_from_turns(recall_turns, project_id, &query_text);
    let (query_task_keys, current_segment_id) =
        active_segment_recall_metadata(db, &query_text, recent_turns)?;
    let candidates = rank_recall_candidates(
        &hits,
        &memories,
        &project_id_string,
        &query_context,
        &query_task_keys,
        RecallRankingOptions {
            project_tiebreaker: config.recall_project_tiebreaker,
            project_score_bonus: config.recall_project_score_bonus,
        },
    );
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<Vec<_>>();
    let eval_history = db
        .eval_history_for_memories(&candidate_ids)
        .context("failed to load recall candidate eval history")?;
    let memory_health = build_memory_health_summaries(&memories, &eval_history);
    let candidates = apply_health_action_rerank(candidates, &memories, &memory_health);
    let mut filter_result = select_recall_candidates_with_llm_filter(
        config,
        candidates,
        &memories,
        RecallFilterRequest {
            current_project_id: &project_id_string,
            query_text: &query_text,
            query_context: &query_context,
            query_task_keys: &query_task_keys,
            current_segment_id,
        },
    );
    filter_result.selected = suppress_source_overlapping_candidates(
        filter_result.selected,
        &mut filter_result.debug_candidates,
        &memories,
        recall_turns,
    );
    let recent_memory_ids = recent_turns
        .last()
        .map(|turn| &turn.session_id)
        .zip(cooldown_since_unix(
            &now,
            config.recall_memory_cooldown_seconds,
        ))
        .map(|(session_id, since_unix)| {
            filter_result.telemetry.cooldown_since_unix = Some(since_unix);
            db.recent_recalled_memory_ids(session_id, since_unix)
                .context("failed to load recent recall memory ids")
        })
        .transpose()?
        .unwrap_or_default();
    let selected_before_cooldown = filter_result.selected.len();
    filter_result.telemetry.recent_recall_candidate_count = recent_memory_ids.len();
    let selected = suppress_recently_recalled_candidates(
        filter_result.selected,
        &mut filter_result.debug_candidates,
        &recent_memory_ids,
    );
    filter_result.telemetry.cooldown_suppressed_count =
        selected_before_cooldown.saturating_sub(selected.len());
    filter_result.telemetry.final_selected_count = selected.len();
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
                    rank: candidate.rank.clone(),
                })
        })
        .collect::<Vec<_>>();
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
                Some(&filter_result.telemetry),
                0,
            )?;
        }
    }
    Ok(write)
}

fn active_segment_recall_metadata(
    db: &Database,
    query_text: &str,
    recent_turns: &[TurnRecord],
) -> anyhow::Result<(Vec<String>, Option<i64>)> {
    let query_task_keys = segment_task_keys(query_text);
    let Some(latest_turn) = recent_turns.last() else {
        return Ok((query_task_keys, None));
    };
    let Some(segment) = db
        .conversation_segment_for_turn(&latest_turn.session_id, latest_turn.ordinal)
        .context("failed to load active conversation segment for recall")?
    else {
        return Ok((query_task_keys, None));
    };
    Ok((
        merge_task_keys(&query_task_keys, &segment.task_keys),
        segment.id,
    ))
}

fn cooldown_since_unix(query_timestamp: &str, cooldown_seconds: u64) -> Option<i64> {
    let timestamp = query_timestamp.strip_prefix("unix:")?.parse::<i64>().ok()?;
    Some(timestamp.saturating_sub(i64::try_from(cooldown_seconds).unwrap_or(i64::MAX)))
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
        let config = Config {
            max_formulation_tokens: 100,
            tool_call_truncation_chars: 40,
            ..Config::default()
        };
        let turns = vec![yaaml_core::TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 0,
            byte_end: 10,
            observed_at: None,
            status: yaaml_core::TurnStatus::Completed,
            display_text: Some(format!("user text\ntool output: {}", "x".repeat(10_000))),
            cwd: None,
            context: None,
        }];

        let prompt = formulation_prompt(&config, "yaaml", &turns, &[]);

        assert!(prompt.chars().count() <= 360);
        assert!(prompt.contains("[truncated]"));
    }

    #[test]
    fn formulation_prompt_includes_refinement_candidates() {
        let config = Config::default();
        let candidates = vec![MemoryRecord {
            id: Some(42),
            title: "Java style preference".to_string(),
            body: "Prefer stream-style transformations.".to_string(),
            scope: MemoryScope::Project,
            kind: yaaml_core::MemoryKind::Preference,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/java".to_string()),
            project_descriptor: Some("java, riskarbiter".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: MemoryValidity::Durable,
        }];

        let prompt = formulation_prompt(&config, "java", &[], &candidates);

        assert!(prompt.contains("Only use these ids for refine_memory_id"));
        assert!(prompt.contains("Memory 42"));
        assert!(prompt.contains("Java style preference"));
    }

    #[test]
    fn formulation_system_prompt_mentions_user_preferences() {
        let prompt = formulation_system_prompt();

        assert!(prompt.contains("refine_memory_id"));
        assert!(prompt.contains("full replacement memory"));
        assert!(prompt.contains("insights gained while solving the problem"));
        assert!(prompt.contains("redirection provided by the user"));
        assert!(prompt.contains("repeated user corrections"));
        assert!(prompt.contains("coding style preferences"));
        assert!(prompt.contains("functional vs imperative"));
        assert!(prompt.contains("unresolved next steps"));
        assert!(prompt.contains("implementation order"));
        assert!(prompt.contains("Use workflow only for reusable procedures"));
        assert!(prompt.contains("completed implementation plans"));
    }

    #[test]
    fn recall_eval_system_prompt_uses_one_to_five_rubric() {
        let prompt = recall_eval_system_prompt();

        assert!(prompt.contains("\"1\" to \"5\""));
        assert!(prompt.contains("relevant, concise, and actionable"));
        assert!(prompt.contains("weak relevance"));
        assert!(prompt.contains("stale task state"));
        assert!(prompt.contains("not relevant"));
    }
}
