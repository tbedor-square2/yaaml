# YAAML — Yet Another Agent Memory Layer

YAAML gives AI coding agents (Claude Code, Codex) persistent memory across sessions without touching their context windows. It watches native transcript files on disk, builds memories from completed conversations, and materializes relevant memories into a plain Markdown file that any agent can read.

## How it works

```
~/.claude/projects/**/*.jsonl   ──┐
~/.codex/sessions/**/*.jsonl    ──┤  FileWatcher (inotify/FSEvents)
                                  │
                          ┌───────▼────────┐
                          │  YAAML daemon  │
                          │                │
                          │  parse turns   │
                          │  embed + store │  ──▶  ~/.yaaml/yaaml.db
                          │  recall query  │  ──▶  ~/.yaaml/chroma/
                          │  consolidate   │
                          └───────┬────────┘
                                  │
                          {project}/.yaaml/recall.md   ◀── agent reads this
```

- **Passive observation** — no hooks or SDK changes required; the daemon tails transcript files using byte-offset cursors.
- **File-first recall** — retrieved memories are written to `.yaaml/recall.md` in your project directory. The agent reads it like any other file; nothing is injected into the context window.
- **Async everywhere** — memory creation, embedding, recall, and consolidation all run in the background and never block agent responses.
- **User-global, project-weighted** — memories are stored globally but recall boosts results from the current project.

## Requirements

- Python ≥ 3.11
- [uv](https://docs.astral.sh/uv/)
- An `ANTHROPIC_API_KEY` for memory summarisation and consolidation
- An `OPENAI_API_KEY` for `text-embedding-3-small` (optional — falls back to local sentence-transformers)

## Installation

```bash
git clone https://github.com/tombedor/yaaml
cd yaaml
uv sync
uv run yaaml init
```

`yaaml init` will:
1. Create `~/.yaaml/` and initialise the SQLite database
2. Install the `yaaml-recall` and `yaaml-transcript` skills to `~/.claude/skills/`
3. Print a `CLAUDE.md` snippet to paste into your project
4. Optionally configure a Claude Code `Stop` hook for tighter turn-boundary detection
5. Optionally ingest your existing transcript history

## Quick start

```bash
# Start the always-on daemon (keep this running)
yaaml daemon

# In another terminal, check status
yaaml status

# Run a one-shot recall for the current project
yaaml recall

# List stored memories
yaaml memories list
yaaml memories list --project ~/myproject --verbose
```

Add this to your project's `CLAUDE.md` so the agent reads memories at session start:

```markdown
## Memory

At the start of each session, run `yaaml recall --project $PWD`, then read `.yaaml/recall.md`.
```

## CLI reference

| Command | Description |
|---|---|
| `yaaml daemon` | Start the background daemon (always-on) |
| `yaaml init` | First-time setup: dirs, skills, hook, backlog |
| `yaaml recall [--query TEXT] [--project PATH]` | One-shot recall → writes `.yaaml/recall.md` |
| `yaaml status` | Memory count, last creation/recall, daemon PID |
| `yaaml memories list [--project PATH] [--since DATE] [--verbose]` | Tabular memory browser |
| `yaaml path [--project PATH]` | Print the recall file path |
| `yaaml ingest FILE` | Ingest a JSONL transcript file manually |
| `yaaml simulate [--agent claude-code\|codex] [--turns N]` | Generate a synthetic session for testing |

## Configuration

YAAML reads `~/.yaaml/config.toml` (user-level) then overlays `.yaaml/config.toml` from the project directory. All keys are optional.

```toml
# ~/.yaaml/config.toml

turns_between_memory = 10          # turn pairs before creating a memory
consolidation_dark_period_seconds = 300  # inactivity before consolidation runs

recall_result_limit = 5            # max memories per recall query
recall_distance_threshold = 1.4    # L2 distance cutoff
recall_project_boost = 1.3         # score multiplier for current-project memories

embedding_model = "text-embedding-3-small"
summary_model = "claude-haiku-4-5-20251001"
consolidation_model = "claude-haiku-4-5-20251001"

max_formulation_tokens = 32000
tool_call_truncation_chars = 500
```

## Architecture

| Component | File | What it does |
|---|---|---|
| Config | `config.py` | TOML loader with user + project overlay |
| Schema | `db.py` | SQLite v1 schema, versioned migration runner |
| Parsers | `parsers.py` | Claude Code (content-pattern) + Codex (`TurnComplete`) JSONL parsers |
| Watcher | `watcher.py` | watchdog → asyncio queue, per-file byte cursors |
| LLM | `llm.py` | Anthropic async calls for summarisation + consolidation |
| Embeddings | `embeddings.py` | ChromaDB `memories_v1` collection |
| Memory | `memory.py` | Turn counter, memory creation, ChromaDB indexing |
| Recall | `recall.py` | Vector search + project boost + dedup → `recall.md` |
| Consolidation | `consolidation.py` | DBSCAN (eps=0.08, cosine ≈ 0.92 similarity) + LLM merge |
| Daemon | `daemon.py` | Wires everything; dark-period timer; backlog on startup |
| CLI | `cli.py` | Typer app |
| Harness | `harness.py` | Test harness: synthetic Claude Code + Codex JSONL generation |

### Transcript observation

The daemon watches two directories:

- **Claude Code**: `~/.claude/projects/<encoded-path>/<session-id>.jsonl` — append-only JSONL, one line per event. Turn boundary detected when an assistant message with text-only content (no `tool_use` blocks) is written.
- **Codex**: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` — immediate per-event flush. Turn boundary from explicit `EventMsg/TurnComplete` line.

Each file gets a byte-offset cursor in SQLite. On restart the daemon resumes from where it left off; no events are re-processed.

Override watch paths for testing:

```bash
YAAML_CLAUDE_WATCH_PATH=/tmp/fake-claude yaaml daemon
YAAML_CODEX_WATCH_PATH=/tmp/fake-codex  yaaml daemon
```

### Memory lifecycle

```
turns (raw, full-fidelity)
    │  every N=10 turn pairs
    ▼
memories (LLM summary, title + body)
    │  stored in SQLite + ChromaDB
    │  every 5-min dark period
    ▼
consolidation (DBSCAN clusters → LLM merge → soft-delete sources)
```

## Development

```bash
uv sync --dev

just check    # ruff + mypy + pytest (25 tests)
just fmt      # auto-format and fix
just test     # tests only
just lint     # ruff only
just typecheck  # mypy only
```

### Test harness

`src/yaaml/harness.py` lets you write synthetic sessions without a running agent:

```python
from yaaml.harness import ClaudeCodeSessionWriter, FakeTurn, FakeToolCall, generate_session, LoremGenerator
from pathlib import Path

writer = ClaudeCodeSessionWriter(
    session_dir=Path("/tmp/test-session"),
    project_cwd="/home/user/myproject",
)
lorem = LoremGenerator(seed=42)
generate_session(writer, n_turns=5, lorem=lorem)
# → /tmp/test-session/<uuid>.jsonl with 5 turns
```

Or use `yaaml simulate` to write a session to a live-watched directory:

```bash
YAAML_CLAUDE_WATCH_PATH=/tmp/fake-claude yaaml daemon &
yaaml simulate --agent claude-code --turns 20 --output-dir /tmp/fake-claude/projects/test
```
