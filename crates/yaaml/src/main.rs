use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

#[cfg(unix)]
use std::ffi::CStr;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use yaaml::llm_judge::JudgeClient;
use yaaml::memory_health::{apply_health_action_rerank, build_memory_health_summaries};
use yaaml::recall_filter::{
    effective_recall_selection_limit, select_recall_candidates_with_llm_filter,
    suppress_recently_recalled_candidates, RecallFilterRequest, RecallFilterTelemetry,
};
use yaaml::turn_hydration::{context_from_turns, hydrate_turns};
use yaaml_core::{
    active_segment_recall_turns, context_score, counterfactual_citation_score,
    derive_project_descriptor, embedded_text_hash, embedding_text, extract_task_keys,
    infer_context_from_memory, infer_context_from_path, infer_context_from_text, infer_memory_kind,
    is_transient_plan_memory, merge_contexts, merge_task_keys, parse_eval_judge_response,
    parse_memory_ids, rank_recall_candidates, recall_file_path, render_recall_markdown,
    session_recall_file_path, write_recall_file, Config, ConfigPaths, ContextMetadata,
    ConversationSegmentRecord, EmbeddingRecord, MemoryKind, MemoryRecord, MemoryScope,
    MemoryValidity, RecallMemory, RecallRankDetails, RecallRankingOptions, RecallWrite,
    SessionRecord, TurnRecord, VectorIndex,
};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::ReqwestTransport;
use yaaml_store::database::{
    EvalResultRecord, EvalRunMetadata, EvalRunRecord, RecallEvalTaskRecord, TaskListRecord,
};
use yaaml_store::lock::DaemonLock;
use yaaml_store::{Database, SqliteExactVectorIndex};

#[derive(Debug, Parser)]
#[command(name = "yaaml")]
#[command(about = "Yet Another Agent Memory Layer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon process.
    Daemon(DaemonArgs),
    /// Install YAAML skills and local setup.
    Init,
    /// Ingest existing agent transcripts once.
    Ingest(IngestArgs),
    /// Manage the user service.
    Service(ServiceArgs),
    /// Run evaluation workflows.
    Eval(EvalArgs),
    /// Show daemon, memory, backlog, and provider status.
    Status(StatusArgs),
    /// Show recall coverage, volume, and usefulness metrics.
    Stats(StatsArgs),
    /// Inspect configuration.
    Config(ConfigArgs),
    /// Inspect or manage daemon tasks.
    Tasks(TasksArgs),
    /// Inspect or rebuild stored memories.
    Memories(MemoriesArgs),
    /// Backfill and inspect conversation segments.
    Segments(SegmentsArgs),
    /// Resolve the current session or project's daemon-owned recall file path.
    Path,
    /// Print existing recall, or update it from user input.
    Recall(RecallArgs),
    /// Store a concise durable memory.
    Remember(RememberArgs),
}

#[derive(Debug, Parser)]
struct DaemonArgs {
    /// Explicit config file path, used by generated service definitions.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct IngestArgs {
    /// Override the Codex sessions root.
    #[arg(long)]
    codex_root: Option<PathBuf>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
}

#[derive(Debug, Parser)]
struct EvalArgs {
    #[command(subcommand)]
    command: EvalCommand,
}

#[derive(Debug, Subcommand)]
enum EvalCommand {
    /// Replay recall against historical turns and store eval results.
    Recall(EvalRecallArgs),
    /// List stored eval runs.
    List(EvalListArgs),
    /// Show one eval run and its results.
    Show(EvalShowArgs),
    /// Summarize recent recall eval quality.
    Summary(EvalSummaryArgs),
    /// Summarize eval outcomes by recalled memory.
    Memories(EvalMemoriesArgs),
    /// Requeue stale n/a recall evals that now have later turns.
    RequeueStale(EvalRequeueStaleArgs),
}

#[derive(Debug, Parser)]
struct EvalRecallArgs {
    /// Maximum turns to replay.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Restrict replay to one session id.
    #[arg(long)]
    session: Option<String>,
    /// Restrict replay to one completed turn ordinal. Requires --session.
    #[arg(long)]
    turn: Option<u64>,
    /// Disable remote LLM judging and record retrieval metadata only.
    #[arg(long)]
    no_judge: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct EvalListArgs {
    /// Maximum runs to show.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Only include eval runs with this id or newer.
    #[arg(long)]
    since_run: Option<i64>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct EvalShowArgs {
    /// Eval run id.
    run_id: i64,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct EvalSummaryArgs {
    /// Maximum recent runs to summarize.
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Only include eval runs with this id or newer.
    #[arg(long)]
    since_run: Option<i64>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct EvalMemoriesArgs {
    /// Maximum recent eval runs to inspect.
    #[arg(long, default_value_t = 200)]
    eval_limit: usize,
    /// Only include eval runs with this id or newer.
    #[arg(long)]
    since_run: Option<i64>,
    /// Maximum memories to show.
    #[arg(long, default_value_t = 25)]
    limit: usize,
    /// Sort mode for the memory diagnostics.
    #[arg(long, value_enum, default_value_t = EvalMemorySort::Mixed)]
    sort: EvalMemorySort,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct EvalRequeueStaleArgs {
    /// Maximum stale eval runs to requeue.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EvalMemorySort {
    Mixed,
    Low,
    Useful,
    Count,
}

#[derive(Debug, Parser)]
struct StatusArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct StatsArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
    /// Maximum recent eval runs to consider for usefulness metrics.
    #[arg(long, default_value_t = 1000)]
    eval_limit: usize,
    /// Include only turns, recall runs, and eval runs at or after this timestamp.
    #[arg(long)]
    since: Option<String>,
    /// Include only recall/eval records from this origin. Repeatable.
    #[arg(long)]
    origin: Vec<String>,
    /// Exclude recall/eval records from this origin. Repeatable.
    #[arg(long)]
    exclude_origin: Vec<String>,
}

#[derive(Debug, Parser)]
struct ConfigArgs {
    /// Print the merged user and project configuration.
    #[arg(long)]
    effective: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct TasksArgs {
    #[command(subcommand)]
    command: TasksCommand,
}

#[derive(Debug, Parser)]
struct MemoriesArgs {
    #[command(subcommand)]
    command: MemoriesCommand,
}

#[derive(Debug, Parser)]
struct SegmentsArgs {
    #[command(subcommand)]
    command: SegmentsCommand,
}

#[derive(Debug, Subcommand)]
enum SegmentsCommand {
    /// Rebuild deterministic segment metadata from stored transcript cursors.
    Backfill(SegmentsBackfillArgs),
    /// List stored conversation segments.
    List(SegmentsListArgs),
}

#[derive(Debug, Parser)]
struct SegmentsBackfillArgs {
    /// Restrict backfill to one session id.
    #[arg(long)]
    session: Option<String>,
    /// Maximum sessions to process. Omit or set 0 for all matching sessions.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct SegmentsListArgs {
    /// Restrict output to one session id.
    #[arg(long)]
    session: Option<String>,
    /// Maximum segments to show.
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum MemoriesCommand {
    /// Show memory counts by scope, kind, and project.
    Stats(MemoriesStatsArgs),
    /// List stored memories with optional filters.
    List(MemoriesListArgs),
    /// Diagnose recall performance failure modes by memory.
    Health(MemoriesHealthArgs),
    /// Soft-deactivate memories with high-confidence bad health recommendations.
    ApplyHealth(MemoriesApplyHealthArgs),
    /// Soft-deactivate active memories and requeue transcript-backed formulation.
    Rebuild(MemoriesRebuildArgs),
}

#[derive(Debug, Parser)]
struct MemoriesStatsArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct MemoriesListArgs {
    /// Maximum memories to show.
    #[arg(long, default_value_t = 25)]
    limit: usize,
    /// Include inactive memories.
    #[arg(long)]
    include_inactive: bool,
    /// Restrict to one memory kind.
    #[arg(long)]
    kind: Option<String>,
    /// Restrict to one project id.
    #[arg(long)]
    project: Option<String>,
    /// Case-insensitive text search over title/body/task keys/project descriptor.
    #[arg(long)]
    query: Option<String>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct MemoriesHealthArgs {
    /// Maximum recent eval runs to inspect.
    #[arg(long, default_value_t = 1000)]
    eval_limit: usize,
    /// Maximum diagnosed memories to show.
    #[arg(long, default_value_t = 25)]
    limit: usize,
    /// Include inactive memories in the ranked output.
    #[arg(long)]
    include_inactive: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct MemoriesApplyHealthArgs {
    /// Maximum recent eval runs to inspect.
    #[arg(long, default_value_t = 1000)]
    eval_limit: usize,
    /// Maximum diagnosed memories to consider for application.
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Required confirmation before changing active memories.
    #[arg(long)]
    yes: bool,
    /// Show eligible memories without deactivating them.
    #[arg(long)]
    dry_run: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct MemoriesRebuildArgs {
    /// Required confirmation for corpus rebuild.
    #[arg(long)]
    yes: bool,
    /// Keep active memories and only queue currently uncovered turns.
    #[arg(long)]
    keep_active: bool,
}

#[derive(Debug, Subcommand)]
enum TasksCommand {
    /// List task queue state.
    List(TaskListArgs),
    /// Retry a queued, running, or parked task immediately.
    Retry(TaskRetryArgs),
    /// Delete tasks that are no longer useful.
    Clear(TaskClearArgs),
}

#[derive(Debug, Parser)]
struct TaskListArgs {
    /// Filter by task display status.
    #[arg(long, value_enum)]
    status: Option<TaskDisplayStatusArg>,
    /// Maximum tasks to show.
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct TaskRetryArgs {
    /// Task id to retry.
    id: i64,
}

#[derive(Debug, Parser)]
struct TaskClearArgs {
    /// Clear one task id.
    #[arg(long)]
    id: Option<i64>,
    /// Clear all tasks with this display status.
    #[arg(long, value_enum)]
    status: Option<TaskDisplayStatusArg>,
    /// Optional task-kind filter when clearing by status.
    #[arg(long)]
    kind: Option<String>,
    /// Required when clearing more than one task.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TaskDisplayStatusArg {
    Queued,
    Scheduled,
    Running,
    Parked,
    Completed,
}

impl TaskDisplayStatusArg {
    fn as_filter(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Parked => "parked",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Parser)]
struct RecallArgs {
    /// User input to embed and search against stored memories. Omit to print existing recall.
    #[arg(long)]
    query: Option<String>,
    /// Recall source for eval segmentation.
    #[arg(long, value_enum)]
    origin: Option<RecallOriginArg>,
    /// Tool name for tool-triggered recall, e.g. Bash or apply_patch.
    #[arg(long)]
    tool_name: Option<String>,
    /// Codex tool-use id for tool-triggered recall.
    #[arg(long)]
    tool_use_id: Option<String>,
    /// Short command/input summary for tool-triggered recall.
    #[arg(long)]
    tool_input_summary: Option<String>,
    /// Raw tool input JSON; Bash/apply_patch commands are summarized automatically.
    #[arg(long)]
    tool_input_json: Option<String>,
    /// Emit Codex PreToolUse hook JSON with additionalContext.
    #[arg(long)]
    codex_hook_output: bool,
    /// Session id to replay recall for.
    #[arg(long)]
    session: Option<String>,
    /// Completed turn ordinal to use as the recall anchor.
    #[arg(long)]
    turn: Option<u64>,
    /// Agent turn id to use as a deferred eval anchor.
    #[arg(long)]
    turn_id: Option<String>,
    /// Do not queue a recall eval for this query.
    #[arg(long)]
    no_eval: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
    /// Include per-memory ranking components in JSON output.
    #[arg(long)]
    debug_ranking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RecallOriginArg {
    SessionBackground,
    ManualQuery,
    ToolPreUse,
}

impl RecallOriginArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionBackground => "session_background",
            Self::ManualQuery => "manual_query",
            Self::ToolPreUse => "tool_pre_use",
        }
    }
}

#[derive(Debug, Parser)]
struct RememberArgs {
    /// Short title for the memory.
    #[arg(long)]
    title: String,
    /// Concise durable lesson, preference, or problem-solving insight.
    #[arg(long)]
    body: String,
    /// Recall scope for this memory.
    #[arg(long, value_enum, default_value = "project")]
    scope: RememberScopeArg,
    /// Optional human-readable project origin override.
    #[arg(long)]
    project_descriptor: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RememberScopeArg {
    Project,
    Global,
}

impl From<RememberScopeArg> for MemoryScope {
    fn from(value: RememberScopeArg) -> Self {
        match value {
            RememberScopeArg::Project => Self::Project,
            RememberScopeArg::Global => Self::Global,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli
        .command
        .unwrap_or(Command::Status(StatusArgs { json: false }))
    {
        Command::Daemon(args) => daemon(args),
        Command::Init => init(),
        Command::Ingest(args) => ingest(args),
        Command::Service(args) => service(args),
        Command::Eval(args) => eval(args),
        Command::Status(args) => status(args),
        Command::Stats(args) => stats(args),
        Command::Config(args) => config(args),
        Command::Tasks(args) => tasks(args),
        Command::Memories(args) => memories(args),
        Command::Segments(args) => segments(args),
        Command::Path => path(),
        Command::Recall(args) => recall(args),
        Command::Remember(args) => remember(args),
    }
}

fn open_database_for_cwd() -> anyhow::Result<(Config, Database)> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    Ok((config, db))
}

fn status(args: StatusArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let status = db.status().context("failed to read status")?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_human_status(&status);
    }

    Ok(())
}

fn stats(args: StatsArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let since_unix = args.since.as_deref().map(parse_since_unix).transpose()?;
    let filters = yaaml::stats::StatsFilters::new(args.origin, args.exclude_origin, since_unix);
    let stats = yaaml::stats::build_stats_with_filters(&db, args.eval_limit, filters)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        yaaml::stats::print_human_stats(&stats);
    }
    Ok(())
}

fn parse_since_unix(value: &str) -> anyhow::Result<i64> {
    let trimmed = value.trim();
    let seconds = trimmed.strip_prefix("unix:").unwrap_or(trimmed);
    seconds.parse::<i64>().with_context(|| {
        format!("--since must be a Unix timestamp like unix:1782760000 or 1782760000, got {value}")
    })
}

fn tasks(args: TasksArgs) -> anyhow::Result<()> {
    match args.command {
        TasksCommand::List(args) => task_list(args),
        TasksCommand::Retry(args) => task_retry(args),
        TasksCommand::Clear(args) => task_clear(args),
    }
}

#[derive(Debug, Serialize)]
struct MemoryStatsOutput {
    total: usize,
    active: usize,
    by_scope_kind: Vec<MemoryScopeKindCount>,
    top_projects: Vec<MemoryProjectCount>,
}

#[derive(Debug, Serialize)]
struct MemoryScopeKindCount {
    scope: String,
    kind: String,
    active: bool,
    count: usize,
}

#[derive(Debug, Serialize)]
struct MemoryProjectCount {
    project_id: String,
    active_count: usize,
}

#[derive(Debug, Serialize)]
struct MemoryListEntry {
    memory_id: i64,
    title: String,
    body: String,
    scope: String,
    kind: String,
    active: bool,
    project_id: Option<String>,
    project_descriptor: Option<String>,
    task_keys: Vec<String>,
    created_at: String,
    updated_at: String,
    session_id: Option<String>,
    origin_segment_id: Option<i64>,
    origin_segment_status: Option<String>,
    validity: String,
}

#[derive(Debug, Serialize)]
struct MemoryRebuildOutput {
    deactivated_memories: u64,
    cleared_memory_tasks: u64,
    queued_memory_jobs: u64,
}

#[derive(Debug, Serialize)]
struct MemoryHealthOutput {
    eval_runs_considered: usize,
    result_rows_considered: usize,
    memories_considered: usize,
    active_memories_considered: usize,
    include_inactive: bool,
    failure_mode_counts: Vec<MemoryFailureModeCount>,
    recommendation_counts: Vec<MemoryRecommendationCount>,
    memories: Vec<MemoryHealthDiagnostic>,
}

#[derive(Debug, Serialize)]
struct MemoryHealthApplyOutput {
    eval_runs_considered: usize,
    result_rows_considered: usize,
    memories_considered: usize,
    active_memories_considered: usize,
    dry_run: bool,
    eligible_memories: usize,
    applied_memories: usize,
    memories: Vec<MemoryHealthAppliedMemory>,
}

#[derive(Debug, Serialize)]
struct MemoryHealthAppliedMemory {
    memory_id: i64,
    title: String,
    failure_mode: String,
    recommended_action: String,
    judged_count: u64,
    useful_count: u64,
    low_count: u64,
    selected_count: u64,
}

#[derive(Debug, Serialize)]
struct MemoryFailureModeCount {
    failure_mode: String,
    count: usize,
}

#[derive(Debug, Serialize)]
struct MemoryRecommendationCount {
    recommendation: String,
    count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryHealthDiagnostic {
    memory_id: i64,
    title: String,
    kind: String,
    scope: String,
    is_active: bool,
    project_id: Option<String>,
    selected_count: u64,
    judged_count: u64,
    useful_count: u64,
    low_count: u64,
    neutral_count: u64,
    insufficient_context_count: u64,
    low_rate: f64,
    useful_rate: f64,
    average_score: Option<f64>,
    failure_mode: String,
    recommended_action: String,
    evidence: Vec<String>,
    latest_low_rationale: Option<String>,
    latest_useful_rationale: Option<String>,
}

fn memories(args: MemoriesArgs) -> anyhow::Result<()> {
    match args.command {
        MemoriesCommand::Stats(args) => memories_stats(args),
        MemoriesCommand::List(args) => memories_list(args),
        MemoriesCommand::Health(args) => memories_health(args),
        MemoriesCommand::ApplyHealth(args) => memories_apply_health(args),
        MemoriesCommand::Rebuild(args) => memories_rebuild(args),
    }
}

fn segments(args: SegmentsArgs) -> anyhow::Result<()> {
    match args.command {
        SegmentsCommand::Backfill(args) => segments_backfill(args),
        SegmentsCommand::List(args) => segments_list(args),
    }
}

#[derive(Debug, Serialize)]
struct SegmentsBackfillOutput {
    sessions_processed: usize,
    segments_written: u64,
    failures: usize,
    failure_details: Vec<SegmentsBackfillFailure>,
}

#[derive(Debug, Serialize)]
struct SegmentsBackfillFailure {
    session_id: String,
    transcript_file_path: String,
    error: String,
}

fn segments_backfill(args: SegmentsBackfillArgs) -> anyhow::Result<()> {
    let (config, db) = open_database_for_cwd()?;
    let explicit_session = args.session.is_some();
    let sessions = if let Some(session_id) = args.session.as_deref() {
        vec![db
            .session_by_id(session_id)
            .context("failed to load session")?
            .with_context(|| format!("session {session_id} not found"))?]
    } else {
        db.sessions_with_completed_turn_counts()
            .context("failed to list sessions")?
            .into_iter()
            .map(|(session, _count)| session)
            .collect()
    };
    let limit = if args.limit == 0 {
        sessions.len()
    } else {
        args.limit.min(sessions.len())
    };
    let mut sessions_processed = 0_usize;
    let mut segments_written = 0_u64;
    let mut failure_details = Vec::new();
    for session in sessions.into_iter().take(limit) {
        match yaaml::daemon::refresh_conversation_segments_for_session(&db, &config, &session.id) {
            Ok(written) => {
                segments_written += written;
                sessions_processed += 1;
            }
            Err(error) if explicit_session => {
                return Err(error)
                    .with_context(|| format!("failed to backfill session {}", session.id));
            }
            Err(error) => {
                failure_details.push(SegmentsBackfillFailure {
                    session_id: session.id,
                    transcript_file_path: session.transcript_file_path,
                    error: format!("{error:#}"),
                });
            }
        }
    }

    let output = SegmentsBackfillOutput {
        sessions_processed,
        segments_written,
        failures: failure_details.len(),
        failure_details,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("YAAML segment backfill");
        println!("  sessions processed: {}", output.sessions_processed);
        println!("  segments written: {}", output.segments_written);
        println!("  failures: {}", output.failures);
        for failure in output.failure_details.iter().take(10) {
            println!(
                "    - session={} transcript={} error={}",
                failure.session_id, failure.transcript_file_path, failure.error
            );
        }
        if output.failure_details.len() > 10 {
            println!(
                "    ... {} more failures; rerun with --json for full details",
                output.failure_details.len() - 10
            );
        }
    }
    Ok(())
}

fn segments_list(args: SegmentsListArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let segments = db
        .list_conversation_segments(args.session.as_deref(), args.limit)
        .context("failed to list conversation segments")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&segments)?);
    } else {
        print_segments(&segments);
    }
    Ok(())
}

fn print_segments(segments: &[ConversationSegmentRecord]) {
    println!("Conversation segments");
    if segments.is_empty() {
        println!("  none");
        return;
    }
    for (index, segment) in segments.iter().enumerate() {
        let keys = if segment.task_keys.is_empty() {
            "-".to_string()
        } else {
            segment
                .task_keys
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  {}. session={} turns={}..={} status={} keys={}",
            index + 1,
            segment.session_id,
            segment.start_turn_ordinal,
            segment.end_turn_ordinal,
            segment.status.as_str(),
            keys
        );
        println!("     {}", segment.summary);
    }
}

fn memories_stats(args: MemoriesStatsArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let stats = build_memory_stats(&db)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        print_human_memory_stats(&stats);
    }
    Ok(())
}

fn memories_list(args: MemoriesListArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let mut memories = db.list_memories().context("failed to list memories")?;
    let kind_filter = args.kind.as_deref().map(str::trim).map(str::to_string);
    let query_filter = args
        .query
        .as_deref()
        .map(|query| query.to_ascii_lowercase());
    memories.retain(|memory| {
        if !args.include_inactive && !memory.is_active {
            return false;
        }
        if let Some(kind) = kind_filter.as_deref() {
            if memory.kind.as_str() != kind {
                return false;
            }
        }
        if let Some(project) = args.project.as_deref() {
            if memory.project_id.as_deref() != Some(project) {
                return false;
            }
        }
        if let Some(query) = query_filter.as_deref() {
            let haystack = format!(
                "{}\n{}\n{}\n{}",
                memory.title,
                memory.body,
                memory.task_keys.join("\n"),
                memory.project_descriptor.as_deref().unwrap_or("")
            )
            .to_ascii_lowercase();
            if !haystack.contains(query) {
                return false;
            }
        }
        true
    });
    memories.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    memories.truncate(args.limit);
    let entries = memories
        .into_iter()
        .map(memory_list_entry)
        .collect::<Vec<_>>();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        print_human_memory_list(&entries);
    }
    Ok(())
}

fn memory_list_entry(memory: MemoryRecord) -> MemoryListEntry {
    MemoryListEntry {
        memory_id: memory.id.unwrap_or_default(),
        title: memory.title,
        body: memory.body,
        scope: memory.scope.as_str().to_string(),
        kind: memory.kind.as_str().to_string(),
        active: memory.is_active,
        project_id: memory.project_id,
        project_descriptor: memory.project_descriptor,
        task_keys: memory.task_keys,
        created_at: memory.created_at,
        updated_at: memory.updated_at,
        session_id: memory.session_id,
        origin_segment_id: memory.origin_segment_id,
        origin_segment_status: memory
            .origin_segment_status
            .map(|status| status.as_str().to_string()),
        validity: memory.validity.as_str().to_string(),
    }
}

fn print_human_memory_list(memories: &[MemoryListEntry]) {
    println!("Memories");
    if memories.is_empty() {
        println!("  none");
        return;
    }
    for (index, memory) in memories.iter().enumerate() {
        let keys = if memory.task_keys.is_empty() {
            "-".to_string()
        } else {
            memory
                .task_keys
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  {}. id={} kind={} scope={} active={} updated={}",
            index + 1,
            memory.memory_id,
            memory.kind,
            memory.scope,
            memory.active,
            memory.updated_at
        );
        println!("     title={}", memory.title);
        println!(
            "     project={} keys={}",
            memory.project_id.as_deref().unwrap_or("-"),
            keys
        );
    }
}

fn memories_health(args: MemoriesHealthArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let runs = db
        .list_eval_runs(args.eval_limit)
        .context("failed to list eval runs")?;
    let health = build_memory_health(&db, runs, args.include_inactive, args.limit)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&health)?);
    } else {
        print_human_memory_health(&health);
    }
    Ok(())
}

fn memories_apply_health(args: MemoriesApplyHealthArgs) -> anyhow::Result<()> {
    if args.yes && args.dry_run {
        bail!("use either --yes or --dry-run, not both");
    }
    if !args.yes && !args.dry_run {
        bail!("memory health application requires --yes or --dry-run");
    }
    let (_config, db) = open_database_for_cwd()?;
    let runs = db
        .list_eval_runs(args.eval_limit)
        .context("failed to list eval runs")?;
    let health = build_memory_health(&db, runs, false, args.limit)?;
    let actionable = health
        .memories
        .iter()
        .filter(|memory| should_apply_memory_health_action(memory))
        .collect::<Vec<_>>();
    let now = unix_timestamp();
    let mut applied_memories = Vec::new();
    for memory in actionable {
        if args.yes {
            db.deactivate_memory(memory.memory_id, &now)
                .with_context(|| format!("failed to deactivate memory {}", memory.memory_id))?;
        }
        applied_memories.push(MemoryHealthAppliedMemory {
            memory_id: memory.memory_id,
            title: memory.title.clone(),
            failure_mode: memory.failure_mode.clone(),
            recommended_action: memory.recommended_action.clone(),
            judged_count: memory.judged_count,
            useful_count: memory.useful_count,
            low_count: memory.low_count,
            selected_count: memory.selected_count,
        });
    }
    let output = MemoryHealthApplyOutput {
        eval_runs_considered: health.eval_runs_considered,
        result_rows_considered: health.result_rows_considered,
        memories_considered: health.memories_considered,
        active_memories_considered: health.active_memories_considered,
        dry_run: args.dry_run,
        eligible_memories: applied_memories.len(),
        applied_memories: if args.yes { applied_memories.len() } else { 0 },
        memories: applied_memories,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_human_memory_health_apply(&output);
    }
    Ok(())
}

fn memories_rebuild(args: MemoriesRebuildArgs) -> anyhow::Result<()> {
    if !args.yes {
        bail!("memory rebuild requires --yes");
    }
    let (config, db) = open_database_for_cwd()?;
    let now = unix_timestamp();
    let deactivated_memories = if args.keep_active {
        0
    } else {
        db.deactivate_active_memories(&now)
            .context("failed to deactivate active memories")?
    };
    let cleared_memory_tasks = clear_memory_build_tasks(&db)?;
    let queued_memory_jobs = yaaml::daemon::queue_missing_memory_formulation_tasks(
        &db,
        &config,
        0,
        yaaml::daemon::PartialBatchPolicy::Include,
    )
    .context("failed to queue memory formulation rebuild")?;
    let output = MemoryRebuildOutput {
        deactivated_memories,
        cleared_memory_tasks,
        queued_memory_jobs,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn clear_memory_build_tasks(db: &Database) -> anyhow::Result<u64> {
    let mut cleared = 0;
    for status in ["queued", "scheduled", "running", "parked"] {
        for kind in [
            yaaml::daemon::TASK_KIND_MEMORY_FORMULATION,
            yaaml::daemon::TASK_KIND_MEMORY_CONSOLIDATION,
        ] {
            cleared += db
                .clear_tasks_by_display_status(status, Some(kind))
                .with_context(|| format!("failed to clear {status} {kind} tasks"))?;
        }
    }
    Ok(cleared)
}

fn build_memory_stats(db: &Database) -> anyhow::Result<MemoryStatsOutput> {
    let memories = db.list_memories().context("failed to list memories")?;
    let mut by_scope_kind = BTreeMap::<(String, String, bool), usize>::new();
    let mut by_project = BTreeMap::<String, usize>::new();
    let mut active = 0;
    for memory in &memories {
        if memory.is_active {
            active += 1;
            if let Some(project_id) = &memory.project_id {
                *by_project.entry(project_id.clone()).or_default() += 1;
            }
        }
        *by_scope_kind
            .entry((
                memory.scope.as_str().to_string(),
                memory.kind.as_str().to_string(),
                memory.is_active,
            ))
            .or_default() += 1;
    }
    let mut top_projects = by_project
        .into_iter()
        .map(|(project_id, active_count)| MemoryProjectCount {
            project_id,
            active_count,
        })
        .collect::<Vec<_>>();
    top_projects.sort_by(|left, right| {
        right
            .active_count
            .cmp(&left.active_count)
            .then_with(|| left.project_id.cmp(&right.project_id))
    });
    top_projects.truncate(20);
    Ok(MemoryStatsOutput {
        total: memories.len(),
        active,
        by_scope_kind: by_scope_kind
            .into_iter()
            .map(|((scope, kind, active), count)| MemoryScopeKindCount {
                scope,
                kind,
                active,
                count,
            })
            .collect(),
        top_projects,
    })
}

#[derive(Debug, Default)]
struct MemoryHealthAccumulator {
    memory_id: i64,
    selected_count: u64,
    judged_scores: Vec<u8>,
    useful_count: u64,
    low_count: u64,
    neutral_count: u64,
    insufficient_context_count: u64,
    latest_low_rationale: Option<String>,
    latest_useful_rationale: Option<String>,
}

fn build_memory_health(
    db: &Database,
    runs: Vec<EvalRunRecord>,
    include_inactive: bool,
    limit: usize,
) -> anyhow::Result<MemoryHealthOutput> {
    let mut accumulators = BTreeMap::<i64, MemoryHealthAccumulator>::new();
    let mut result_rows_considered = 0_usize;

    for run in &runs {
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        for result in results {
            let Some(memory_id) = result.memory_id else {
                continue;
            };
            result_rows_considered += 1;
            let accumulator =
                accumulators
                    .entry(memory_id)
                    .or_insert_with(|| MemoryHealthAccumulator {
                        memory_id,
                        ..MemoryHealthAccumulator::default()
                    });
            accumulator.selected_count += 1;
            match result.judge_score.as_deref() {
                Some("insufficient_context") => accumulator.insufficient_context_count += 1,
                Some(score) => {
                    if let Some(numeric_score) = numeric_eval_score(score) {
                        accumulator.judged_scores.push(numeric_score);
                        if numeric_score >= 4 {
                            accumulator.useful_count += 1;
                            if let Some(rationale) = result.rationale.as_deref() {
                                accumulator.latest_useful_rationale =
                                    Some(eval_summary_snippet(rationale));
                            }
                        } else if numeric_score <= 2 {
                            accumulator.low_count += 1;
                            if let Some(rationale) = result.rationale.as_deref() {
                                accumulator.latest_low_rationale =
                                    Some(eval_summary_snippet(rationale));
                            }
                        } else {
                            accumulator.neutral_count += 1;
                        }
                    }
                }
                None => {}
            }
        }
    }

    let memory_ids = accumulators.keys().copied().collect::<Vec<_>>();
    let memory_by_id = db
        .list_memories_by_ids(&memory_ids)
        .context("failed to load memories for health diagnostics")?
        .into_iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();

    let active_memories_considered = memory_by_id
        .values()
        .filter(|memory| memory.is_active)
        .count();
    let mut diagnostics = accumulators
        .into_values()
        .filter_map(|accumulator| {
            let memory = memory_by_id.get(&accumulator.memory_id)?;
            if !include_inactive && !memory.is_active {
                return None;
            }
            Some(memory_health_diagnostic(accumulator, memory))
        })
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        health_severity(right)
            .cmp(&health_severity(left))
            .then_with(|| right.selected_count.cmp(&left.selected_count))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    let failure_mode_counts = count_failure_modes(&diagnostics);
    let recommendation_counts = count_recommendations(&diagnostics);
    diagnostics.truncate(limit);

    Ok(MemoryHealthOutput {
        eval_runs_considered: runs.len(),
        result_rows_considered,
        memories_considered: memory_ids.len(),
        active_memories_considered,
        include_inactive,
        failure_mode_counts,
        recommendation_counts,
        memories: diagnostics,
    })
}

fn memory_health_diagnostic(
    accumulator: MemoryHealthAccumulator,
    memory: &MemoryRecord,
) -> MemoryHealthDiagnostic {
    let judged_count = accumulator.judged_scores.len() as u64;
    let average_score = if accumulator.judged_scores.is_empty() {
        None
    } else {
        Some(
            accumulator
                .judged_scores
                .iter()
                .map(|score| f64::from(*score))
                .sum::<f64>()
                / accumulator.judged_scores.len() as f64,
        )
    };
    let low_rate = ratio(accumulator.low_count, judged_count);
    let useful_rate = ratio(accumulator.useful_count, judged_count);
    let mut evidence = memory_health_evidence(memory, &accumulator, low_rate, useful_rate);
    let failure_mode = diagnose_memory_failure(memory, &accumulator, low_rate, useful_rate);
    let recommended_action = recommended_memory_action(&failure_mode);
    evidence.insert(
        0,
        format!(
            "judged={} useful={} low={} low_rate={:.1}% useful_rate={:.1}%",
            judged_count,
            accumulator.useful_count,
            accumulator.low_count,
            low_rate * 100.0,
            useful_rate * 100.0
        ),
    );

    MemoryHealthDiagnostic {
        memory_id: accumulator.memory_id,
        title: memory.title.clone(),
        kind: memory.kind.as_str().to_string(),
        scope: memory.scope.as_str().to_string(),
        is_active: memory.is_active,
        project_id: memory.project_id.clone(),
        selected_count: accumulator.selected_count,
        judged_count,
        useful_count: accumulator.useful_count,
        low_count: accumulator.low_count,
        neutral_count: accumulator.neutral_count,
        insufficient_context_count: accumulator.insufficient_context_count,
        low_rate,
        useful_rate,
        average_score,
        failure_mode,
        recommended_action,
        evidence,
        latest_low_rationale: accumulator.latest_low_rationale,
        latest_useful_rationale: accumulator.latest_useful_rationale,
    }
}

fn diagnose_memory_failure(
    memory: &MemoryRecord,
    accumulator: &MemoryHealthAccumulator,
    low_rate: f64,
    useful_rate: f64,
) -> String {
    let judged_count = accumulator.judged_scores.len() as u64;
    let body_len = memory.body.chars().count();
    let task_key_count = memory.task_keys.len();
    let latest_low = accumulator
        .latest_low_rationale
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();

    if judged_count >= 5 && accumulator.useful_count == 0 && low_rate >= 0.70 {
        if looks_like_stale_task_state(memory, &latest_low) {
            return "stale_task_state".to_string();
        }
        if looks_stale_or_episodic(memory, &latest_low) {
            return "stale_episodic".to_string();
        }
        if rationale_mentions_wrong_context(&latest_low) {
            return "wrong_context".to_string();
        }
        if body_len < 300 {
            return "vague_under_contextualized".to_string();
        }
        if task_key_count >= 6 {
            return "noisy_metadata".to_string();
        }
        return "consistently_low_value".to_string();
    }

    if judged_count >= 5 && useful_rate >= 0.70 {
        return "proven_useful".to_string();
    }

    if accumulator.useful_count > 0 && accumulator.low_count > 0 {
        if rationale_mentions_wrong_context(&latest_low)
            && matches!(memory.kind, MemoryKind::Lesson | MemoryKind::Workflow)
        {
            return "context_sensitive_wrong_context".to_string();
        }
        if rationale_mentions_wrong_context(&latest_low)
            || memory.kind == MemoryKind::TaskState
            || memory.kind == MemoryKind::TaskCheckpoint
            || memory.kind == MemoryKind::ProjectFact
        {
            return "context_sensitive".to_string();
        }
        return "mixed_performance".to_string();
    }

    if judged_count > 0 && low_rate >= 0.70 {
        if body_len < 300 {
            return "vague_under_contextualized".to_string();
        }
        if task_key_count >= 6 {
            return "noisy_metadata".to_string();
        }
        return "likely_low_value".to_string();
    }

    if accumulator.insufficient_context_count > 0 && judged_count == 0 {
        return "insufficient_eval_context".to_string();
    }

    "unproven".to_string()
}

fn memory_health_evidence(
    memory: &MemoryRecord,
    accumulator: &MemoryHealthAccumulator,
    low_rate: f64,
    useful_rate: f64,
) -> Vec<String> {
    let mut evidence = Vec::new();
    let task_key_count = memory.task_keys.len();
    let body_len = memory.body.chars().count();
    if memory.project_id.as_deref() == Some(&home_dir_string()) {
        evidence.push("project_id is home root".to_string());
    }
    if task_key_count >= 6 {
        evidence.push(format!("many task keys ({task_key_count})"));
    } else if task_key_count == 0 {
        evidence.push("no task keys".to_string());
    }
    if body_len < 300 {
        evidence.push(format!("short body ({body_len} chars)"));
    }
    if matches!(
        memory.kind,
        MemoryKind::TaskState | MemoryKind::TaskCheckpoint | MemoryKind::ProjectFact
    ) {
        evidence.push(format!("episodic kind ({})", memory.kind.as_str()));
    }
    let latest_low = accumulator
        .latest_low_rationale
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if rationale_mentions_wrong_context(&latest_low) {
        evidence.push("latest low rationale says unrelated/wrong context".to_string());
    }
    if looks_stale_or_episodic(memory, &latest_low) {
        evidence.push("memory/rationale has stale episodic signals".to_string());
    }
    if accumulator.useful_count > 0 && accumulator.low_count > 0 {
        evidence.push(format!(
            "mixed eval outcomes: useful_rate={:.1}% low_rate={:.1}%",
            useful_rate * 100.0,
            low_rate * 100.0
        ));
    }
    evidence
}

fn recommended_memory_action(failure_mode: &str) -> String {
    match failure_mode {
        "wrong_context" => "regenerate_metadata_or_tighten_gates",
        "stale_task_state" => "move_to_dormant",
        "stale_episodic" => "move_to_dormant",
        "vague_under_contextualized" => "refine_or_suppress",
        "noisy_metadata" => "regenerate_task_keys",
        "consistently_low_value" => "suppress_or_tombstone",
        "context_sensitive_wrong_context" => "require_strong_task_match",
        "context_sensitive" => "require_stronger_context_match",
        "mixed_performance" => "context_sensitive_rerank",
        "likely_low_value" => "suppress_pending_more_evals",
        "insufficient_eval_context" => "re_eval_when_context_available",
        "proven_useful" => "boost_or_keep_active",
        _ => "keep_observing",
    }
    .to_string()
}

fn should_apply_memory_health_action(memory: &MemoryHealthDiagnostic) -> bool {
    if !memory.is_active || memory.judged_count < 5 || memory.useful_count != 0 {
        return false;
    }
    if memory.low_rate < 0.70 {
        return false;
    }
    let high_confidence_context_failure = memory.low_rate >= 0.90
        && matches!(
            memory.recommended_action.as_str(),
            "refine_or_suppress" | "regenerate_metadata_or_tighten_gates" | "regenerate_task_keys"
        );
    high_confidence_context_failure
        || matches!(
            memory.recommended_action.as_str(),
            "move_to_dormant" | "suppress_or_tombstone"
        )
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn rationale_mentions_wrong_context(rationale: &str) -> bool {
    [
        "unrelated",
        "wrong context",
        "different project",
        "different domain",
        "no connection",
        "no bearing",
        "irrelevant",
        "mismatch",
        "not directly actionable",
        "requires substantial reframing",
        "tangential",
    ]
    .iter()
    .any(|needle| rationale.contains(needle))
}

fn looks_stale_or_episodic(memory: &MemoryRecord, rationale: &str) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        memory.title.to_ascii_lowercase(),
        memory.body.to_ascii_lowercase(),
        rationale
    );
    looks_like_stale_task_state(memory, rationale)
        || [
            "stale",
            "obsolete",
            "old pr",
            "draft pr",
            "paused",
            "blocked",
            "remaining",
            "open questions",
            "completed",
            "rollout",
            "temporary",
            "current progress",
        ]
        .iter()
        .any(|needle| text.contains(needle))
}

fn looks_like_stale_task_state(memory: &MemoryRecord, rationale: &str) -> bool {
    matches!(
        memory.kind,
        MemoryKind::TaskState | MemoryKind::TaskCheckpoint
    ) && stale_task_state_signal(memory, rationale)
}

fn stale_task_state_signal(memory: &MemoryRecord, rationale: &str) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        memory.title.to_ascii_lowercase(),
        memory.body.to_ascii_lowercase(),
        rationale
    );
    [
        "stale",
        "obsolete",
        "old task",
        "past task",
        "previous task",
        "already resolved",
        "no longer",
        "current pr",
        "this pr",
        "branch",
        "queued",
        "parked",
        "status",
        "unrelated",
        "irrelevant",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn home_dir_string() -> String {
    env::var("HOME").unwrap_or_default()
}

fn health_severity(memory: &MemoryHealthDiagnostic) -> (u8, u64, u64) {
    let mode_rank = match memory.failure_mode.as_str() {
        "wrong_context" => 10,
        "stale_task_state" => 9,
        "stale_episodic" => 9,
        "noisy_metadata" => 8,
        "consistently_low_value" => 7,
        "vague_under_contextualized" => 6,
        "context_sensitive" => 5,
        "mixed_performance" => 4,
        "likely_low_value" => 3,
        "insufficient_eval_context" => 2,
        "unproven" => 1,
        "proven_useful" => 0,
        _ => 0,
    };
    (mode_rank, memory.low_count, memory.judged_count)
}

fn count_failure_modes(diagnostics: &[MemoryHealthDiagnostic]) -> Vec<MemoryFailureModeCount> {
    let mut counts = BTreeMap::<String, usize>::new();
    for diagnostic in diagnostics {
        *counts.entry(diagnostic.failure_mode.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(failure_mode, count)| MemoryFailureModeCount {
            failure_mode,
            count,
        })
        .collect()
}

fn count_recommendations(diagnostics: &[MemoryHealthDiagnostic]) -> Vec<MemoryRecommendationCount> {
    let mut counts = BTreeMap::<String, usize>::new();
    for diagnostic in diagnostics {
        *counts
            .entry(diagnostic.recommended_action.clone())
            .or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(recommendation, count)| MemoryRecommendationCount {
            recommendation,
            count,
        })
        .collect()
}

fn print_human_memory_health(health: &MemoryHealthOutput) {
    println!("Memory health diagnostics");
    println!("  eval runs: {}", health.eval_runs_considered);
    println!("  result rows: {}", health.result_rows_considered);
    println!(
        "  memories: {} considered ({} active)",
        health.memories_considered, health.active_memories_considered
    );
    println!("  include inactive: {}", health.include_inactive);
    println!("  failure modes:");
    for count in &health.failure_mode_counts {
        println!("    {}: {}", count.failure_mode, count.count);
    }
    println!("  recommendations:");
    for count in &health.recommendation_counts {
        println!("    {}: {}", count.recommendation, count.count);
    }
    for memory in &health.memories {
        let average_score = memory
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let status = if memory.is_active {
            "active"
        } else {
            "inactive"
        };
        println!(
            "  memory={} {} mode={} action={} avg={} selected={} useful={} low={} title={}",
            memory.memory_id,
            status,
            memory.failure_mode,
            memory.recommended_action,
            average_score,
            memory.selected_count,
            memory.useful_count,
            memory.low_count,
            memory.title
        );
        if let Some(project_id) = &memory.project_id {
            println!("    project={project_id}");
        }
        if !memory.evidence.is_empty() {
            println!("    evidence: {}", memory.evidence.join("; "));
        }
        if let Some(rationale) = &memory.latest_low_rationale {
            println!("    latest low rationale: {rationale}");
        }
    }
}

fn print_human_memory_health_apply(output: &MemoryHealthApplyOutput) {
    if output.dry_run {
        println!("Memory health application dry run");
    } else {
        println!("Memory health application");
    }
    println!("  eval runs: {}", output.eval_runs_considered);
    println!("  result rows: {}", output.result_rows_considered);
    println!(
        "  memories: {} considered ({} active)",
        output.memories_considered, output.active_memories_considered
    );
    println!("  eligible memories: {}", output.eligible_memories);
    println!("  applied memories: {}", output.applied_memories);
    for memory in &output.memories {
        println!(
            "  memory={} action={} mode={} selected={} useful={} low={} title={}",
            memory.memory_id,
            memory.recommended_action,
            memory.failure_mode,
            memory.selected_count,
            memory.useful_count,
            memory.low_count,
            memory.title
        );
    }
}

fn task_list(args: TaskListArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    let tasks = db
        .list_tasks(args.status.map(TaskDisplayStatusArg::as_filter), args.limit)
        .context("failed to list tasks")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&tasks)?);
    } else {
        print_human_tasks(&tasks);
    }
    Ok(())
}

fn task_retry(args: TaskRetryArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    if db
        .retry_task(args.id, &unix_timestamp())
        .context("failed to retry task")?
    {
        println!("retried task {}", args.id);
    } else {
        bail!("task {} was not found or is already completed", args.id);
    }
    Ok(())
}

fn task_clear(args: TaskClearArgs) -> anyhow::Result<()> {
    let (_config, db) = open_database_for_cwd()?;
    match (args.id, args.status) {
        (Some(id), None) => {
            if db.clear_task(id).context("failed to clear task")? {
                println!("cleared task {id}");
            } else {
                bail!("task {id} was not found");
            }
        }
        (None, Some(status)) => {
            if !args.yes {
                bail!("clearing by status requires --yes");
            }
            let cleared = db
                .clear_tasks_by_display_status(status.as_filter(), args.kind.as_deref())
                .context("failed to clear tasks")?;
            println!("cleared {cleared} {} tasks", status.as_filter());
        }
        _ => bail!("provide exactly one of --id or --status"),
    }
    Ok(())
}

fn config(args: ConfigArgs) -> anyhow::Result<()> {
    if !args.effective {
        bail!("only --effective is currently supported");
    }
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!("{}", toml::to_string_pretty(&config)?);
    }
    Ok(())
}

fn daemon(args: DaemonArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = load_config(&cwd, args.config)?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let data_dir = db_path
        .parent()
        .map(PathBuf::from)
        .context("db_path has no parent directory")?;
    let _lock = DaemonLock::acquire(&data_dir).context("failed to acquire daemon lock")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    yaaml::daemon::recover_running_tasks(&db)?;
    let remote_task_limit = config.backlog_max_concurrent_remote_jobs.max(1);
    let codex_root = yaaml_core::paths::home_dir()
        .context("failed to resolve HOME")?
        .join(".codex")
        .join("sessions");
    let report = yaaml::daemon::process_codex_backlog(&db, &config, &codex_root)?;
    yaaml::daemon::queue_memory_consolidation_if_due(&db, &config)
        .context("failed to queue memory consolidation")?;
    yaaml::daemon::queue_stale_recall_eval_tasks(&db, 10)
        .context("failed to queue stale recall evals")?;
    let completed_tasks = yaaml::daemon::run_queued_tasks(&db, &config, remote_task_limit)?;
    println!(
        "processed Codex backlog: {} files, {} turns, {} tasks, {} failures",
        report.processed_files, report.processed_turns, completed_tasks, report.failures
    );
    let shutdown = yaaml::daemon::DaemonShutdown::default();
    let socket = data_dir.join("daemon.sock");
    let signal_thread = yaaml::daemon::start_signal_socket(&socket, shutdown.clone())?;

    while !shutdown.is_requested() {
        let changes = yaaml::daemon::process_codex_changes(&db, &config, &codex_root)?;
        yaaml::daemon::queue_memory_consolidation_if_due(&db, &config)
            .context("failed to queue memory consolidation")?;
        yaaml::daemon::queue_stale_recall_eval_tasks(&db, 10)
            .context("failed to queue stale recall evals")?;
        let completed_tasks = yaaml::daemon::run_queued_tasks(&db, &config, remote_task_limit)?;
        if changes.changed_files > 0 || completed_tasks > 0 || changes.failures > 0 {
            println!(
                "processed Codex changes: {} files, {} turns, {} tasks, {} failures",
                changes.changed_files, changes.processed_turns, completed_tasks, changes.failures
            );
        }
        thread::sleep(Duration::from_secs(5));
    }
    signal_thread
        .join()
        .map_err(|_| anyhow::anyhow!("signal thread panicked"))??;
    Ok(())
}

fn ingest(args: IngestArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let codex_root = match args.codex_root {
        Some(path) => path,
        None => yaaml_core::paths::home_dir()
            .context("failed to resolve HOME")?
            .join(".codex")
            .join("sessions"),
    };
    let backlog_report = yaaml::daemon::process_codex_backlog(&db, &config, &codex_root)?;
    let change_report = yaaml::daemon::process_codex_changes(&db, &config, &codex_root)?;
    yaaml::daemon::queue_memory_consolidation_if_due(&db, &config)
        .context("failed to queue memory consolidation")?;
    yaaml::daemon::queue_stale_recall_eval_tasks(&db, 10)
        .context("failed to queue stale recall evals")?;
    let completed_tasks = yaaml::daemon::run_queued_tasks(
        &db,
        &config,
        config.backlog_max_concurrent_remote_jobs.max(1),
    )?;
    let processed_files = backlog_report.processed_files + change_report.changed_files;
    let processed_turns = backlog_report.processed_turns + change_report.processed_turns;
    let queued_memory_jobs = backlog_report.queued_memory_jobs + change_report.queued_memory_jobs;
    let failures = backlog_report.failures + change_report.failures;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "discovered_files": backlog_report.discovered_files,
                "scanned_files": change_report.scanned_files,
                "changed_files": change_report.changed_files,
                "processed_files": processed_files,
                "processed_turns": processed_turns,
                "queued_memory_jobs": queued_memory_jobs,
                "completed_tasks": completed_tasks,
                "failures": failures,
            }))?
        );
    } else {
        println!(
            "ingested {} files, {} turns, queued {} memory jobs, completed {} tasks, {} failures",
            processed_files, processed_turns, queued_memory_jobs, completed_tasks, failures
        );
    }
    Ok(())
}

fn init() -> anyhow::Result<()> {
    let home = yaaml_core::paths::home_dir().context("failed to resolve HOME")?;
    let binary = env::current_exe().context("failed to resolve current executable")?;
    let paths = yaaml::skills::InitPaths::for_home_with_binary(&home, binary);
    let report = yaaml::skills::init(&paths)?;

    println!(
        "installed Codex recall skill: {}",
        report.codex_skill.display()
    );
    println!(
        "installed Codex remember skill: {}",
        report.codex_remember_skill.display()
    );
    println!(
        "installed Claude recall skill: {}",
        report.claude_skill.display()
    );
    println!(
        "installed Claude remember skill: {}",
        report.claude_remember_skill.display()
    );
    println!("removed legacy Codex PreToolUse hook if present");
    Ok(())
}

fn service(args: ServiceArgs) -> anyhow::Result<()> {
    let home = yaaml_core::paths::home_dir().context("failed to resolve HOME")?;
    let binary = env::current_exe().context("failed to resolve current executable")?;
    let paths = yaaml::service::ServicePaths::for_home(&home, binary);

    match args.command {
        ServiceCommand::Install => {
            let report = yaaml::service::install(&paths)?;
            println!("installed service: {}", report.service_file.display());
        }
        ServiceCommand::Uninstall => yaaml::service::uninstall(&paths)?,
        ServiceCommand::Start => yaaml::service::start(&paths)?,
        ServiceCommand::Stop => yaaml::service::stop()?,
        ServiceCommand::Status => yaaml::service::status()?,
    }
    Ok(())
}

fn eval(args: EvalArgs) -> anyhow::Result<()> {
    match args.command {
        EvalCommand::Recall(args) => eval_recall(args),
        EvalCommand::List(args) => eval_list(args),
        EvalCommand::Show(args) => eval_show(args),
        EvalCommand::Summary(args) => eval_summary(args),
        EvalCommand::Memories(args) => eval_memories(args),
        EvalCommand::RequeueStale(args) => eval_requeue_stale(args),
    }
}

fn eval_recall(args: EvalRecallArgs) -> anyhow::Result<()> {
    if args.turn.is_some() && args.session.is_none() {
        bail!("--turn requires --session");
    }
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let now = unix_timestamp();
    let run_id = db
        .insert_eval_run_with_metadata(
            "default",
            &now,
            &serde_json::json!({
                "limit": args.limit,
                "session": args.session.as_deref(),
                "turn": args.turn,
                "judge_provider": config.eval_judge_provider,
                "judge_model": config.eval_judge_model,
                "judge_enabled": !args.no_judge,
                "retrieval": "current_active_memories_vector_or_lexical_fallback",
            })
            .to_string(),
            EvalRunMetadata {
                session_id: args.session.clone(),
                turn_ordinal: args.turn,
                recall_origin: "replay".to_string(),
                ..EvalRunMetadata::default()
            },
        )
        .context("failed to create eval run")?;
    let turns = eval_replay_turns(&db, &args).context("failed to load replay turns")?;
    let raw_turns = turns
        .iter()
        .map(|(_, turn)| turn.clone())
        .collect::<Vec<_>>();
    let hydrated_turns =
        hydrate_turns(&db, &raw_turns).context("failed to hydrate replay turns")?;
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(&config),
        ReqwestTransport::default(),
    );
    let vector_index =
        SqliteExactVectorIndex::new(&db, config.embedding_model.clone(), now.clone());
    let judge_client = eval_judge_client(&config, args.no_judge);
    let mut evaluated_memories = 0_u64;
    for ((turn_row_id, _), turn) in turns.iter().zip(hydrated_turns.iter()) {
        let query = turn.display_text.clone().unwrap_or_default();
        let fallback_project = turn.cwd.as_deref().unwrap_or("");
        let query_context = context_from_turns(
            std::slice::from_ref(turn),
            std::path::Path::new(fallback_project),
            &query,
        );
        let candidates =
            match production_recall_eval_candidates(&db, &config, &embedding_client, turn, &now)? {
                Some(candidates) => candidates,
                None => {
                    let memories = db
                        .list_memories()
                        .context("failed to load memories for replay turn")?
                        .into_iter()
                        .filter(|memory| memory.is_active)
                        .collect::<Vec<_>>();
                    select_eval_candidates(
                        &config,
                        &embedding_client,
                        &vector_index,
                        &query,
                        &query_context,
                        &memories,
                    )
                }
            };
        if candidates.is_empty() {
            db.insert_eval_result(
                run_id,
                *turn_row_id,
                None,
                "neutral",
                "no eligible recalled memories",
                &now,
            )
            .context("failed to insert eval result")?;
        } else {
            for candidate in candidates {
                evaluated_memories += 1;
                let citation_score = counterfactual_citation_score(&candidate.memory, turn);
                let (judge_score, rationale) =
                    judge_eval_candidate(judge_client.as_ref(), turn, &candidate, citation_score);
                db.insert_eval_result(
                    run_id,
                    *turn_row_id,
                    candidate.memory.id,
                    &judge_score,
                    &rationale,
                    &now,
                )
                .context("failed to insert eval result")?;
            }
        }
    }
    db.complete_eval_run(run_id, &unix_timestamp())
        .context("failed to complete eval run")?;

    if args.json {
        let results = db
            .eval_results_for_run(run_id)
            .context("failed to load eval results")?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": run_id,
                "evaluated_turns": turns.len(),
                "evaluated_memories": evaluated_memories,
                "score_counts": score_counts(results.iter().filter_map(|result| result.judge_score.as_deref())),
            }))?
        );
    } else {
        println!(
            "evaluated {} turns and {} recalled memories in run {}",
            turns.len(),
            evaluated_memories,
            run_id
        );
    }
    Ok(())
}

fn production_recall_eval_candidates(
    db: &Database,
    config: &Config,
    embedding_client: &OpenAiEmbeddingClient<ReqwestTransport>,
    turn: &TurnRecord,
    now: &str,
) -> anyhow::Result<Option<Vec<EvalCandidate>>> {
    let Some(session) = db
        .session_by_id(&turn.session_id)
        .context("failed to load replay session")?
    else {
        return Ok(Some(Vec::new()));
    };
    let window = u64::try_from(config.recall_live_turn_window).unwrap_or(u64::MAX);
    let start_ordinal = turn.ordinal.saturating_add(1).saturating_sub(window.max(1));
    let turns = db
        .completed_turns_for_session_range(
            &turn.session_id,
            start_ordinal,
            turn.ordinal.saturating_add(1),
        )
        .context("failed to load production replay recall turns")?;
    let turns = hydrate_turns(db, &turns).context("failed to hydrate production replay turns")?;
    if turns.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let recall_turns = active_segment_recall_turns(&turns);
    let query = yaaml_core::build_recall_query(
        recall_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    if query.trim().is_empty() {
        return Ok(Some(Vec::new()));
    }
    let query_context = context_from_turns(recall_turns, Path::new(&session.project_id), &query);
    let Ok(query_embedding) = embedding_client.embed(&query) else {
        return Ok(None);
    };
    let recall_result = recall_from_embedding(
        db,
        config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &session.project_id,
            session_id: Some(turn.session_id.as_str()),
            turn_ordinal: Some(turn.ordinal),
            query_text: &query,
            query_context: Some(query_context),
            query_source: "replay".to_string(),
            query_timestamp: now.to_string(),
            apply_cooldown: false,
        },
    )?;
    let selected_ids = recall_result.selected_memory_ids;
    if selected_ids.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let memories = db
        .list_active_memories_by_ids(&selected_ids)
        .context("failed to load production replay memories")?;
    let memory_by_id = memories
        .into_iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();
    let score_by_id = recall_result
        .memories
        .into_iter()
        .map(|memory| (memory.memory_id, memory.score))
        .collect::<HashMap<_, _>>();
    let candidates = selected_ids
        .into_iter()
        .enumerate()
        .filter_map(|(index, memory_id)| {
            let memory = memory_by_id.get(&memory_id)?.clone();
            Some(EvalCandidate {
                memory,
                rank: index + 1,
                retrieval_score: score_by_id.get(&memory_id).copied().unwrap_or_default(),
                retrieval_strategy: "production_recall",
            })
        })
        .collect::<Vec<_>>();
    Ok(Some(candidates))
}

fn eval_replay_turns(
    db: &Database,
    args: &EvalRecallArgs,
) -> anyhow::Result<Vec<(i64, TurnRecord)>> {
    match (args.session.as_deref(), args.turn) {
        (Some(session_id), Some(turn)) => {
            let turn = db
                .turn_with_id_for_session_ordinal(session_id, turn)
                .with_context(|| format!("failed to load turn {turn} for session {session_id}"))?
                .with_context(|| format!("turn {turn} for session {session_id} not found"))?;
            Ok(vec![turn])
        }
        (Some(session_id), None) => db
            .turns_for_session(session_id, args.limit)
            .with_context(|| format!("failed to load turns for session {session_id}"))?
            .into_iter()
            .map(|turn| {
                let row_id = db
                    .turn_row_id_for_session_ordinal(&turn.session_id, turn.ordinal)
                    .with_context(|| {
                        format!(
                            "failed to load row id for session {} turn {}",
                            turn.session_id, turn.ordinal
                        )
                    })?
                    .with_context(|| {
                        format!(
                            "row id for session {} turn {} not found",
                            turn.session_id, turn.ordinal
                        )
                    })?;
                Ok((row_id, turn))
            })
            .collect(),
        (None, None) => db
            .list_turns_with_ids(args.limit)
            .context("failed to load replay turns"),
        (None, Some(_)) => unreachable!("validated before eval replay turn loading"),
    }
}

fn eval_list(args: EvalListArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let runs = db
        .list_eval_runs_filtered(args.limit, args.since_run)
        .context("failed to list eval runs")?;
    let runs = runs.into_iter().map(EvalListRun::from).collect::<Vec<_>>();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
    } else if runs.is_empty() {
        println!("No eval runs");
    } else {
        println!("Eval runs");
        for run in runs {
            let completed = run.completed_at_human.as_deref().unwrap_or("running");
            let session = run.session_id.as_deref().unwrap_or("-");
            let turn = run
                .turn_ordinal
                .map(|ordinal| ordinal.to_string())
                .unwrap_or_else(|| "-".to_string());
            let score = run.score.as_deref().unwrap_or("-");
            let origin = run.recall_origin.as_str();
            let tool = run.tool_name.as_deref().unwrap_or("-");
            println!(
                "  {}  score={}  origin={}  tool={}  session={}  turn={}  started={}  completed={}  results={}",
                run.id,
                score,
                origin,
                tool,
                session,
                turn,
                run.started_at_human,
                completed,
                run.result_count
            );
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct EvalListRun {
    id: i64,
    strategy: String,
    started_at: String,
    started_at_human: String,
    completed_at: Option<String>,
    completed_at_human: Option<String>,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
    turn_id: Option<String>,
    recall_origin: String,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input_summary: Option<String>,
    injected: Option<bool>,
    score: Option<String>,
    result_count: u64,
}

impl From<EvalRunRecord> for EvalListRun {
    fn from(run: EvalRunRecord) -> Self {
        let started_at_human = human_timestamp(&run.started_at);
        let completed_at_human = run.completed_at.as_deref().map(human_timestamp);
        Self {
            id: run.id,
            strategy: run.strategy,
            started_at: run.started_at,
            started_at_human,
            completed_at: run.completed_at,
            completed_at_human,
            session_id: run.session_id,
            turn_ordinal: run.turn_ordinal,
            turn_id: run.agent_turn_id,
            recall_origin: run.recall_origin,
            tool_name: run.tool_name,
            tool_use_id: run.tool_use_id,
            tool_input_summary: run.tool_input_summary,
            injected: run.injected,
            score: run.score.map(display_eval_score),
            result_count: run.result_count,
        }
    }
}

#[derive(Debug, Serialize)]
struct EvalShowMemory {
    memory_id: i64,
    title: Option<String>,
    is_active: bool,
    project_id: Option<String>,
    project_descriptor: Option<String>,
}

fn display_eval_score(score: String) -> String {
    if score == "insufficient_context" {
        "n/a".to_string()
    } else {
        score
    }
}

fn eval_show(args: EvalShowArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let run = db
        .eval_run_by_id(args.run_id)
        .context("failed to load eval run")?
        .with_context(|| format!("eval run {} not found", args.run_id))?;
    let results = db
        .eval_results_for_run(args.run_id)
        .context("failed to load eval results")?;
    let run_context = eval_run_context(&run);
    let recalled_memories =
        recalled_memories_for_run(&db, &run_context).context("failed to load recalled memories")?;
    let later_completed_turns = later_completed_turn_count(&db, &run_context)
        .context("failed to count later completed turns")?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run,
                "score_counts": score_counts(results.iter().filter_map(|result| result.judge_score.as_deref())),
                "recalled_memories": recalled_memories,
                "later_completed_turns": later_completed_turns,
                "results": results,
            }))?
        );
    } else {
        println!(
            "Eval run {}  {}  started={}  completed={}  results={}",
            run.id,
            run.strategy,
            run.started_at,
            run.completed_at.as_deref().unwrap_or("running"),
            run.result_count
        );
        if let (Some(session_id), Some(turn_ordinal)) =
            (run_context.session_id.as_deref(), run_context.turn_ordinal)
        {
            println!("Anchor: session={session_id} turn={turn_ordinal}");
        }
        if let Some(later_completed_turns) = later_completed_turns {
            println!("Later completed turns: {later_completed_turns}");
        }
        if !recalled_memories.is_empty() {
            println!("Recalled memories:");
            for memory in &recalled_memories {
                let title = memory.title.as_deref().unwrap_or("missing");
                let active = if memory.is_active {
                    "active"
                } else {
                    "inactive"
                };
                println!("  memory={} {} title={}", memory.memory_id, active, title);
            }
        }
        let counts = score_counts(
            results
                .iter()
                .filter_map(|result| result.judge_score.as_deref()),
        );
        if !counts.is_empty() {
            let rendered = counts
                .iter()
                .map(|(score, count)| format!("{score}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Scores: {rendered}");
        }
        for result in results {
            let title = result.memory_title.as_deref().unwrap_or("no memory");
            let score = result
                .judge_score
                .as_deref()
                .map(|score| display_eval_score(score.to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            println!(
                "  result={} turn={} memory={} score={} title={}",
                result.id,
                result.turn_id,
                result
                    .memory_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                score,
                title
            );
            if let Some(rationale) = result.rationale {
                println!("    {rationale}");
            }
        }
    }
    Ok(())
}

fn eval_summary(args: EvalSummaryArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let runs = db
        .list_eval_runs_filtered(args.limit, args.since_run)
        .context("failed to list eval runs")?;
    let summary = build_eval_summary(&db, runs)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_human_eval_summary(&summary);
    }
    Ok(())
}

fn eval_memories(args: EvalMemoriesArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let runs = db
        .list_eval_runs_filtered(args.eval_limit, args.since_run)
        .context("failed to list eval runs")?;
    let diagnostics = build_eval_memory_diagnostics(&db, runs, args.sort, args.limit)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&diagnostics)?);
    } else {
        print_human_eval_memory_diagnostics(&diagnostics);
    }
    Ok(())
}

fn eval_requeue_stale(args: EvalRequeueStaleArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let report = yaaml::daemon::queue_stale_recall_eval_tasks_report(&db, args.limit)
        .context("failed to requeue stale recall evals")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.queued == 0 {
        println!(
            "No stale recall evals queued (stale={}, already_scored={}, already_pending={}, missing_anchor={}, no_later_turns={}, empty_recall_text={})",
            report.stale_runs,
            report.skipped_already_scored,
            report.skipped_already_pending,
            report.skipped_missing_anchor,
            report.skipped_no_later_turns,
            report.skipped_empty_recall_text
        );
    } else {
        println!("Queued {} stale recall evals", report.queued);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct EvalMemoryDiagnostics {
    eval_runs_considered: usize,
    result_rows_considered: usize,
    memories_considered: usize,
    sort: String,
    memories: Vec<EvalMemoryDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalMemoryDiagnostic {
    memory_id: i64,
    title: Option<String>,
    kind: Option<String>,
    scope: Option<String>,
    is_active: Option<bool>,
    project_id: Option<String>,
    selected_count: u64,
    judged_count: u64,
    useful_count: u64,
    low_count: u64,
    neutral_count: u64,
    insufficient_context_count: u64,
    average_score: Option<f64>,
    mixed_useful_and_low: bool,
    latest_run_id: i64,
    latest_turn_ordinal: Option<u64>,
    latest_score: Option<String>,
    latest_rationale: Option<String>,
}

#[derive(Debug, Default)]
struct EvalMemoryAccumulator {
    memory_id: i64,
    selected_count: u64,
    judged_scores: Vec<u8>,
    useful_count: u64,
    low_count: u64,
    neutral_count: u64,
    insufficient_context_count: u64,
    latest_run_id: i64,
    latest_turn_ordinal: Option<u64>,
    latest_score: Option<String>,
    latest_rationale: Option<String>,
}

fn build_eval_memory_diagnostics(
    db: &Database,
    runs: Vec<EvalRunRecord>,
    sort: EvalMemorySort,
    limit: usize,
) -> anyhow::Result<EvalMemoryDiagnostics> {
    let mut accumulators = BTreeMap::<i64, EvalMemoryAccumulator>::new();
    let mut result_rows_considered = 0_usize;

    for run in &runs {
        let run_context = eval_run_context(run);
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        for result in results {
            let Some(memory_id) = result.memory_id else {
                continue;
            };
            result_rows_considered += 1;
            let accumulator =
                accumulators
                    .entry(memory_id)
                    .or_insert_with(|| EvalMemoryAccumulator {
                        memory_id,
                        latest_run_id: run.id,
                        latest_turn_ordinal: run_context.turn_ordinal,
                        ..EvalMemoryAccumulator::default()
                    });
            accumulator.selected_count += 1;
            if run.id >= accumulator.latest_run_id {
                accumulator.latest_run_id = run.id;
                accumulator.latest_turn_ordinal = run_context.turn_ordinal;
                accumulator.latest_score = result
                    .judge_score
                    .as_deref()
                    .map(|score| display_eval_score(score.to_string()));
                accumulator.latest_rationale =
                    result.rationale.as_deref().map(eval_summary_snippet);
            }
            match result.judge_score.as_deref() {
                Some("insufficient_context") => accumulator.insufficient_context_count += 1,
                Some(score) => {
                    if let Some(numeric_score) = numeric_eval_score(score) {
                        accumulator.judged_scores.push(numeric_score);
                        if numeric_score >= 4 {
                            accumulator.useful_count += 1;
                        } else if numeric_score <= 2 {
                            accumulator.low_count += 1;
                        } else {
                            accumulator.neutral_count += 1;
                        }
                    }
                }
                None => {}
            }
        }
    }

    let memory_ids = accumulators.keys().copied().collect::<Vec<_>>();
    let memory_by_id = db
        .list_memories_by_ids(&memory_ids)
        .context("failed to load eval memories")?
        .into_iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();
    let mut memories = accumulators
        .into_values()
        .map(|accumulator| {
            let memory_id = accumulator.memory_id;
            eval_memory_diagnostic(accumulator, memory_by_id.get(&memory_id))
        })
        .collect::<Vec<_>>();
    sort_eval_memory_diagnostics(&mut memories, sort);
    memories.truncate(limit);

    Ok(EvalMemoryDiagnostics {
        eval_runs_considered: runs.len(),
        result_rows_considered,
        memories_considered: memory_ids.len(),
        sort: eval_memory_sort_name(sort).to_string(),
        memories,
    })
}

fn eval_memory_diagnostic(
    accumulator: EvalMemoryAccumulator,
    memory: Option<&MemoryRecord>,
) -> EvalMemoryDiagnostic {
    let average_score = if accumulator.judged_scores.is_empty() {
        None
    } else {
        Some(
            accumulator
                .judged_scores
                .iter()
                .map(|score| f64::from(*score))
                .sum::<f64>()
                / accumulator.judged_scores.len() as f64,
        )
    };
    EvalMemoryDiagnostic {
        memory_id: accumulator.memory_id,
        title: memory.map(|memory| memory.title.clone()),
        kind: memory.map(|memory| format!("{:?}", memory.kind)),
        scope: memory.map(|memory| format!("{:?}", memory.scope)),
        is_active: memory.map(|memory| memory.is_active),
        project_id: memory.and_then(|memory| memory.project_id.clone()),
        selected_count: accumulator.selected_count,
        judged_count: accumulator.judged_scores.len() as u64,
        useful_count: accumulator.useful_count,
        low_count: accumulator.low_count,
        neutral_count: accumulator.neutral_count,
        insufficient_context_count: accumulator.insufficient_context_count,
        average_score,
        mixed_useful_and_low: accumulator.useful_count > 0 && accumulator.low_count > 0,
        latest_run_id: accumulator.latest_run_id,
        latest_turn_ordinal: accumulator.latest_turn_ordinal,
        latest_score: accumulator.latest_score,
        latest_rationale: accumulator.latest_rationale,
    }
}

fn sort_eval_memory_diagnostics(memories: &mut [EvalMemoryDiagnostic], sort: EvalMemorySort) {
    memories.sort_by(|left, right| {
        let ordering = match sort {
            EvalMemorySort::Mixed => (
                right.mixed_useful_and_low,
                right.useful_count.min(right.low_count),
                right.selected_count,
            )
                .cmp(&(
                    left.mixed_useful_and_low,
                    left.useful_count.min(left.low_count),
                    left.selected_count,
                )),
            EvalMemorySort::Low => {
                (right.low_count, right.selected_count).cmp(&(left.low_count, left.selected_count))
            }
            EvalMemorySort::Useful => (right.useful_count, right.selected_count)
                .cmp(&(left.useful_count, left.selected_count)),
            EvalMemorySort::Count => right.selected_count.cmp(&left.selected_count),
        };
        ordering.then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn eval_memory_sort_name(sort: EvalMemorySort) -> &'static str {
    match sort {
        EvalMemorySort::Mixed => "mixed",
        EvalMemorySort::Low => "low",
        EvalMemorySort::Useful => "useful",
        EvalMemorySort::Count => "count",
    }
}

fn print_human_eval_memory_diagnostics(diagnostics: &EvalMemoryDiagnostics) {
    println!("Eval memory diagnostics");
    println!("  eval runs: {}", diagnostics.eval_runs_considered);
    println!("  result rows: {}", diagnostics.result_rows_considered);
    println!("  memories: {}", diagnostics.memories_considered);
    println!("  sort: {}", diagnostics.sort);
    for memory in &diagnostics.memories {
        let average_score = memory
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let status = memory
            .is_active
            .map(|active| if active { "active" } else { "inactive" })
            .unwrap_or("missing");
        let title = memory.title.as_deref().unwrap_or("missing");
        println!(
            "  memory={} {} avg={} selected={} useful={} low={} neutral={} n/a={} mixed={} latest_run={} latest_score={} title={}",
            memory.memory_id,
            status,
            average_score,
            memory.selected_count,
            memory.useful_count,
            memory.low_count,
            memory.neutral_count,
            memory.insufficient_context_count,
            memory.mixed_useful_and_low,
            memory.latest_run_id,
            memory.latest_score.as_deref().unwrap_or("-"),
            title
        );
        if let Some(project_id) = &memory.project_id {
            println!("    project={project_id}");
        }
        if let Some(rationale) = &memory.latest_rationale {
            println!("    latest rationale: {rationale}");
        }
    }
}

#[derive(Debug, Serialize)]
struct EvalSummary {
    runs_considered: usize,
    results_considered: usize,
    judged_results: usize,
    average_score: Option<f64>,
    score_counts: BTreeMap<String, u64>,
    origin_breakdown: Vec<EvalSegmentSummary>,
    tool_breakdown: Vec<EvalSegmentSummary>,
    conversation_segment_breakdown: Vec<EvalConversationSegmentSummary>,
    session_breakdown: Vec<EvalSessionSummary>,
    stale_insufficient_context: Vec<EvalStaleInsufficientContext>,
    queued_recall_evals: Vec<EvalQueuedRecallTask>,
    low_score_examples: Vec<EvalSummaryExample>,
    high_score_examples: Vec<EvalSummaryExample>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalConversationSegmentSummary {
    session_id: Option<String>,
    start_turn_ordinal: Option<u64>,
    end_turn_ordinal: Option<u64>,
    summary: Option<String>,
    task_keys: Vec<String>,
    runs: usize,
    results: usize,
    judged_results: usize,
    average_score: Option<f64>,
    score_counts: BTreeMap<String, u64>,
    latest_run_id: i64,
}

#[derive(Debug, Clone, Serialize)]
struct EvalSegmentSummary {
    name: String,
    runs: usize,
    results: usize,
    judged_results: usize,
    average_score: Option<f64>,
    score_counts: BTreeMap<String, u64>,
    latest_run_id: i64,
}

#[derive(Debug, Clone, Serialize)]
struct EvalSessionSummary {
    session_id: Option<String>,
    project_id: Option<String>,
    runs: usize,
    results: usize,
    judged_results: usize,
    average_score: Option<f64>,
    score_counts: BTreeMap<String, u64>,
    latest_run_id: i64,
    latest_turn_ordinal: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalStaleInsufficientContext {
    run_id: i64,
    session_id: String,
    turn_ordinal: u64,
    score: String,
    later_completed_turns: u64,
    requeue_status: String,
}

#[derive(Debug, Clone, Serialize)]
struct EvalQueuedRecallTask {
    task_id: i64,
    status: String,
    attempts: u64,
    max_attempts: u64,
    next_run_at: Option<String>,
    next_run_at_human: Option<String>,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
    recall_origin: String,
    tool_name: Option<String>,
    injected: Option<bool>,
    memory_count: u64,
    tool_input_summary: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalSummaryExample {
    run_id: i64,
    result_id: i64,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
    recall_origin: String,
    tool_name: Option<String>,
    score: String,
    memory_id: Option<i64>,
    memory_title: Option<String>,
    rationale: Option<String>,
}

#[derive(Debug, Default)]
struct EvalSessionAccumulator {
    session_id: Option<String>,
    project_id: Option<String>,
    runs: usize,
    results: usize,
    numeric_scores: Vec<u8>,
    all_scores: Vec<String>,
    latest_run_id: i64,
    latest_turn_ordinal: Option<u64>,
}

#[derive(Debug, Default)]
struct EvalSegmentAccumulator {
    runs: usize,
    results: usize,
    numeric_scores: Vec<u8>,
    all_scores: Vec<String>,
    latest_run_id: i64,
}

#[derive(Debug, Default)]
struct EvalConversationSegmentAccumulator {
    session_id: Option<String>,
    start_turn_ordinal: Option<u64>,
    end_turn_ordinal: Option<u64>,
    summary: Option<String>,
    task_keys: Vec<String>,
    runs: usize,
    results: usize,
    numeric_scores: Vec<u8>,
    all_scores: Vec<String>,
    latest_run_id: i64,
}

fn build_eval_summary(db: &Database, runs: Vec<EvalRunRecord>) -> anyhow::Result<EvalSummary> {
    let mut all_scores = Vec::new();
    let mut numeric_scores = Vec::new();
    let mut low_score_examples = Vec::new();
    let mut high_score_examples = Vec::new();
    let mut stale_insufficient_context = Vec::new();
    let mut session_accumulators = BTreeMap::<String, EvalSessionAccumulator>::new();
    let mut origin_accumulators = BTreeMap::<String, EvalSegmentAccumulator>::new();
    let mut tool_accumulators = BTreeMap::<String, EvalSegmentAccumulator>::new();
    let mut conversation_segment_accumulators =
        BTreeMap::<String, EvalConversationSegmentAccumulator>::new();
    let mut results_considered = 0_usize;
    let mut runs_considered = 0_usize;

    for run in &runs {
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        let run_context = eval_run_context(run);
        let has_insufficient_context = results
            .iter()
            .any(|result| result.judge_score.as_deref() == Some("insufficient_context"));
        if has_insufficient_context
            && db
                .recall_eval_scored_rerun_exists(run.id)
                .with_context(|| format!("failed to check scored rerun for eval run {}", run.id))?
        {
            continue;
        }
        runs_considered += 1;
        let origin_key = run_context.recall_origin.clone();
        let origin_accumulator = origin_accumulators.entry(origin_key).or_default();
        origin_accumulator.runs += 1;
        if run.id > origin_accumulator.latest_run_id {
            origin_accumulator.latest_run_id = run.id;
        }
        let mut tool_accumulator = run_context
            .tool_name
            .clone()
            .map(|tool_name| tool_accumulators.entry(tool_name).or_default());
        if let Some(tool_accumulator) = tool_accumulator.as_mut() {
            tool_accumulator.runs += 1;
            if run.id > tool_accumulator.latest_run_id {
                tool_accumulator.latest_run_id = run.id;
            }
        }
        let segment_key = eval_conversation_segment_key(&run_context);
        let segment_accumulator = conversation_segment_accumulators
            .entry(segment_key)
            .or_insert_with(|| EvalConversationSegmentAccumulator {
                session_id: run_context.session_id.clone(),
                start_turn_ordinal: run_context.segment_start_turn_ordinal,
                end_turn_ordinal: run_context.segment_end_turn_ordinal,
                summary: run_context.segment_summary.clone(),
                task_keys: run_context.segment_task_keys.clone(),
                latest_run_id: run.id,
                ..EvalConversationSegmentAccumulator::default()
            });
        segment_accumulator.runs += 1;
        if run.id > segment_accumulator.latest_run_id {
            segment_accumulator.latest_run_id = run.id;
        }
        let session_key = run_context
            .session_id
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        let project_id = run_context
            .session_id
            .as_deref()
            .and_then(|session_id| db.session_by_id(session_id).ok().flatten())
            .map(|session| session.project_id);
        let accumulator =
            session_accumulators
                .entry(session_key)
                .or_insert_with(|| EvalSessionAccumulator {
                    session_id: run_context.session_id.clone(),
                    project_id,
                    latest_run_id: run.id,
                    latest_turn_ordinal: run_context.turn_ordinal,
                    ..EvalSessionAccumulator::default()
                });
        accumulator.runs += 1;
        if run.id > accumulator.latest_run_id {
            accumulator.latest_run_id = run.id;
            accumulator.latest_turn_ordinal = run_context.turn_ordinal;
        }

        if has_insufficient_context && stale_insufficient_context.len() < 10 {
            if let (Some(session_id), Some(turn_ordinal)) =
                (run_context.session_id.as_deref(), run_context.turn_ordinal)
            {
                let later_turn_count = later_completed_turn_count(db, &run_context)?.unwrap_or(0);
                if later_turn_count > 0 {
                    let requeue_status = stale_eval_requeue_status(db, &run_context)?.to_string();
                    stale_insufficient_context.push(EvalStaleInsufficientContext {
                        run_id: run.id,
                        session_id: session_id.to_string(),
                        turn_ordinal,
                        score: "n/a".to_string(),
                        later_completed_turns: later_turn_count,
                        requeue_status,
                    });
                }
            }
        }

        for result in results {
            results_considered += 1;
            accumulator.results += 1;
            origin_accumulator.results += 1;
            segment_accumulator.results += 1;
            if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                tool_accumulator.results += 1;
            }
            if let Some(score) = result.judge_score.as_deref() {
                all_scores.push(display_eval_score(score.to_string()));
                accumulator
                    .all_scores
                    .push(display_eval_score(score.to_string()));
                origin_accumulator
                    .all_scores
                    .push(display_eval_score(score.to_string()));
                segment_accumulator
                    .all_scores
                    .push(display_eval_score(score.to_string()));
                if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                    tool_accumulator
                        .all_scores
                        .push(display_eval_score(score.to_string()));
                }
                if let Some(numeric_score) = numeric_eval_score(score) {
                    numeric_scores.push(numeric_score);
                    accumulator.numeric_scores.push(numeric_score);
                    origin_accumulator.numeric_scores.push(numeric_score);
                    segment_accumulator.numeric_scores.push(numeric_score);
                    if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                        tool_accumulator.numeric_scores.push(numeric_score);
                    }
                    let example = eval_summary_example(&run_context, &result, score);
                    if numeric_score <= 2 && low_score_examples.len() < 5 {
                        low_score_examples.push(example);
                    } else if numeric_score >= 4 && high_score_examples.len() < 5 {
                        high_score_examples.push(example);
                    }
                }
            }
        }
    }

    let judged_results = numeric_scores.len();
    let average_score = if numeric_scores.is_empty() {
        None
    } else {
        Some(
            numeric_scores
                .iter()
                .map(|score| f64::from(*score))
                .sum::<f64>()
                / numeric_scores.len() as f64,
        )
    };
    let origin_breakdown = eval_segment_summaries(origin_accumulators);
    let tool_breakdown = eval_segment_summaries(tool_accumulators);
    let conversation_segment_breakdown =
        eval_conversation_segment_summaries(conversation_segment_accumulators);
    let mut session_breakdown = session_accumulators
        .into_values()
        .map(|accumulator| {
            let average_score = if accumulator.numeric_scores.is_empty() {
                None
            } else {
                Some(
                    accumulator
                        .numeric_scores
                        .iter()
                        .map(|score| f64::from(*score))
                        .sum::<f64>()
                        / accumulator.numeric_scores.len() as f64,
                )
            };
            EvalSessionSummary {
                session_id: accumulator.session_id,
                project_id: accumulator.project_id,
                runs: accumulator.runs,
                results: accumulator.results,
                judged_results: accumulator.numeric_scores.len(),
                average_score,
                score_counts: score_counts(accumulator.all_scores.iter().map(String::as_str)),
                latest_run_id: accumulator.latest_run_id,
                latest_turn_ordinal: accumulator.latest_turn_ordinal,
            }
        })
        .collect::<Vec<_>>();
    session_breakdown.sort_by_key(|session| std::cmp::Reverse(session.latest_run_id));
    let queued_recall_evals = db
        .list_recall_eval_tasks(20)
        .context("failed to list recall eval tasks")?
        .into_iter()
        .map(EvalQueuedRecallTask::from)
        .collect::<Vec<_>>();

    Ok(EvalSummary {
        runs_considered,
        results_considered,
        judged_results,
        average_score,
        score_counts: score_counts(all_scores.iter().map(String::as_str)),
        origin_breakdown,
        tool_breakdown,
        conversation_segment_breakdown,
        session_breakdown,
        stale_insufficient_context,
        queued_recall_evals,
        low_score_examples,
        high_score_examples,
    })
}

fn eval_segment_summaries(
    accumulators: BTreeMap<String, EvalSegmentAccumulator>,
) -> Vec<EvalSegmentSummary> {
    let mut summaries = accumulators
        .into_iter()
        .map(|(name, accumulator)| {
            let average_score = if accumulator.numeric_scores.is_empty() {
                None
            } else {
                Some(
                    accumulator
                        .numeric_scores
                        .iter()
                        .map(|score| f64::from(*score))
                        .sum::<f64>()
                        / accumulator.numeric_scores.len() as f64,
                )
            };
            EvalSegmentSummary {
                name,
                runs: accumulator.runs,
                results: accumulator.results,
                judged_results: accumulator.numeric_scores.len(),
                average_score,
                score_counts: score_counts(accumulator.all_scores.iter().map(String::as_str)),
                latest_run_id: accumulator.latest_run_id,
            }
        })
        .collect::<Vec<_>>();
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.latest_run_id));
    summaries
}

fn eval_conversation_segment_summaries(
    accumulators: BTreeMap<String, EvalConversationSegmentAccumulator>,
) -> Vec<EvalConversationSegmentSummary> {
    let mut summaries = accumulators
        .into_values()
        .map(|accumulator| {
            let average_score = if accumulator.numeric_scores.is_empty() {
                None
            } else {
                Some(
                    accumulator
                        .numeric_scores
                        .iter()
                        .map(|score| f64::from(*score))
                        .sum::<f64>()
                        / accumulator.numeric_scores.len() as f64,
                )
            };
            EvalConversationSegmentSummary {
                session_id: accumulator.session_id,
                start_turn_ordinal: accumulator.start_turn_ordinal,
                end_turn_ordinal: accumulator.end_turn_ordinal,
                summary: accumulator.summary,
                task_keys: accumulator.task_keys,
                runs: accumulator.runs,
                results: accumulator.results,
                judged_results: accumulator.numeric_scores.len(),
                average_score,
                score_counts: score_counts(accumulator.all_scores.iter().map(String::as_str)),
                latest_run_id: accumulator.latest_run_id,
            }
        })
        .collect::<Vec<_>>();
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.latest_run_id));
    summaries
}

fn eval_conversation_segment_key(run_context: &EvalRunContext) -> String {
    match (
        run_context.session_id.as_deref(),
        run_context.segment_start_turn_ordinal,
        run_context.segment_end_turn_ordinal,
    ) {
        (Some(session_id), Some(start), Some(end)) => format!("{session_id}:{start}-{end}"),
        (Some(session_id), None, None) => format!("{session_id}:unsegmented"),
        _ => "(unknown)".to_string(),
    }
}

#[derive(Debug, Clone)]
struct EvalRunContext {
    run_id: i64,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
    segment_start_turn_ordinal: Option<u64>,
    segment_end_turn_ordinal: Option<u64>,
    segment_summary: Option<String>,
    segment_task_keys: Vec<String>,
    memory_ids: Vec<i64>,
    rerun_for_eval_run_id: Option<i64>,
    recall_origin: String,
    tool_name: Option<String>,
}

fn eval_run_context(run: &EvalRunRecord) -> EvalRunContext {
    let config = serde_json::from_str::<serde_json::Value>(&run.config_json).ok();
    EvalRunContext {
        run_id: run.id,
        session_id: run.session_id.clone(),
        turn_ordinal: run.turn_ordinal,
        segment_start_turn_ordinal: run.segment_start_turn_ordinal,
        segment_end_turn_ordinal: run.segment_end_turn_ordinal,
        segment_summary: run.segment_summary.clone(),
        segment_task_keys: run.segment_task_keys.clone(),
        memory_ids: config
            .as_ref()
            .and_then(|value| value.get("memory_ids"))
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_i64)
                    .collect()
            })
            .unwrap_or_default(),
        rerun_for_eval_run_id: config
            .as_ref()
            .and_then(|value| value.get("rerun_for_eval_run_id"))
            .and_then(serde_json::Value::as_i64),
        recall_origin: run.recall_origin.clone(),
        tool_name: run.tool_name.clone(),
    }
}

fn stale_eval_requeue_status(
    db: &Database,
    run_context: &EvalRunContext,
) -> anyhow::Result<&'static str> {
    if db
        .recall_eval_scored_rerun_exists(run_context.run_id)
        .with_context(|| {
            format!(
                "failed to check scored rerun for eval run {}",
                run_context.run_id
            )
        })?
    {
        return Ok("already_scored");
    }
    if db
        .recall_eval_pending_rerun_exists(run_context.run_id)
        .with_context(|| {
            format!(
                "failed to check pending rerun for eval run {}",
                run_context.run_id
            )
        })?
    {
        return Ok("already_pending");
    }
    if run_context.rerun_for_eval_run_id.is_some() {
        return Ok("rerun_record");
    }
    if run_context.session_id.is_none() || run_context.turn_ordinal.is_none() {
        return Ok("missing_anchor");
    }
    if run_context.memory_ids.is_empty() {
        return Ok("empty_recall_text");
    }
    Ok("actionable")
}

fn recalled_memories_for_run(
    db: &Database,
    run_context: &EvalRunContext,
) -> anyhow::Result<Vec<EvalShowMemory>> {
    if run_context.memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let memories = db
        .list_memories_by_ids(&run_context.memory_ids)
        .context("failed to load memories by id")?;
    let memory_by_id = memories
        .into_iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();
    Ok(run_context
        .memory_ids
        .iter()
        .map(|memory_id| {
            let memory = memory_by_id.get(memory_id);
            EvalShowMemory {
                memory_id: *memory_id,
                title: memory.map(|memory| memory.title.clone()),
                is_active: memory.map(|memory| memory.is_active).unwrap_or(false),
                project_id: memory.and_then(|memory| memory.project_id.clone()),
                project_descriptor: memory.and_then(|memory| memory.project_descriptor.clone()),
            }
        })
        .collect())
}

fn later_completed_turn_count(
    db: &Database,
    run_context: &EvalRunContext,
) -> anyhow::Result<Option<u64>> {
    let (Some(session_id), Some(turn_ordinal)) =
        (run_context.session_id.as_deref(), run_context.turn_ordinal)
    else {
        return Ok(None);
    };
    let later_turns = db
        .completed_turns_for_session_after_ordinal(session_id, turn_ordinal, 10_000)
        .context("failed to load later completed turns")?;
    Ok(Some(later_turns.len() as u64))
}

impl From<RecallEvalTaskRecord> for EvalQueuedRecallTask {
    fn from(task: RecallEvalTaskRecord) -> Self {
        let next_run_at_human = task.next_run_at.as_deref().map(human_timestamp);
        Self {
            task_id: task.id,
            status: task.status,
            attempts: task.attempts,
            max_attempts: task.max_attempts,
            next_run_at: task.next_run_at,
            next_run_at_human,
            session_id: task.session_id,
            turn_ordinal: task.turn_ordinal,
            recall_origin: task
                .recall_origin
                .unwrap_or_else(|| "session_background".to_string()),
            tool_name: task.tool_name,
            injected: task.injected,
            memory_count: task.memory_count,
            tool_input_summary: task.tool_input_summary.as_deref().map(eval_summary_snippet),
            last_error: task.last_error,
        }
    }
}

fn eval_summary_example(
    run_context: &EvalRunContext,
    result: &EvalResultRecord,
    score: &str,
) -> EvalSummaryExample {
    EvalSummaryExample {
        run_id: run_context.run_id,
        result_id: result.id,
        session_id: run_context.session_id.clone(),
        turn_ordinal: run_context.turn_ordinal,
        recall_origin: run_context.recall_origin.clone(),
        tool_name: run_context.tool_name.clone(),
        score: score.to_string(),
        memory_id: result.memory_id,
        memory_title: result.memory_title.clone(),
        rationale: result.rationale.as_deref().map(eval_summary_snippet),
    }
}

fn eval_summary_snippet(text: &str) -> String {
    if text.chars().count() <= 240 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(237).collect::<String>())
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

fn print_human_eval_summary(summary: &EvalSummary) {
    println!("Eval summary");
    println!("  runs: {}", summary.runs_considered);
    println!("  results: {}", summary.results_considered);
    println!("  judged results: {}", summary.judged_results);
    match summary.average_score {
        Some(score) => println!("  average score: {score:.2}"),
        None => println!("  average score: n/a"),
    }
    if summary.score_counts.is_empty() {
        println!("  scores: none");
    } else {
        let rendered = summary
            .score_counts
            .iter()
            .map(|(score, count)| format!("{score}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  scores: {rendered}");
    }
    print_eval_segment_breakdown("Origin breakdown", &summary.origin_breakdown);
    print_eval_segment_breakdown("Tool breakdown", &summary.tool_breakdown);
    print_eval_conversation_segment_breakdown(&summary.conversation_segment_breakdown);
    print_eval_session_breakdown(&summary.session_breakdown);
    print_stale_insufficient_context(&summary.stale_insufficient_context);
    print_queued_recall_evals(&summary.queued_recall_evals);
    print_eval_summary_examples("Low-score examples", &summary.low_score_examples);
    print_eval_summary_examples("High-score examples", &summary.high_score_examples);
}

fn print_eval_segment_breakdown(label: &str, segments: &[EvalSegmentSummary]) {
    if segments.is_empty() {
        return;
    }
    println!("{label}");
    for segment in segments {
        let average_score = segment
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let rendered_scores = if segment.score_counts.is_empty() {
            "none".to_string()
        } else {
            segment
                .score_counts
                .iter()
                .map(|(score, count)| format!("{score}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  {} runs={} avg={} scores={} latest_run={}",
            segment.name, segment.runs, average_score, rendered_scores, segment.latest_run_id
        );
    }
}

fn print_eval_conversation_segment_breakdown(segments: &[EvalConversationSegmentSummary]) {
    if segments.is_empty() {
        return;
    }
    println!("Conversation segment breakdown");
    for segment in segments.iter().take(10) {
        let average_score = segment
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let rendered_scores = if segment.score_counts.is_empty() {
            "none".to_string()
        } else {
            segment
                .score_counts
                .iter()
                .map(|(score, count)| format!("{score}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let session_id = segment.session_id.as_deref().unwrap_or("-");
        let turn_range = match (segment.start_turn_ordinal, segment.end_turn_ordinal) {
            (Some(start), Some(end)) => format!("{start}..={end}"),
            _ => "-".to_string(),
        };
        let summary = segment
            .summary
            .as_deref()
            .map(eval_summary_snippet)
            .unwrap_or_else(|| "unsegmented".to_string());
        let task_keys = if segment.task_keys.is_empty() {
            "-".to_string()
        } else {
            segment
                .task_keys
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  session={} turns={} runs={} avg={} scores={} latest_run={}",
            session_id,
            turn_range,
            segment.runs,
            average_score,
            rendered_scores,
            segment.latest_run_id
        );
        println!("    summary: {summary}");
        println!("    keys: {task_keys}");
    }
}

fn print_eval_session_breakdown(sessions: &[EvalSessionSummary]) {
    if sessions.is_empty() {
        return;
    }
    println!("Session breakdown");
    for session in sessions.iter().take(10) {
        let session_id = session.session_id.as_deref().unwrap_or("-");
        let project_id = session.project_id.as_deref().unwrap_or("-");
        let average_score = session
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let rendered_scores = if session.score_counts.is_empty() {
            "none".to_string()
        } else {
            session
                .score_counts
                .iter()
                .map(|(score, count)| format!("{score}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  session={} project={} runs={} avg={} scores={} latest_run={} latest_turn={}",
            session_id,
            project_id,
            session.runs,
            average_score,
            rendered_scores,
            session.latest_run_id,
            session
                .latest_turn_ordinal
                .map(|ordinal| ordinal.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
}

fn print_stale_insufficient_context(stale: &[EvalStaleInsufficientContext]) {
    if stale.is_empty() {
        return;
    }
    println!("N/a evals needing attention");
    for eval in stale {
        println!(
            "  run={} status={} session={} turn={} later_turns={}",
            eval.run_id,
            eval.requeue_status,
            eval.session_id,
            eval.turn_ordinal,
            eval.later_completed_turns
        );
    }
}

fn print_queued_recall_evals(tasks: &[EvalQueuedRecallTask]) {
    if tasks.is_empty() {
        return;
    }
    println!("Queued recall evals");
    for task in tasks {
        println!(
            "  task={} status={} origin={} tool={} injected={} memories={} attempts={}/{} session={} turn={} next_run={}",
            task.task_id,
            task.status,
            task.recall_origin,
            task.tool_name.as_deref().unwrap_or("-"),
            task.injected
                .map(|injected| injected.to_string())
                .unwrap_or_else(|| "-".to_string()),
            task.memory_count,
            task.attempts,
            task.max_attempts,
            task.session_id.as_deref().unwrap_or("-"),
            task.turn_ordinal
                .map(|ordinal| ordinal.to_string())
                .unwrap_or_else(|| "-".to_string()),
            task.next_run_at_human
                .as_deref()
                .or(task.next_run_at.as_deref())
                .unwrap_or("-")
        );
        if let Some(error) = &task.last_error {
            println!("    {error}");
        }
        if let Some(summary) = &task.tool_input_summary {
            println!("    input: {summary}");
        }
    }
}

fn print_eval_summary_examples(label: &str, examples: &[EvalSummaryExample]) {
    if examples.is_empty() {
        return;
    }
    println!("{label}");
    for example in examples {
        let title = example.memory_title.as_deref().unwrap_or("no memory");
        let origin = example.recall_origin.as_str();
        let tool = example.tool_name.as_deref().unwrap_or("-");
        println!(
            "  run={} result={} score={} origin={} tool={} memory={} title={}",
            example.run_id,
            example.result_id,
            example.score,
            origin,
            tool,
            example
                .memory_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            title
        );
        if let Some(rationale) = &example.rationale {
            println!("    {rationale}");
        }
    }
}

#[derive(Debug, Clone)]
struct EvalCandidate {
    memory: MemoryRecord,
    rank: usize,
    retrieval_score: f32,
    retrieval_strategy: &'static str,
}

fn eval_judge_client(config: &Config, disabled: bool) -> Option<JudgeClient> {
    JudgeClient::from_config(config, disabled)
}

fn select_eval_candidates(
    config: &Config,
    embedding_client: &OpenAiEmbeddingClient<ReqwestTransport>,
    vector_index: &SqliteExactVectorIndex<'_>,
    query: &str,
    query_context: &ContextMetadata,
    eligible_memories: &[MemoryRecord],
) -> Vec<EvalCandidate> {
    if eligible_memories.is_empty() {
        return Vec::new();
    }
    if let Ok(embedding) = embedding_client.embed(query) {
        let eligible_ids = eligible_memories
            .iter()
            .filter_map(|memory| memory.id)
            .collect::<HashSet<_>>();
        let search_limit = eligible_ids.len().max(config.recall_candidate_pool);
        if let Ok(hits) = vector_index.search(&embedding, search_limit, 0.0) {
            let mut candidates = hits
                .into_iter()
                .filter(|hit| eligible_ids.contains(&hit.memory_id))
                .filter_map(|hit| {
                    eligible_memories
                        .iter()
                        .find(|memory| memory.id == Some(hit.memory_id))
                        .map(|memory| {
                            let memory_context = infer_context_from_memory(memory);
                            (
                                memory.clone(),
                                hit.similarity + context_score(query_context, &memory_context),
                            )
                        })
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|(left_memory, left_score), (right_memory, right_score)| {
                right_score
                    .partial_cmp(left_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| right_memory.created_at.cmp(&left_memory.created_at))
                    .then_with(|| right_memory.id.cmp(&left_memory.id))
            });
            let candidates = candidates
                .into_iter()
                .take(config.recall_result_limit)
                .enumerate()
                .map(|(index, (memory, score))| EvalCandidate {
                    memory,
                    rank: index + 1,
                    retrieval_score: score,
                    retrieval_strategy: "vector_context",
                })
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return candidates;
            }
        }
    }
    lexical_eval_candidates(config, query, query_context, eligible_memories)
}

fn lexical_eval_candidates(
    config: &Config,
    query: &str,
    query_context: &ContextMetadata,
    eligible_memories: &[MemoryRecord],
) -> Vec<EvalCandidate> {
    let query_terms = terms(query);
    let mut candidates = eligible_memories
        .iter()
        .cloned()
        .map(|memory| {
            let memory_text = format!("{} {}", memory.title, memory.body);
            let memory_context = infer_context_from_memory(&memory);
            let score = lexical_score(&query_terms, &memory_text)
                + context_score(query_context, &memory_context);
            (memory, score)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_memory, left_score), (right_memory, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right_memory.created_at.cmp(&left_memory.created_at))
            .then_with(|| right_memory.id.cmp(&left_memory.id))
    });
    candidates
        .into_iter()
        .take(config.recall_result_limit)
        .enumerate()
        .map(|(index, (memory, score))| EvalCandidate {
            memory,
            rank: index + 1,
            retrieval_score: score,
            retrieval_strategy: "lexical",
        })
        .collect()
}

fn terms(text: &str) -> HashSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3)
        .collect()
}

fn lexical_score(query_terms: &HashSet<String>, text: &str) -> f32 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let text_terms = terms(text);
    let overlap = query_terms
        .iter()
        .filter(|term| text_terms.contains(*term))
        .count();
    overlap as f32 / query_terms.len() as f32
}

fn judge_eval_candidate(
    judge_client: Option<&JudgeClient>,
    turn: &TurnRecord,
    candidate: &EvalCandidate,
    citation_score: &str,
) -> (String, String) {
    let retrieval_metadata = format!(
        "retrieval_strategy={}; rank={}; retrieval_score={:.4}; counterfactual_citation={}",
        candidate.retrieval_strategy, candidate.rank, candidate.retrieval_score, citation_score
    );
    let Some(client) = judge_client else {
        return ("unjudged".to_string(), retrieval_metadata);
    };
    let prompt = eval_judge_prompt(turn, candidate, citation_score);
    match client.structured_json(eval_judge_system_prompt(), &prompt) {
        Ok(value) => {
            let outcome = parse_eval_judge_response(&value);
            (
                outcome.score,
                format!(
                    "{retrieval_metadata}; judge_rationale={}",
                    outcome.rationale
                ),
            )
        }
        Err(error) => (
            "unjudged".to_string(),
            format!("{retrieval_metadata}; judge_error={error}"),
        ),
    }
}

fn eval_judge_system_prompt() -> &'static str {
    concat!(
        "Rate whether recalled context helped an AI coding agent after it was incorporated into the conversation. ",
        "Return only JSON with fields score and rationale. score must be a string from \"1\" to \"5\". ",
        "5: recalled context was relevant, concise, and actionable. ",
        "4: recalled context was relevant and concise, but not directly actionable. ",
        "3: recalled context was partially relevant, but also partially irrelevant or overly long. ",
        "2: recalled context had only weak relevance, was stale/misleading, or required substantial filtering before use. ",
        "1: recalled context was not relevant. ",
        "For scores 1 or 2, name the main failure mode in the rationale when possible: stale task state, wrong context, noisy metadata, too generic, or too long."
    )
}

fn eval_judge_prompt(turn: &TurnRecord, candidate: &EvalCandidate, citation_score: &str) -> String {
    format!(
        "Replay turn:\n{}\n\nMemory title:\n{}\n\nMemory body:\n{}\n\nRetrieval rank: {}\nRetrieval score: {:.4}\nCounterfactual citation signal: {}\n\nReturn JSON.",
        truncate_eval_text(turn.display_text.as_deref().unwrap_or(""), 4_000),
        truncate_eval_text(&candidate.memory.title, 500),
        truncate_eval_text(&candidate.memory.body, 4_000),
        candidate.rank,
        candidate.retrieval_score,
        citation_score
    )
}

fn truncate_eval_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn score_counts<'a>(scores: impl Iterator<Item = &'a str>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for score in scores {
        *counts
            .entry(display_eval_score(score.to_string()))
            .or_insert(0) += 1;
    }
    counts
}

fn path() -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let project_id = yaaml_core::paths::normalize_project_id(&cwd);
    let recall_dir = config
        .recall_dir()
        .context("failed to resolve recall_dir")?;
    let path = contextual_recall_file_path(&recall_dir, &project_id, &db)?;

    println!("{}", path.display());
    Ok(())
}

fn recall(args: RecallArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let project_id_path = yaaml_core::paths::normalize_project_id(&cwd);
    let recall_dir = config
        .recall_dir()
        .context("failed to resolve recall_dir")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    if args.debug_ranking && !args.json {
        bail!("--debug-ranking requires --json");
    }

    let tool_input_summary = tool_input_summary(&args)?;
    let tool_query = tool_recall_query(&args, tool_input_summary.as_deref());
    let explicit_recall = args.query.is_some() || tool_query.is_some();

    if !explicit_recall && (args.session.is_some() || args.turn.is_some()) {
        return recall_for_historical_turn(args, &config, &db);
    }

    let recall_path = contextual_recall_file_path(&recall_dir, &project_id_path, &db)?;
    let recall_selection_limit = effective_recall_selection_limit(&config);
    let Some(query) = args.query.clone().or(tool_query) else {
        if args.json || args.debug_ranking {
            bail!("--json and --debug-ranking require --query or --session/--turn");
        }
        if let Some(contents) = read_active_recall_file(&db, &recall_path, recall_selection_limit)?
        {
            print!("{contents}");
            return Ok(());
        }
        if let Some(rendered) =
            refresh_missing_recall_file(&db, &config, &project_id_path, &recall_path)?
        {
            print!("{rendered}");
            return Ok(());
        }
        let project_recall_path = recall_file_path(&recall_dir, &project_id_path);
        if project_recall_path != recall_path {
            if let Some(contents) =
                read_active_recall_file(&db, &project_recall_path, recall_selection_limit)?
            {
                print!("{contents}");
                return Ok(());
            }
        }
        println!("no recall file at {}", recall_path.display());
        return Ok(());
    };
    if config.embedding_provider != "openai" {
        bail!(
            "unsupported embedding_provider {}; only openai is implemented",
            config.embedding_provider
        );
    }
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(&config),
        ReqwestTransport::default(),
    );
    let query_embedding = embedding_client
        .embed(&query)
        .context("failed to embed recall query")?;
    let now = unix_timestamp();
    let project_id = project_id_path.display().to_string();
    let recall_origin = args.origin.map(RecallOriginArg::as_str).unwrap_or(
        if args.tool_name.is_some() || args.tool_input_json.is_some() {
            "tool_pre_use"
        } else {
            "manual_query"
        },
    );
    let query_source = match recall_origin {
        "tool_pre_use" => args
            .tool_name
            .as_deref()
            .map(|tool_name| format!("tool_pre_use:{tool_name}"))
            .unwrap_or_else(|| "tool_pre_use".to_string()),
        "manual_query" => "user input".to_string(),
        other => other.to_string(),
    };
    let anchor = recall_anchor_from_args_or_current(&db, &project_id, &args)?;
    let recall_result = recall_from_embedding(
        &db,
        &config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &project_id,
            session_id: anchor.as_ref().map(|anchor| anchor.session_id.as_str()),
            turn_ordinal: anchor.as_ref().and_then(|anchor| anchor.turn_ordinal),
            query_text: &query,
            query_context: None,
            query_source,
            query_timestamp: now.clone(),
            apply_cooldown: true,
        },
    )?;
    let RecallSearchResult {
        query_timestamp,
        query_source,
        project_id: result_project_id,
        selected_memory_ids,
        memories,
        ranking,
        filter_telemetry,
        markdown: rendered,
    } = recall_result;
    let selected_ids = selected_memory_ids.clone();
    if !args.no_eval && !args.debug_ranking {
        queue_query_recall_eval(
            &db,
            &anchor,
            &rendered,
            &selected_ids,
            Some(&filter_telemetry),
            yaaml::daemon::RecallEvalMetadata {
                recall_origin: recall_origin.to_string(),
                turn_id: args.turn_id.clone(),
                tool_name: args.tool_name.clone(),
                tool_use_id: args.tool_use_id.clone(),
                tool_input_summary: tool_input_summary.clone(),
                injected: (recall_origin == "tool_pre_use").then_some(!selected_ids.is_empty()),
            },
        )?;
    }
    if args.codex_hook_output {
        println!(
            "{}",
            serde_json::to_string(&codex_hook_recall_output(
                !selected_ids.is_empty(),
                &rendered
            ))?
        );
        return Ok(());
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&RecallCommandOutput {
                session_id: anchor
                    .as_ref()
                    .map(|anchor| anchor.session_id.clone())
                    .unwrap_or_default(),
                turn_ordinal: anchor
                    .and_then(|anchor| anchor.turn_ordinal)
                    .unwrap_or_default(),
                query_timestamp,
                query_source,
                project_id: result_project_id,
                selected_memory_ids,
                memories,
                ranking: args.debug_ranking.then_some(ranking),
                filter_telemetry: Some(filter_telemetry),
                markdown: rendered,
            })?
        );
        return Ok(());
    }
    let write = write_recall_file(&recall_path, &rendered, &selected_ids)
        .context("failed to write recall file")?;

    match write {
        RecallWrite::Written | RecallWrite::Unchanged => print!("{rendered}"),
        RecallWrite::NoopEmptyResults => {
            if let Some(contents) =
                read_active_recall_file(&db, &recall_path, recall_selection_limit)?
            {
                print!("{contents}");
            } else {
                println!("no recall results");
            }
        }
    }
    Ok(())
}

fn tool_input_summary(args: &RecallArgs) -> anyhow::Result<Option<String>> {
    if let Some(summary) = args.tool_input_summary.as_deref() {
        let summary = summary.trim();
        if !summary.is_empty() {
            return Ok(Some(truncate_chars(summary, 500)));
        }
    }
    let Some(input_json) = args.tool_input_json.as_deref() else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(input_json).context("failed to parse --tool-input-json")?;
    let summary = value
        .get("command")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/tool_input/command")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| value.get("cmd").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| truncate_chars(&value.to_string(), 500));
    Ok(Some(truncate_chars(summary.trim(), 500)))
}

fn tool_recall_query(args: &RecallArgs, tool_input_summary: Option<&str>) -> Option<String> {
    let tool_name = args.tool_name.as_deref()?;
    let mut parts = vec![format!("tool:{tool_name}")];
    if let Some(summary) = tool_input_summary {
        if !summary.trim().is_empty() {
            parts.push(format!("tool input: {summary}"));
        }
    }
    Some(parts.join("\n"))
}

fn recall_anchor_from_args_or_current(
    db: &Database,
    project_id: &str,
    args: &RecallArgs,
) -> anyhow::Result<Option<QueryRecallAnchor>> {
    if let Some(session_id) = args.session.as_deref() {
        return Ok(Some(QueryRecallAnchor {
            session_id: session_id.to_string(),
            turn_ordinal: args.turn,
        }));
    }
    Ok(
        recall_eval_anchor(db, project_id)?.map(|anchor| QueryRecallAnchor {
            session_id: anchor.session_id,
            turn_ordinal: Some(anchor.turn_ordinal),
        }),
    )
}

struct QueryRecallAnchor {
    session_id: String,
    turn_ordinal: Option<u64>,
}

fn queue_query_recall_eval(
    db: &Database,
    anchor: &Option<QueryRecallAnchor>,
    rendered: &str,
    selected_ids: &[i64],
    filter_telemetry: Option<&RecallFilterTelemetry>,
    metadata: yaaml::daemon::RecallEvalMetadata,
) -> anyhow::Result<()> {
    let Some(anchor) = anchor else {
        return Ok(());
    };
    yaaml::daemon::queue_recall_eval_after_turn_with_metadata(
        db,
        &anchor.session_id,
        anchor.turn_ordinal,
        metadata.turn_id.clone(),
        rendered,
        selected_ids,
        filter_telemetry,
        metadata,
        0,
    )?;
    Ok(())
}

fn codex_hook_recall_output(injected: bool, rendered: &str) -> serde_json::Value {
    if injected {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": rendered,
            },
        })
    } else {
        serde_json::json!({})
    }
}

fn refresh_missing_recall_file(
    db: &Database,
    config: &Config,
    project_id_path: &std::path::Path,
    recall_path: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    if config.embedding_provider != "openai" {
        return Ok(None);
    }
    let project_id = project_id_path.display().to_string();
    let Some(session) = recall_session(db, &project_id)? else {
        return Ok(None);
    };
    let Some(turn_ordinal) = latest_turn_ordinal(db, &session.id)? else {
        return Ok(None);
    };
    let window = u64::try_from(config.recall_live_turn_window).unwrap_or(u64::MAX);
    let start_ordinal = turn_ordinal.saturating_add(1).saturating_sub(window.max(1));
    let turns = db
        .completed_turns_for_session_range(
            &session.id,
            start_ordinal,
            turn_ordinal.saturating_add(1),
        )
        .context("failed to load turns for missing recall file")?;
    let turns = hydrate_turns(db, &turns).context("failed to hydrate missing recall turns")?;
    if turns.is_empty() {
        return Ok(None);
    }
    let recall_turns = active_segment_recall_turns(&turns);
    let query = yaaml_core::build_recall_query(
        recall_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    if query.trim().is_empty() {
        return Ok(None);
    }
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let query_embedding = embedding_client
        .embed(&query)
        .context("failed to embed missing recall query")?;
    let now = unix_timestamp();
    let query_context = context_from_turns(
        recall_turns,
        std::path::Path::new(&session.project_id),
        &query,
    );
    let query_source = recall_query_source(
        "on-demand active segment",
        &turns,
        recall_turns,
        start_ordinal,
        turn_ordinal,
        query.len(),
    );
    let result = recall_from_embedding(
        db,
        config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &session.project_id,
            session_id: Some(&session.id),
            turn_ordinal: Some(turn_ordinal),
            query_text: &query,
            query_context: Some(query_context),
            query_source,
            query_timestamp: now,
            apply_cooldown: true,
        },
    )?;
    if result.selected_memory_ids.is_empty() {
        return Ok(None);
    }
    let write = write_recall_file(recall_path, &result.markdown, &result.selected_memory_ids)
        .context("failed to write missing recall file")?;
    if matches!(write, RecallWrite::Written | RecallWrite::Unchanged) {
        yaaml::daemon::queue_recall_eval_after_turn(
            db,
            &session.id,
            turn_ordinal,
            &result.markdown,
            &result.selected_memory_ids,
            None,
            0,
        )?;
    }
    Ok(Some(result.markdown))
}

fn read_active_recall_file(
    db: &Database,
    recall_path: &std::path::Path,
    recall_result_limit: usize,
) -> anyhow::Result<Option<String>> {
    if !recall_path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(recall_path).context("failed to read recall file")?;
    let memory_ids = parse_memory_ids(&contents);
    if memory_ids.is_empty() {
        invalidate_recall_file(recall_path)?;
        return Ok(None);
    }
    if memory_ids.len() > recall_result_limit {
        invalidate_recall_file(recall_path)?;
        return Ok(None);
    }
    let active_memories = db
        .list_active_memories_by_ids(&memory_ids)
        .context("failed to validate recall memory ids")?;
    let active_ids = active_memories
        .iter()
        .filter_map(|memory| memory.id)
        .collect::<HashSet<_>>();
    if active_memories
        .iter()
        .any(cached_memory_requires_recall_refresh)
    {
        invalidate_recall_file(recall_path)?;
        return Ok(None);
    }
    if memory_ids
        .iter()
        .all(|memory_id| active_ids.contains(memory_id))
    {
        Ok(Some(contents))
    } else {
        invalidate_recall_file(recall_path)?;
        Ok(None)
    }
}

fn cached_memory_requires_recall_refresh(memory: &MemoryRecord) -> bool {
    is_transient_plan_memory(memory)
        && !memory.task_keys.iter().any(|key| {
            key.split_once(':')
                .map(|(prefix, _)| prefix != "tool")
                .unwrap_or(true)
        })
}

fn invalidate_recall_file(recall_path: &std::path::Path) -> anyhow::Result<()> {
    match fs::remove_file(recall_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to remove stale recall file {}",
                recall_path.display()
            )
        }),
    }
}

fn recall_session(db: &Database, project_id: &str) -> anyhow::Result<Option<SessionRecord>> {
    if let Some(session_id) = current_session_id() {
        if let Some(session) = db
            .session_by_id(&session_id)
            .context("failed to load current recall session")?
        {
            return Ok(Some(session));
        }
    }
    db.latest_session_for_project(project_id)
        .context("failed to load latest project session")
}

#[derive(Debug, Serialize)]
struct RecallCommandOutput {
    session_id: String,
    turn_ordinal: u64,
    query_timestamp: String,
    query_source: String,
    project_id: String,
    selected_memory_ids: Vec<i64>,
    memories: Vec<RecallMemory>,
    ranking: Option<Vec<RecallDebugRanking>>,
    filter_telemetry: Option<RecallFilterTelemetry>,
    markdown: String,
}

#[derive(Debug, Serialize)]
struct RecallDebugRanking {
    memory_id: i64,
    selected: bool,
    score: f32,
    similarity: f32,
    memory_kind: String,
    filter_reasons: Vec<String>,
    rank: RecallRankDetails,
}

#[derive(Debug)]
struct RecallSearchResult {
    query_timestamp: String,
    query_source: String,
    project_id: String,
    selected_memory_ids: Vec<i64>,
    memories: Vec<RecallMemory>,
    ranking: Vec<RecallDebugRanking>,
    filter_telemetry: RecallFilterTelemetry,
    markdown: String,
}

fn recall_query_source(
    label: &str,
    window_turns: &[TurnRecord],
    recall_turns: &[TurnRecord],
    window_start_ordinal: u64,
    window_end_ordinal: u64,
    query_chars: usize,
) -> String {
    let active_start = recall_turns
        .first()
        .map(|turn| turn.ordinal)
        .unwrap_or(window_start_ordinal);
    let active_end = recall_turns
        .last()
        .map(|turn| turn.ordinal)
        .unwrap_or(window_end_ordinal);
    let window_count = window_turns.len();
    let active_count = recall_turns.len();
    format!(
        "{label} turns {active_start}..={active_end} from window {window_start_ordinal}..={window_end_ordinal} ({active_count}/{window_count} turns): {query_chars} chars"
    )
}

fn recall_for_historical_turn(
    args: RecallArgs,
    config: &Config,
    db: &Database,
) -> anyhow::Result<()> {
    if args.query.is_some() {
        bail!("--query cannot be combined with --session/--turn");
    }
    let session_id = args
        .session
        .as_deref()
        .context("--session is required when using --turn")?;
    let turn_ordinal = args
        .turn
        .context("--turn is required when using --session")?;
    let session = db
        .session_by_id(session_id)
        .context("failed to load session")?
        .with_context(|| format!("session {session_id} not found"))?;
    let window = u64::try_from(config.recall_live_turn_window).unwrap_or(u64::MAX);
    let start_ordinal = turn_ordinal.saturating_add(1).saturating_sub(window.max(1));
    let turns = db
        .completed_turns_for_session_range(
            session_id,
            start_ordinal,
            turn_ordinal.saturating_add(1),
        )
        .context("failed to load recall turns")?;
    let turns = hydrate_turns(db, &turns).context("failed to hydrate historical recall turns")?;
    if turns.is_empty() {
        bail!("no completed turns found for session {session_id} through turn {turn_ordinal}");
    }
    let recall_turns = active_segment_recall_turns(&turns);
    let query = yaaml_core::build_recall_query(
        recall_turns,
        config.recall_query_max_chars,
        config.tool_call_truncation_chars,
    );
    if query.trim().is_empty() {
        bail!(
            "turn window for session {session_id} through turn {turn_ordinal} has no recall text"
        );
    }
    if config.embedding_provider != "openai" {
        bail!(
            "unsupported embedding_provider {}; only openai is implemented",
            config.embedding_provider
        );
    }
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(config),
        ReqwestTransport::default(),
    );
    let query_embedding = embedding_client
        .embed(&query)
        .context("failed to embed historical recall query")?;
    let now = unix_timestamp();
    let query_context = context_from_turns(
        recall_turns,
        std::path::Path::new(&session.project_id),
        &query,
    );
    let query_source = recall_query_source(
        &format!("session {session_id} active segment"),
        &turns,
        recall_turns,
        start_ordinal,
        turn_ordinal,
        query.len(),
    );
    let result = recall_from_embedding(
        db,
        config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &session.project_id,
            session_id: Some(session_id),
            turn_ordinal: Some(turn_ordinal),
            query_text: &query,
            query_context: Some(query_context),
            query_source,
            query_timestamp: now,
            apply_cooldown: false,
        },
    )?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&RecallCommandOutput {
                session_id: session_id.to_string(),
                turn_ordinal,
                query_timestamp: result.query_timestamp,
                query_source: result.query_source,
                project_id: result.project_id,
                selected_memory_ids: result.selected_memory_ids,
                memories: result.memories,
                ranking: args.debug_ranking.then_some(result.ranking),
                filter_telemetry: Some(result.filter_telemetry),
                markdown: result.markdown,
            })?
        );
    } else if result.selected_memory_ids.is_empty() {
        println!("no recall results");
    } else {
        print!("{}", result.markdown);
    }
    Ok(())
}

struct RecallEmbeddingRequest<'a> {
    query_embedding: &'a [f32],
    project_id: &'a str,
    session_id: Option<&'a str>,
    turn_ordinal: Option<u64>,
    query_text: &'a str,
    query_context: Option<ContextMetadata>,
    query_source: String,
    query_timestamp: String,
    apply_cooldown: bool,
}

fn recall_from_embedding(
    db: &Database,
    config: &Config,
    request: RecallEmbeddingRequest<'_>,
) -> anyhow::Result<RecallSearchResult> {
    let index = SqliteExactVectorIndex::new(
        db,
        config.embedding_model.clone(),
        request.query_timestamp.clone(),
    );
    let hits = index
        .search(
            request.query_embedding,
            config.recall_candidate_pool,
            config.recall_similarity_threshold,
        )
        .context("failed to search vector index")?;
    let hit_ids = hits.iter().map(|hit| hit.memory_id).collect::<Vec<_>>();
    let memories = db
        .list_active_memories_by_ids(&hit_ids)
        .context("failed to load matching memories")?;
    let query_context = request.query_context.unwrap_or_else(|| {
        let mut query_context = infer_context_from_path(Path::new(request.project_id));
        merge_contexts(
            &mut query_context,
            infer_context_from_text(request.query_text),
        );
        query_context
    });
    let (query_task_keys, current_segment_id) = recall_query_metadata(
        db,
        request.query_text,
        request.session_id,
        request.turn_ordinal,
    )?;
    let candidates = rank_recall_candidates(
        &hits,
        &memories,
        request.project_id,
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
            current_project_id: request.project_id,
            query_text: request.query_text,
            query_context: &query_context,
            query_task_keys: &query_task_keys,
            current_segment_id,
        },
    );
    let cooldown_since = request.apply_cooldown.then_some(()).and_then(|()| {
        request.session_id.zip(cooldown_since_unix(
            &request.query_timestamp,
            config.recall_memory_cooldown_seconds,
        ))
    });
    filter_result.telemetry.cooldown_since_unix =
        cooldown_since.as_ref().map(|(_session_id, since)| *since);
    let recent_memory_ids = if let Some((session_id, since_unix)) = cooldown_since {
        db.recent_recalled_memory_ids(session_id, since_unix)
            .context("failed to load recent recall memory ids")?
    } else {
        HashSet::new()
    };
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
    let debug_candidates = filter_result.debug_candidates;
    let selected_memory_ids = selected
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<Vec<_>>();
    let selected_memories = db
        .list_active_memories_by_ids(&selected_memory_ids)
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
    let selected_id_set = selected_memory_ids.iter().copied().collect::<HashSet<_>>();
    let ranking = debug_candidates
        .iter()
        .map(|candidate| RecallDebugRanking {
            memory_id: candidate.memory_id,
            selected: selected_id_set.contains(&candidate.memory_id),
            score: candidate.score,
            similarity: candidate.similarity,
            memory_kind: memories
                .iter()
                .find(|memory| memory.id == Some(candidate.memory_id))
                .map(|memory| memory.kind.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            filter_reasons: candidate.rank.filter_reasons.clone(),
            rank: candidate.rank.clone(),
        })
        .collect::<Vec<_>>();
    let markdown = render_recall_markdown(
        &request.query_timestamp,
        &request.query_source,
        request.project_id,
        &recall_memories,
    );
    Ok(RecallSearchResult {
        query_timestamp: request.query_timestamp,
        query_source: request.query_source,
        project_id: request.project_id.to_string(),
        selected_memory_ids,
        memories: recall_memories,
        ranking,
        filter_telemetry: filter_result.telemetry,
        markdown,
    })
}

fn recall_query_metadata(
    db: &Database,
    query_text: &str,
    session_id: Option<&str>,
    turn_ordinal: Option<u64>,
) -> anyhow::Result<(Vec<String>, Option<i64>)> {
    let query_task_keys = extract_task_keys(query_text);
    let Some((session_id, turn_ordinal)) = session_id.zip(turn_ordinal) else {
        return Ok((query_task_keys, None));
    };
    let Some(segment) = db
        .conversation_segment_for_turn(session_id, turn_ordinal)
        .context("failed to load conversation segment for recall query")?
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

fn remember(args: RememberArgs) -> anyhow::Result<()> {
    let title = args.title.trim();
    let body = args.body.trim();
    if title.is_empty() {
        bail!("memory title cannot be empty");
    }
    if body.is_empty() {
        bail!("memory body cannot be empty");
    }

    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    if config.embedding_provider != "openai" {
        bail!(
            "unsupported embedding_provider {}; only openai is implemented",
            config.embedding_provider
        );
    }
    let project_id_path = yaaml_core::paths::normalize_project_id(&cwd);
    let project_id = project_id_path.display().to_string();
    let project_descriptor = args
        .project_descriptor
        .as_deref()
        .map(str::trim)
        .filter(|descriptor| !descriptor.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derive_project_descriptor(&project_id_path, None));
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;

    let now = unix_timestamp();
    let memory = MemoryRecord {
        id: None,
        title: truncate_chars(title, 200),
        body: truncate_chars(body, config.max_memory_length),
        scope: args.scope.into(),
        kind: infer_memory_kind(title, body, args.scope.into()),
        task_keys: extract_task_keys(&format!("{title}\n{body}")),
        source_turn_refs: Vec::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
        is_active: true,
        session_id: current_session_id(),
        project_id: Some(project_id),
        project_descriptor: Some(project_descriptor),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: MemoryValidity::Durable,
    };
    let text = embedding_text(&memory);
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(&config),
        ReqwestTransport::default(),
    );
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
        updated_at: now,
    })
    .context("failed to persist embedding")?;

    println!("remembered memory {memory_id} ({})", memory.scope.as_str());
    Ok(())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecallEvalAnchor {
    session_id: String,
    turn_ordinal: u64,
}

fn recall_eval_anchor(db: &Database, project_id: &str) -> anyhow::Result<Option<RecallEvalAnchor>> {
    if let Some(session_id) = current_session_id() {
        let turn_ordinal = latest_turn_ordinal(db, &session_id)?.unwrap_or(0);
        return Ok(Some(RecallEvalAnchor {
            session_id,
            turn_ordinal,
        }));
    }
    let Some(session) = db
        .latest_session_for_project(project_id)
        .context("failed to load latest project session")?
    else {
        return Ok(None);
    };
    let turn_ordinal = latest_turn_ordinal(db, &session.id)?.unwrap_or(0);
    Ok(Some(RecallEvalAnchor {
        session_id: session.id,
        turn_ordinal,
    }))
}

fn latest_turn_ordinal(db: &Database, session_id: &str) -> anyhow::Result<Option<u64>> {
    db.turns_for_session(session_id, 1)
        .context("failed to load session turns")
        .map(|turns| turns.into_iter().next().map(|turn| turn.ordinal))
}

fn contextual_recall_file_path(
    recall_dir: &std::path::Path,
    project_id: &std::path::Path,
    db: &Database,
) -> anyhow::Result<PathBuf> {
    if let Some(session_id) = current_session_id() {
        return Ok(session_recall_file_path(recall_dir, &session_id));
    }
    let project_id_string = project_id.display().to_string();
    if let Some(session) = db
        .latest_session_for_project(&project_id_string)
        .context("failed to load latest project session")?
    {
        return Ok(session_recall_file_path(recall_dir, &session.id));
    }
    Ok(recall_file_path(recall_dir, project_id))
}

fn current_session_id() -> Option<String> {
    env::var("CODEX_THREAD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn print_human_status(status: &yaaml_core::status::Status) {
    println!("YAAML status");
    println!("  database: {}", status.db_path);
    println!(
        "  memories: {} active / {} total",
        status.active_memory_count, status.memory_count
    );
    println!(
        "  transcripts: {} files tracked, {} sessions, {} stored turns",
        status.backlog.transcript_files, status.backlog.sessions, status.backlog.stored_turns
    );
    println!(
        "  workers: {} queued, {} scheduled, {} running",
        status.workers.queued_jobs, status.workers.scheduled_jobs, status.workers.running_jobs
    );
    println!("  parked jobs: {}", status.parked_jobs);
}

fn print_human_tasks(tasks: &[TaskListRecord]) {
    if tasks.is_empty() {
        println!("No tasks");
        return;
    }
    println!("Tasks");
    for task in tasks {
        let next_run = task
            .next_run_at
            .as_deref()
            .map(human_timestamp)
            .unwrap_or_else(|| "-".to_string());
        let error = task.last_error.as_deref().unwrap_or("-");
        println!(
            "  {}  {}  {}  attempts={}/{}  next={}  error={}",
            task.id,
            task.display_status,
            task.kind,
            task.attempts,
            task.max_attempts,
            next_run,
            truncate_task_error(error)
        );
    }
}

fn print_human_memory_stats(stats: &MemoryStatsOutput) {
    println!("YAAML memories");
    println!(
        "  memories: {} active / {} total",
        stats.active, stats.total
    );
    println!("  by scope/kind:");
    for row in &stats.by_scope_kind {
        let active = if row.active { "active" } else { "inactive" };
        println!("    {} {} {}: {}", row.scope, row.kind, active, row.count);
    }
    if !stats.top_projects.is_empty() {
        println!("  top projects:");
        for project in &stats.top_projects {
            println!("    {}: {}", project.project_id, project.active_count);
        }
    }
}

fn truncate_task_error(error: &str) -> String {
    const MAX_ERROR_CHARS: usize = 120;
    if error.chars().count() <= MAX_ERROR_CHARS {
        return error.to_string();
    }
    let mut truncated = error.chars().take(MAX_ERROR_CHARS).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn load_config(cwd: &std::path::Path, explicit_config: Option<PathBuf>) -> anyhow::Result<Config> {
    if let Some(config) = explicit_config {
        Config::load_from_paths(ConfigPaths {
            user_config: config,
            project_config: cwd.join(".yaaml").join("config.toml"),
        })
        .context("failed to load explicit config")
    } else {
        Config::load_for_cwd(cwd).context("failed to load config")
    }
}

fn unix_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix:{seconds}")
}

fn human_timestamp(timestamp: &str) -> String {
    let Some(seconds) = timestamp
        .strip_prefix("unix:")
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return timestamp.to_string();
    };
    local_human_timestamp(seconds).unwrap_or_else(|| timestamp.to_string())
}

#[cfg(unix)]
fn local_human_timestamp(seconds: i64) -> Option<String> {
    let time: libc::time_t = seconds;
    let mut local_time = std::mem::MaybeUninit::<libc::tm>::uninit();
    let format = b"%Y-%m-%d %H:%M:%S %Z\0";
    let mut buffer = [0 as libc::c_char; 64];
    unsafe {
        if libc::localtime_r(&time, local_time.as_mut_ptr()).is_null() {
            return None;
        }
        let local_time = local_time.assume_init();
        let len = libc::strftime(
            buffer.as_mut_ptr(),
            buffer.len(),
            format.as_ptr().cast(),
            &local_time,
        );
        if len == 0 {
            return None;
        }
        CStr::from_ptr(buffer.as_ptr())
            .to_str()
            .ok()
            .map(str::to_string)
    }
}

#[cfg(not(unix))]
fn local_human_timestamp(_seconds: i64) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_timestamp_formats_unix_seconds_in_local_timezone() {
        let formatted = human_timestamp("unix:1781205326");
        assert!(!formatted.starts_with("unix:"));
        assert!(formatted.contains("2026-"));
        assert!(formatted.contains(":26 "));
        assert_eq!(
            human_timestamp("2026-06-11T00:00:00Z"),
            "2026-06-11T00:00:00Z"
        );
    }

    #[test]
    fn health_apply_policy_accepts_only_high_confidence_inactive_actions() {
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "stale_episodic",
            "move_to_dormant",
            true,
            5,
            0,
            4
        )));
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "stale_task_state",
            "move_to_dormant",
            true,
            6,
            0,
            5
        )));
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "consistently_low_value",
            "suppress_or_tombstone",
            true,
            6,
            0,
            5
        )));
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "wrong_context",
            "regenerate_metadata_or_tighten_gates",
            true,
            8,
            0,
            8
        )));
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "noisy_metadata",
            "regenerate_task_keys",
            true,
            6,
            0,
            6
        )));
        assert!(should_apply_memory_health_action(&health_diagnostic(
            "vague_under_contextualized",
            "refine_or_suppress",
            true,
            9,
            0,
            9
        )));
        assert!(!should_apply_memory_health_action(&health_diagnostic(
            "likely_low_value",
            "suppress_pending_more_evals",
            true,
            4,
            0,
            4
        )));
        assert!(!should_apply_memory_health_action(&health_diagnostic(
            "stale_episodic",
            "move_to_dormant",
            true,
            6,
            1,
            5
        )));
        assert!(!should_apply_memory_health_action(&health_diagnostic(
            "wrong_context",
            "regenerate_metadata_or_tighten_gates",
            true,
            8,
            1,
            7
        )));
        assert!(!should_apply_memory_health_action(&health_diagnostic(
            "wrong_context",
            "regenerate_metadata_or_tighten_gates",
            true,
            8,
            0,
            7
        )));
        assert!(!should_apply_memory_health_action(&health_diagnostic(
            "stale_episodic",
            "move_to_dormant",
            false,
            6,
            0,
            5
        )));
    }

    fn health_diagnostic(
        failure_mode: &str,
        recommended_action: &str,
        is_active: bool,
        judged_count: u64,
        useful_count: u64,
        low_count: u64,
    ) -> MemoryHealthDiagnostic {
        MemoryHealthDiagnostic {
            memory_id: 1,
            title: "memory".to_string(),
            kind: "lesson".to_string(),
            scope: "project".to_string(),
            is_active,
            project_id: Some("/tmp/project".to_string()),
            selected_count: judged_count,
            judged_count,
            useful_count,
            low_count,
            neutral_count: judged_count.saturating_sub(useful_count + low_count),
            insufficient_context_count: 0,
            low_rate: ratio(low_count, judged_count),
            useful_rate: ratio(useful_count, judged_count),
            average_score: None,
            failure_mode: failure_mode.to_string(),
            recommended_action: recommended_action.to_string(),
            evidence: Vec::new(),
            latest_low_rationale: None,
            latest_useful_rationale: None,
        }
    }
}
