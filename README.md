# YAAML

YAAML is "Yet Another Agent Memory Layer": a local, file-first memory system for AI coding agents such as Codex and Claude Code.

Instead of injecting recalled memories into an agent conversation, YAAML watches native agent transcripts, summarizes durable context into a local SQLite database, and materializes relevant memories as Markdown files under `~/.yaaml/recall`. Agents discover those files through installed skills or the `yaaml recall` command.

## What It Does

- Watches Codex and Claude Code transcript directories for completed turns.
- Stores durable memories in a user-global database at `~/.yaaml/yaaml.db`.
- Uses embeddings to recall memories relevant to the current project or prompt.
- Writes recall output to daemon-owned Markdown files instead of mutating the live chat context.
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

Print the current recall file for this project or session:

```sh
yaaml recall
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

Inspect the path YAAML will use for recall in the current working directory:

```sh
yaaml path
```

Ingest existing Codex transcripts once:

```sh
yaaml ingest
```

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
recall_result_limit = 3
recall_live_turn_window = 3
recall_similarity_threshold = 0.3

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
yaaml path         Print the current recall file path
yaaml recall       Print existing recall or update it from user input
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
