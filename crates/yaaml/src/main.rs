use std::env;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use yaaml_core::Config;
use yaaml_store::Database;

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
}

#[derive(Debug, Parser)]
struct StatusArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli
        .command
        .unwrap_or(Command::Status(StatusArgs { json: false }))
    {
        Command::Status(args) => status(args),
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
