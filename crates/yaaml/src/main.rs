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
use yaaml::turn_hydration::{context_from_turns, hydrate_turns};
use yaaml_core::{
    context_score, counterfactual_citation_score, derive_project_descriptor, embedded_text_hash,
    embedding_text, extract_task_keys, infer_context_from_memory, infer_context_from_path,
    infer_context_from_text, infer_memory_kind, merge_contexts, parse_eval_judge_response,
    parse_memory_ids, rank_recall_candidates, recall_file_path, render_recall_markdown,
    session_recall_file_path, write_recall_file, Config, ConfigPaths, ContextMetadata,
    EmbeddingRecord, MemoryRecord, MemoryScope, RecallMemory, RecallRankDetails,
    RecallRankingOptions, RecallWrite, SessionRecord, TurnRecord, VectorIndex,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::ReqwestTransport;
use yaaml_store::database::{
    EvalResultRecord, EvalRunRecord, RecallEvalTaskRecord, TaskListRecord,
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
    /// Inspect configuration.
    Config(ConfigArgs),
    /// Inspect or manage daemon tasks.
    Tasks(TasksArgs),
    /// Inspect or rebuild stored memories.
    Memories(MemoriesArgs),
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
}

#[derive(Debug, Parser)]
struct EvalRecallArgs {
    /// Maximum turns to replay.
    #[arg(long, default_value_t = 10)]
    limit: usize,
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
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct StatusArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
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

#[derive(Debug, Subcommand)]
enum MemoriesCommand {
    /// Show memory counts by scope, kind, and project.
    Stats(MemoriesStatsArgs),
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
    /// Session id to replay recall for.
    #[arg(long)]
    session: Option<String>,
    /// Completed turn ordinal to use as the recall anchor.
    #[arg(long)]
    turn: Option<u64>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
    /// Include per-memory ranking components in JSON output.
    #[arg(long)]
    debug_ranking: bool,
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
        Command::Config(args) => config(args),
        Command::Tasks(args) => tasks(args),
        Command::Memories(args) => memories(args),
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
struct MemoryRebuildOutput {
    deactivated_memories: u64,
    cleared_memory_tasks: u64,
    queued_memory_jobs: u64,
}

fn memories(args: MemoriesArgs) -> anyhow::Result<()> {
    match args.command {
        MemoriesCommand::Stats(args) => memories_stats(args),
        MemoriesCommand::Rebuild(args) => memories_rebuild(args),
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
    let paths = yaaml::skills::InitPaths::for_home(&home);
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
    }
}

fn eval_recall(args: EvalRecallArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let now = unix_timestamp();
    let run_id = db
        .insert_eval_run(
            "default",
            &now,
            &serde_json::json!({
                "limit": args.limit,
                "judge_provider": config.eval_judge_provider,
                "judge_model": config.eval_judge_model,
                "judge_enabled": !args.no_judge,
                "retrieval": "vector_or_lexical_fallback",
            })
            .to_string(),
        )
        .context("failed to create eval run")?;
    let turns = db
        .list_turns_with_ids(args.limit)
        .context("failed to load replay turns")?;
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
        let memories = match turn.observed_at.as_deref() {
            Some(observed_at) => db
                .list_active_memories_created_before(observed_at)
                .context("failed to load memories for replay turn")?,
            None => Vec::new(),
        };
        let query = turn.display_text.clone().unwrap_or_default();
        let fallback_project = turn.cwd.as_deref().unwrap_or("");
        let query_context = context_from_turns(
            std::slice::from_ref(turn),
            std::path::Path::new(fallback_project),
            &query,
        );
        let candidates = select_eval_candidates(
            &config,
            &embedding_client,
            &vector_index,
            &query,
            &query_context,
            &memories,
        );
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

fn eval_list(args: EvalListArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let runs = db
        .list_eval_runs(args.limit)
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
            println!(
                "  {}  score={}  session={}  turn={}  started={}  completed={}  results={}",
                run.id, score, session, turn, run.started_at_human, completed, run.result_count
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
        .list_eval_runs(args.limit)
        .context("failed to list eval runs")?;
    let summary = build_eval_summary(&db, runs)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_human_eval_summary(&summary);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct EvalSummary {
    runs_considered: usize,
    results_considered: usize,
    judged_results: usize,
    average_score: Option<f64>,
    score_counts: BTreeMap<String, u64>,
    session_breakdown: Vec<EvalSessionSummary>,
    stale_insufficient_context: Vec<EvalStaleInsufficientContext>,
    queued_recall_evals: Vec<EvalQueuedRecallTask>,
    low_score_examples: Vec<EvalSummaryExample>,
    high_score_examples: Vec<EvalSummaryExample>,
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
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalSummaryExample {
    run_id: i64,
    result_id: i64,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
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

fn build_eval_summary(db: &Database, runs: Vec<EvalRunRecord>) -> anyhow::Result<EvalSummary> {
    let mut all_scores = Vec::new();
    let mut numeric_scores = Vec::new();
    let mut low_score_examples = Vec::new();
    let mut high_score_examples = Vec::new();
    let mut stale_insufficient_context = Vec::new();
    let mut session_accumulators = BTreeMap::<String, EvalSessionAccumulator>::new();
    let mut results_considered = 0_usize;

    for run in &runs {
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        let run_context = eval_run_context(run);
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

        let has_insufficient_context = results
            .iter()
            .any(|result| result.judge_score.as_deref() == Some("insufficient_context"));
        if has_insufficient_context && stale_insufficient_context.len() < 10 {
            if let (Some(session_id), Some(turn_ordinal)) =
                (run_context.session_id.as_deref(), run_context.turn_ordinal)
            {
                let later_turn_count = later_completed_turn_count(db, &run_context)?.unwrap_or(0);
                if later_turn_count > 0 {
                    stale_insufficient_context.push(EvalStaleInsufficientContext {
                        run_id: run.id,
                        session_id: session_id.to_string(),
                        turn_ordinal,
                        score: "n/a".to_string(),
                        later_completed_turns: later_turn_count,
                    });
                }
            }
        }

        for result in results {
            results_considered += 1;
            accumulator.results += 1;
            if let Some(score) = result.judge_score.as_deref() {
                all_scores.push(display_eval_score(score.to_string()));
                accumulator
                    .all_scores
                    .push(display_eval_score(score.to_string()));
                if let Some(numeric_score) = numeric_eval_score(score) {
                    numeric_scores.push(numeric_score);
                    accumulator.numeric_scores.push(numeric_score);
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
        runs_considered: runs.len(),
        results_considered,
        judged_results,
        average_score,
        score_counts: score_counts(all_scores.iter().map(String::as_str)),
        session_breakdown,
        stale_insufficient_context,
        queued_recall_evals,
        low_score_examples,
        high_score_examples,
    })
}

#[derive(Debug, Clone)]
struct EvalRunContext {
    run_id: i64,
    session_id: Option<String>,
    turn_ordinal: Option<u64>,
    memory_ids: Vec<i64>,
}

fn eval_run_context(run: &EvalRunRecord) -> EvalRunContext {
    let config = serde_json::from_str::<serde_json::Value>(&run.config_json).ok();
    EvalRunContext {
        run_id: run.id,
        session_id: run.session_id.clone().or_else(|| {
            config
                .as_ref()
                .and_then(|value| value.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        }),
        turn_ordinal: run.turn_ordinal.or_else(|| {
            config
                .as_ref()
                .and_then(|value| value.get("turn_ordinal"))
                .and_then(serde_json::Value::as_u64)
        }),
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
    }
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
    print_eval_session_breakdown(&summary.session_breakdown);
    print_stale_insufficient_context(&summary.stale_insufficient_context);
    print_queued_recall_evals(&summary.queued_recall_evals);
    print_eval_summary_examples("Low-score examples", &summary.low_score_examples);
    print_eval_summary_examples("High-score examples", &summary.high_score_examples);
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
    println!("N/a evals with later turns");
    for eval in stale {
        println!(
            "  run={} session={} turn={} later_turns={}",
            eval.run_id, eval.session_id, eval.turn_ordinal, eval.later_completed_turns
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
            "  task={} status={} attempts={}/{} session={} turn={} next_run={}",
            task.task_id,
            task.status,
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
    }
}

fn print_eval_summary_examples(label: &str, examples: &[EvalSummaryExample]) {
    if examples.is_empty() {
        return;
    }
    println!("{label}");
    for example in examples {
        let title = example.memory_title.as_deref().unwrap_or("no memory");
        println!(
            "  run={} result={} score={} memory={} title={}",
            example.run_id,
            example.result_id,
            example.score,
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

fn eval_judge_client(
    config: &Config,
    disabled: bool,
) -> Option<AnthropicMessageClient<ReqwestTransport>> {
    if disabled || config.eval_judge_provider != "anthropic" {
        return None;
    }
    if env::var(&config.eval_judge_api_key_env).is_err() {
        return None;
    }
    Some(AnthropicMessageClient::new(
        AnthropicMessageConfig::judge_from_config(config),
        ReqwestTransport::default(),
    ))
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
    judge_client: Option<&AnthropicMessageClient<ReqwestTransport>>,
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
        "1: recalled context was not relevant."
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

    if args.session.is_some() || args.turn.is_some() {
        return recall_for_historical_turn(args, &config, &db);
    }

    let recall_path = contextual_recall_file_path(&recall_dir, &project_id_path, &db)?;
    let Some(query) = args.query else {
        if args.json || args.debug_ranking {
            bail!("--json and --debug-ranking require --query or --session/--turn");
        }
        if let Some(contents) = read_active_recall_file(&db, &recall_path)? {
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
            if let Some(contents) = read_active_recall_file(&db, &project_recall_path)? {
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
    let recall_result = recall_from_embedding(
        &db,
        &config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &project_id,
            query_text: &query,
            query_context: None,
            query_source: "user input".to_string(),
            query_timestamp: now.clone(),
        },
    )?;
    let rendered = recall_result.markdown;
    let selected_ids = recall_result.selected_memory_ids;
    if args.json {
        let anchor = recall_eval_anchor(&db, &project_id)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&RecallCommandOutput {
                session_id: anchor
                    .as_ref()
                    .map(|anchor| anchor.session_id.clone())
                    .unwrap_or_default(),
                turn_ordinal: anchor.map(|anchor| anchor.turn_ordinal).unwrap_or_default(),
                query_timestamp: recall_result.query_timestamp,
                query_source: recall_result.query_source,
                project_id: recall_result.project_id,
                selected_memory_ids: selected_ids,
                memories: recall_result.memories,
                ranking: args.debug_ranking.then_some(recall_result.ranking),
                markdown: rendered,
            })?
        );
        return Ok(());
    }
    let write = write_recall_file(&recall_path, &rendered, &selected_ids)
        .context("failed to write recall file")?;
    if !selected_ids.is_empty() && matches!(write, RecallWrite::Written | RecallWrite::Unchanged) {
        if let Some(anchor) = recall_eval_anchor(&db, &project_id)? {
            yaaml::daemon::queue_recall_eval_after_turn(
                &db,
                &anchor.session_id,
                anchor.turn_ordinal,
                &rendered,
                &selected_ids,
                0,
            )?;
        }
    }

    match write {
        RecallWrite::Written | RecallWrite::Unchanged => print!("{rendered}"),
        RecallWrite::NoopEmptyResults => {
            if let Some(contents) = read_active_recall_file(&db, &recall_path)? {
                print!("{contents}");
            } else {
                println!("no recall results");
            }
        }
    }
    Ok(())
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
    let query = yaaml_core::build_recall_query(
        &turns,
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
    let query_source = format!(
        "on-demand completed turns {start_ordinal}..={turn_ordinal}: {} chars",
        query.len()
    );
    let query_context =
        context_from_turns(&turns, std::path::Path::new(&session.project_id), &query);
    let result = recall_from_embedding(
        db,
        config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &session.project_id,
            query_text: &query,
            query_context: Some(query_context),
            query_source,
            query_timestamp: now,
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
            0,
        )?;
    }
    Ok(Some(result.markdown))
}

fn read_active_recall_file(
    db: &Database,
    recall_path: &std::path::Path,
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
    let active_memories = db
        .list_active_memories_by_ids(&memory_ids)
        .context("failed to validate recall memory ids")?;
    let active_ids = active_memories
        .into_iter()
        .filter_map(|memory| memory.id)
        .collect::<HashSet<_>>();
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
    markdown: String,
}

#[derive(Debug, Serialize)]
struct RecallDebugRanking {
    memory_id: i64,
    score: f32,
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
    markdown: String,
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
    let query = yaaml_core::build_recall_query(
        &turns,
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
    let query_source = format!(
        "session {session_id} completed turns {start_ordinal}..={turn_ordinal}: {} chars",
        query.len()
    );
    let query_context =
        context_from_turns(&turns, std::path::Path::new(&session.project_id), &query);
    let result = recall_from_embedding(
        db,
        config,
        RecallEmbeddingRequest {
            query_embedding: &query_embedding,
            project_id: &session.project_id,
            query_text: &query,
            query_context: Some(query_context),
            query_source,
            query_timestamp: now,
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
    query_text: &'a str,
    query_context: Option<ContextMetadata>,
    query_source: String,
    query_timestamp: String,
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
    let query_task_keys = extract_task_keys(request.query_text);
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
    let selected = candidates
        .into_iter()
        .take(config.recall_result_limit)
        .collect::<Vec<_>>();
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
    let ranking = selected
        .iter()
        .map(|candidate| RecallDebugRanking {
            memory_id: candidate.memory_id,
            score: candidate.score,
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
        markdown,
    })
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
        "  ingestion totals: {} files discovered, {} file passes, {} turns inserted",
        status.backlog.discovered_files,
        status.backlog.processed_files,
        status.backlog.processed_turns
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
}
