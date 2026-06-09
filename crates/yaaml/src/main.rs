use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use yaaml_core::{
    apply_project_bonus, counterfactual_citation_score, derive_project_descriptor,
    embedded_text_hash, embedding_text, parse_eval_judge_response, recall_file_path,
    render_recall_markdown, session_recall_file_path, write_recall_file, Config, ConfigPaths,
    EmbeddingRecord, MemoryRecord, MemoryScope, RecallCandidate, RecallMemory, RecallWrite,
    TurnRecord, VectorIndex,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::ReqwestTransport;
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
struct StatusArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct RecallArgs {
    /// User input to embed and search against stored memories. Omit to print existing recall.
    #[arg(long)]
    query: Option<String>,
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
        Command::Path => path(),
        Command::Recall(args) => recall(args),
        Command::Remember(args) => remember(args),
    }
}

fn status(args: StatusArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;
    let status = db.status().context("failed to read status")?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_human_status(&status);
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
    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(&config),
        ReqwestTransport::default(),
    );
    let vector_index =
        SqliteExactVectorIndex::new(&db, config.embedding_model.clone(), now.clone());
    let judge_client = eval_judge_client(&config, args.no_judge);
    let mut evaluated_memories = 0_u64;
    for (turn_row_id, turn) in &turns {
        let memories = match turn.observed_at.as_deref() {
            Some(observed_at) => db
                .list_active_memories_created_before(observed_at)
                .context("failed to load memories for replay turn")?,
            None => Vec::new(),
        };
        let query = turn.display_text.clone().unwrap_or_default();
        let candidates =
            select_eval_candidates(&config, &embedding_client, &vector_index, &query, &memories);
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
    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
    } else if runs.is_empty() {
        println!("No eval runs");
    } else {
        println!("Eval runs");
        for run in runs {
            let completed = run.completed_at.as_deref().unwrap_or("running");
            println!(
                "  {}  {}  started={}  completed={}  results={}",
                run.id, run.strategy, run.started_at, completed, run.result_count
            );
        }
    }
    Ok(())
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
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run,
                "score_counts": score_counts(results.iter().filter_map(|result| result.judge_score.as_deref())),
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
            println!(
                "  result={} turn={} memory={} score={} title={}",
                result.id,
                result.turn_id,
                result
                    .memory_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                result.judge_score.as_deref().unwrap_or("unknown"),
                title
            );
            if let Some(rationale) = result.rationale {
                println!("    {rationale}");
            }
        }
    }
    Ok(())
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
            let candidates = hits
                .into_iter()
                .filter(|hit| eligible_ids.contains(&hit.memory_id))
                .filter_map(|hit| {
                    eligible_memories
                        .iter()
                        .find(|memory| memory.id == Some(hit.memory_id))
                        .map(|memory| (memory.clone(), hit.similarity))
                })
                .enumerate()
                .map(|(index, (memory, similarity))| EvalCandidate {
                    memory,
                    rank: index + 1,
                    retrieval_score: similarity,
                    retrieval_strategy: "vector",
                })
                .take(config.recall_result_limit)
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return candidates;
            }
        }
    }
    lexical_eval_candidates(config, query, eligible_memories)
}

fn lexical_eval_candidates(
    config: &Config,
    query: &str,
    eligible_memories: &[MemoryRecord],
) -> Vec<EvalCandidate> {
    let query_terms = terms(query);
    let mut candidates = eligible_memories
        .iter()
        .cloned()
        .map(|memory| {
            let memory_text = format!("{} {}", memory.title, memory.body);
            let score = lexical_score(&query_terms, &memory_text);
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
        *counts.entry(score.to_string()).or_insert(0) += 1;
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
    let recall_path = contextual_recall_file_path(&recall_dir, &project_id_path, &db)?;
    let Some(query) = args.query else {
        if recall_path.exists() {
            print!(
                "{}",
                fs::read_to_string(&recall_path).context("failed to read recall file")?
            );
        } else {
            println!("no recall file at {}", recall_path.display());
        }
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
    let index = SqliteExactVectorIndex::new(&db, config.embedding_model.clone(), now.clone());
    let hits = index
        .search(
            &query_embedding,
            config.recall_candidate_pool,
            config.recall_similarity_threshold,
        )
        .context("failed to search vector index")?;
    let hit_ids = hits.iter().map(|hit| hit.memory_id).collect::<Vec<_>>();
    let memories = db
        .list_active_memories_by_ids(&hit_ids)
        .context("failed to load matching memories")?;
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
    let project_id = project_id_path.display().to_string();
    let candidates = if config.recall_project_tiebreaker {
        apply_project_bonus(candidates, &project_id, config.recall_project_score_bonus)
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
    let rendered = render_recall_markdown(&now, "user input", &project_id, &recall_memories);
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
            if recall_path.exists() {
                print!(
                    "{}",
                    fs::read_to_string(&recall_path).context("failed to read recall file")?
                );
            } else {
                println!("no recall results");
            }
        }
    }
    Ok(())
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
        return Ok(session_recall_file_path(
            recall_dir,
            project_id,
            &session_id,
        ));
    }
    let project_id_string = project_id.display().to_string();
    if let Some(session) = db
        .latest_session_for_project(&project_id_string)
        .context("failed to load latest project session")?
    {
        return Ok(session_recall_file_path(
            recall_dir,
            project_id,
            &session.id,
        ));
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
        "  backlog: {} files discovered, {} processed, {} turns processed",
        status.backlog.discovered_files,
        status.backlog.processed_files,
        status.backlog.processed_turns
    );
    println!(
        "  workers: {} queued, {} running",
        status.workers.queued_jobs, status.workers.running_jobs
    );
    println!("  parked jobs: {}", status.parked_jobs);
}

fn display(path: &PathBuf) -> String {
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
