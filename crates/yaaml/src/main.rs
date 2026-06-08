use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use yaaml_core::{
    apply_project_bonus, recall_file_path, render_recall_markdown, write_recall_file, Config,
    RecallCandidate, RecallMemory, RecallWrite, VectorIndex,
};
use yaaml_llm::openai::{OpenAiEmbeddingClient, OpenAiEmbeddingConfig};
use yaaml_llm::ReqwestTransport;
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
    /// Show daemon, memory, backlog, and provider status.
    Status(StatusArgs),
    /// Resolve the current project's daemon-owned recall file path.
    Path,
    /// Write recalled memories for a manual query.
    Recall(RecallArgs),
}

#[derive(Debug, Parser)]
struct StatusArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct RecallArgs {
    /// Query text to embed and search against stored memories.
    #[arg(long)]
    query: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli
        .command
        .unwrap_or(Command::Status(StatusArgs { json: false }))
    {
        Command::Status(args) => status(args),
        Command::Path => path(),
        Command::Recall(args) => recall(args),
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

fn path() -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    let project_id = yaaml_core::paths::normalize_project_id(&cwd);
    let recall_dir = config
        .recall_dir()
        .context("failed to resolve recall_dir")?;
    let path = recall_file_path(&recall_dir, &project_id);

    println!("{}", path.display());
    Ok(())
}

fn recall(args: RecallArgs) -> anyhow::Result<()> {
    let cwd = env::current_dir().context("failed to determine current directory")?;
    let config = Config::load_for_cwd(&cwd).context("failed to load config")?;
    if config.embedding_provider != "openai" {
        bail!(
            "unsupported embedding_provider {}; only openai is implemented",
            config.embedding_provider
        );
    }
    let db_path = config.db_path().context("failed to resolve db_path")?;
    let mut db = Database::open(&db_path)
        .with_context(|| format!("failed to open {}", display(&db_path)))?;
    db.migrate().context("failed to migrate database")?;

    let embedding_client = OpenAiEmbeddingClient::new(
        OpenAiEmbeddingConfig::from_config(&config),
        ReqwestTransport::default(),
    );
    let query_embedding = embedding_client
        .embed(&args.query)
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
    let project_id_path = yaaml_core::paths::normalize_project_id(&cwd);
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
    let rendered = render_recall_markdown(&now, "manual query", &project_id, &recall_memories);
    let recall_dir = config
        .recall_dir()
        .context("failed to resolve recall_dir")?;
    let recall_path = recall_file_path(&recall_dir, &project_id_path);
    let write = write_recall_file(&recall_path, &rendered, &selected_ids)
        .context("failed to write recall file")?;

    match write {
        RecallWrite::Written => println!("wrote {}", recall_path.display()),
        RecallWrite::Unchanged => println!("unchanged {}", recall_path.display()),
        RecallWrite::NoopEmptyResults => println!("no recall results; preserved existing file"),
    }
    Ok(())
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

fn unix_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix:{seconds}")
}
