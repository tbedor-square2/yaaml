# YAAML

YAAML is "Yet Another Agent Memory Layer": a local, file-first memory system for AI coding agents such as Codex and Claude Code.

YAAML watches native agent transcripts, summarizes durable context into a local SQLite database, and retrieves memories only when an agent or user makes an explicit recall query. Results can be cached as Markdown under `~/.yaaml/recall`; YAAML does not automatically inject or preload them into agent conversations.

## What It Does

- Watches Codex and Claude Code transcript directories for completed turns.
- Stores durable memories in a user-global database at `~/.yaaml/yaaml.db`.
- Uses embeddings for explicit, on-demand recall queries.
- Can cache explicit recall output as Markdown instead of mutating the live chat context.
- Installs `yaaml` and `yaaml-remember` skills for Codex and Claude Code.
- Provides CLI commands for status, manual recall, manual memory creation, task inspection, and recall evaluation.

## Requirements

- Rust and Cargo
- `just` for the repository quality gate
- `OPENAI_API_KEY` for embeddings
- `ANTHROPIC_API_KEY` for the default summarization, consolidation, and eval judge models

OpenAI is currently the implemented embedding provider. Anthropic is the default message provider for memory formulation and consolidation.

## Install

From this checkout:

```sh
cargo install --path crates/yaaml
```

Or run it without installing while developing:

```sh
cargo run -p yaaml -- --help
```

After installing, make sure `yaaml` is on your `PATH`:

```sh
yaaml --help
```

## Initial Setup

Export provider keys in your shell:

```sh
export OPENAI_API_KEY=...
export ANTHROPIC_API_KEY=...
```

Install agent skills:

```sh
yaaml init
```

This writes skills into:

- `~/.codex/skills/yaaml`
- `~/.codex/skills/yaaml-remember`
- `~/.claude/skills/yaaml`
- `~/.claude/skills/yaaml-remember`

Install and start the background service:

```sh
yaaml service install
yaaml service start
```

On macOS this creates a LaunchAgent. On Linux it creates a systemd user unit. Service logs are written under `~/.yaaml/`.

You can also run the daemon directly:

```sh
yaaml daemon
```

## Everyday Use

Check daemon, database, provider, and backlog status:

```sh
yaaml status
```

Run explicit recall from a prompt:

```sh
yaaml recall --query "What should I remember before working on this repo?"
```

Store a durable memory manually:

```sh
yaaml remember \
  --title "Prefer concise repo READMEs" \
  --body "For this project, keep documentation practical and command-focused." \
  --scope project
```

Inspect the path YAAML uses to cache non-empty explicit recall results:

```sh
yaaml path
```

Ingest existing Codex transcripts once:

```sh
yaaml ingest
```

## Recall Metrics and Evals

YAAML evaluates recall along three axes:

1. Recall rate: how often recall runs and how often it returns at least one memory.
2. Recall volume: how many memories and characters are added when recall is non-empty.
3. Recall usefulness: whether recalled memories were relevant enough to help later agent work.

View the rollup:

```sh
yaaml stats
yaaml stats --json
```

Run and inspect evals:

```sh
yaaml eval recall
yaaml eval summary
yaaml eval list
yaaml eval memories
```

Numeric eval scores use a 1-5 scale where 5 is relevant, concise, and actionable, and 1 is not relevant. Empty recall is treated as abstention, not as a numeric failure: evals distinguish `clean_abstention` from `missed_useful_abstention`. Runs with no later transcript context are recorded as `insufficient_context` and excluded from numeric quality metrics.

## Configuration

YAAML merges configuration from:

1. `~/.yaaml/config.toml`
2. `.yaaml/config.toml` in the current project

Project config wins over user config. Print the merged configuration with:

```sh
yaaml config --effective
```

Common defaults include:

```toml
turns_between_memory = 10
recall_result_limit = 2
recall_candidate_pool = 16
recall_live_turn_window = 3
recall_similarity_threshold = 0.3
recall_memory_cooldown_seconds = 1200

embedding_provider = "openai"
embedding_model = "text-embedding-3-small"
embedding_api_key_env = "OPENAI_API_KEY"

summary_provider = "anthropic"
summary_model = "claude-haiku-4-5-20251001"
summary_api_key_env = "ANTHROPIC_API_KEY"
```

## CLI Reference

```text
yaaml daemon       Run the daemon process
yaaml init         Install YAAML skills and local setup
yaaml ingest       Ingest existing agent transcripts once
yaaml service      Manage the user service
yaaml eval         Run evaluation workflows
yaaml status       Show daemon, memory, backlog, and provider status
yaaml stats        Show recall coverage, volume, and usefulness metrics
yaaml config       Inspect configuration
yaaml tasks        Inspect or manage daemon tasks
yaaml memories     Inspect or rebuild stored memories
yaaml path         Print the explicit-recall cache path
yaaml recall       Run explicit recall or historical replay
yaaml remember     Store a concise durable memory
```

Use `--json` on supported commands for machine-readable output.

## Development

Run the full local quality gate:

```sh
just quality
```

That script checks formatting, typechecking, strict Clippy lints, tests, and coverage.

Useful development commands:

```sh
cargo test
cargo run -p yaaml -- status
cargo run -p yaaml -- config --effective
```
