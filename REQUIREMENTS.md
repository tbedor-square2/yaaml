# YAAML: Yet Another Agent Memory Layer — Requirements

## Overview

YAAML is a memory layer for AI coding agents (Claude Code, Codex, etc.) that operates on a fundamentally different principle from existing systems like Elroy: rather than injecting recalled memories as synthetic messages into the agent's live context window, YAAML writes recalled content to a file on disk that the agent can reference explicitly. Both memory creation and recall are asynchronous processes that run outside the agent's main execution path.

### Core Design Philosophy

- **File-first recall**: Recalled memories are materialized as a file. The agent reads the file as it would any project file — no synthetic context injection, no polluted conversation history.
- **Async everything**: Memory creation and recall triggering happen in background processes, not in the hot path of an agent turn.
- **Agent-agnostic interface**: The file boundary makes YAAML usable by any agent that can read files (Claude Code, Codex, Cursor, etc.) without requiring SDK-level integration.
- **Passive observation**: YAAML observes agent activity by watching native transcript files on disk — no per-project hook configuration required for basic operation.
- **User-global, project-weighted**: Memories are stored globally per user but recall is weighted toward the current project context.

---

## Reference: Elroy Memory System

Elroy (the reference implementation) uses:
- **Dual storage**: ChromaDB for vector embeddings + SQLite/PostgreSQL for metadata
- **Creation trigger**: Auto-creates memory every N user messages (default: 10); uses a fast LLM to summarize recent conversation into a titled memory
- **Recall mechanism**: Two-stage gate (heuristics → LLM classifier) → vector search → inject as synthetic TOOL messages into the context window
- **Consolidation**: DBSCAN clustering on embeddings; merges similar memories via LLM; runs every M new memories (default: 5)
- **File backing (optional)**: Syncs memories to Markdown files for Obsidian integration

YAAML borrows the storage and creation architecture from Elroy but replaces the context injection step with file-based materialization.

---

## Functional Requirements

### 1. Memory Creation

**1.1 Observation Input**

YAAML observes agent conversations by watching native transcript files written by the agent runtime:

- **Claude Code**: Append-only JSONL files at `~/.claude/projects/<encoded-path>/<session-id>.jsonl`. Each line is a JSON object with `type`, `message.content` (may include `tool_use` and `tool_result` blocks), `uuid`, `timestamp`, `cwd`, `gitBranch`.
- **Codex CLI**: Append-only JSONL files at `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`. Per-event, immediate flush to disk (via a background async writer). The first line of each file is a `SessionMeta` record containing `id`, `cwd`, `model_provider`, `git.branch`, `cli_version`. Subsequent lines are `RolloutItem` variants: `EventMsg/UserMessage`, `ResponseItem/Message`, `ResponseItem/LocalShellCall` (and other tool types), `ResponseItem/FunctionCallOutput`, etc.

The daemon watches both root directories recursively via filesystem events (inotify on Linux, FSEvents on macOS). No per-project hook configuration is required for observation.

Each observed unit is a "turn pair": one user message + the subsequent assistant response, including all interleaved tool calls and tool results.

**Cursor model**: The daemon tracks its read position in each JSONL file via a `file_cursors` table in SQLite: `(file_path TEXT PRIMARY KEY, last_byte_offset INTEGER, last_processed_at TIMESTAMP)`. On each filesystem change event, the daemon reads only bytes from the stored offset to EOF, then updates the cursor. This ensures no lines are re-processed after daemon restarts and handles multiple concurrent sessions naturally — each file has its own independent cursor.

**Turn boundary detection** differs by agent:
- **Codex**: Explicit `EventMsg/TurnComplete` (or `TurnAborted`) marker in the JSONL. Unambiguous — no inference needed.
- **Claude Code**: Pattern-based — a turn is complete when an assistant message line appears whose `content` array contains only `text` blocks (no `tool_use` blocks), after any number of tool_use/tool_result cycles.

Both agents also support a `Stop` hook that fires after a turn completes. YAAML can optionally use this as a flush signal:
- **Claude Code `Stop` hook** payload: `session_id`, `cwd`, `transcript_path`, turn context.
- **Codex `Stop` hook** payload: `session_id`, `cwd`, `transcript_path`, `turn_id`, `last_assistant_message`.
The `transcript_path` field in both payloads is convenient for bootstrapping cursor tracking on new session files. Hook configuration is opt-in via `yaaml init`; file watching alone is sufficient for normal operation.

Turn content stored per line: timestamp, role, text content, tool name + truncated output (for tool calls). Tool call content is stored at full fidelity in the raw transcript table but **truncated to a fixed character limit (default: 500 chars per tool call) at memory formulation time** — the stored transcript is never modified.

**1.2 Creation Trigger**
- Memory creation is triggered automatically after a configurable number of observed turn pairs (default: 10).
- Memory creation may also be triggered manually via CLI or API.
- Creation runs asynchronously — it must not block the agent's response generation.
- **Consolidation trigger**: Consolidation runs after a configurable "dark period" — a fixed duration with no new observed messages (default: 5 minutes). Each new message resets the timer.

**1.3 Memory Formulation**
- A fast LLM call summarizes recent conversation turns into a titled memory.
- Memory title: concise, includes date when time-relevant (ISO 8601 format).
- Memory body: plain prose summary, max ~12,000 characters.
- Memory should capture facts, decisions, user preferences, and project state — not raw transcript.
- **Context scoping**: Memory formulation uses all turns since the last memory was created.
- **Max context window**: A configurable token cap (default: 32,000 tokens) limits the input to the summarization model. When the window since the last memory exceeds this cap, turns are processed oldest-to-newest in overlapping chunks, producing multiple memories if needed.
- **Backlog ingestion**: On first daemon startup, the user is prompted (via CLI) whether to ingest existing transcript history. If yes, YAAML processes all existing JSONL files in the agent home directories. This is a one-time operation.

**1.4 Memory Storage**
- Memories are persisted to a local SQLite database at `~/.yaaml/yaaml.db` (user-global).
- Each memory record stores: id, title, body, source_turn_ids[], created_at, updated_at, is_active, session_id, project_id.
- **Project ID** is the absolute path of the working directory (`cwd`) at the time of the conversation, normalized (resolved symlinks, trailing slash stripped).
- Vector embeddings are stored in ChromaDB at `~/.yaaml/chroma` (user-global).
- Embedding model is configurable (default: `text-embedding-3-small` or local equivalent).

**1.5 Consolidation**
- After the dark period timer fires, a consolidation job runs asynchronously.
- DBSCAN clustering on memory embeddings identifies overlapping memories. Similarity threshold: 0.92 cosine similarity (conservative — avoids over-merging distinct coding decisions that share surface similarity).
- An LLM merges clustered memories into a single consolidated memory.
- Original memories are marked inactive (soft delete) with lineage references preserved.

---

### 2. Memory Recall

**2.1 Recall Trigger**
- Recall is triggered on every agent turn, asynchronously and best-effort.
- **First turn**: Recall is skipped on the very first turn of a session — there is no context yet to embed. The recall file from a prior session (if any) is preserved and available, but no new recall query runs until the first turn completes.
- **Deduplication**: The daemon tracks which memory IDs are currently materialized in the recall file. If the top-N results for a new turn are identical to the current recall file contents, no rewrite occurs. Memories are only re-added to the recall file if they drop out and then become relevant again.
- A recall classifier (heuristics + optional LLM gate, configurable) may skip recall entirely when recent context is too short to generate a meaningful query.

**2.2 Recall Query**
- Recall uses vector similarity search against stored memory embeddings.
- Query is derived from recent context (last N turns) embedded as a single vector.
- Result limit: top 4–5 memories by score (configurable, default: 5).
- Similarity distance threshold: L2 ≤ 1.4 (configurable, matching Elroy default).
- Inactive memories excluded.
- **Project weighting**: After the initial vector search retrieves top-K candidates (default K=20), memories whose `project_id` matches the current working directory have their similarity score multiplied by a configurable boost factor (default: 1.3×). Results are then re-sorted and trimmed to the result limit. This is a simple post-hoc reranking step — no separate ChromaDB query required.

**2.3 Recall Output File**
- Retrieved memories are written to `.yaaml/recall.md` relative to the current working directory.
- Format: Markdown. Each memory is one section: title (heading), body, timestamp, originating project (if different from current).
- The recall file is overwritten on each recall run.
- Metadata block at top: query timestamp, memory count, query source.
- **Persistence**: Recall file is preserved between sessions. Cleared only when a new recall run produces results.

**2.4 Agent Discovery of Recall File**
- Primary: a `yaaml-recall` Claude Code skill installed by `yaaml init`. See §5.
- Secondary: `yaaml path` CLI command prints the recall file path.
- YAAML provides a CLI command to emit a ready-made `CLAUDE.md` snippet.

---

### 3. Session and Transcript Model

**3.1 Session Metadata**
- A session corresponds to a single JSONL file in the agent's transcript directory.
- Session record stores: id, agent_type (claude-code/codex), project_id, transcript_file_path, started_at, last_seen_at.
- Memories reference source session IDs and turn ranges for full lineage.

**3.2 Raw Transcript Table**
- All observed turns are stored verbatim (pre-truncation) in a `turns` table: id, session_id, role, content_json, observed_at.
- Tool call content is stored at full fidelity here; truncation is applied only at memory formulation time.

**3.3 Transcript Skill**
- A `yaaml-transcript` Claude Code skill (installed by `yaaml init`) allows the agent to read back the stored transcript for a session or time range.
- Default scope when called with no args: current session (matched by `cwd` + recent timestamp).
- See §5 for skill details.

---

### 4. Async Architecture

**4.1 Background Workers**
- Memory creation, embedding indexing, consolidation, and recall all run in background worker processes or threads.
- Workers are managed by a long-running, always-on YAAML daemon (`yaaml daemon`). The daemon must be always-on to: (a) maintain filesystem watchers across sessions, (b) track the dark-period consolidation timer across session boundaries, (c) maintain file cursors.

**4.2 Observation Interface**
- Daemon watches `~/.claude/projects/` and `~/.codex/sessions/` via filesystem events (inotify/FSEvents). Watchers cover the full directory tree so new session files are picked up automatically.
- Also accepts lightweight `{session_id, timestamp}` signals via a Unix socket for the optional Stop hook integration. No full turn content is piped via the socket — the daemon reads the JSONL directly.

**4.3 Durability**
- Failed tasks (LLM errors, embedding errors) are retried with exponential backoff.
- Task state survives daemon restarts (tracked in SQLite).

---

### 5. Skills

Installed by `yaaml init` into `~/.claude/skills/` (or equivalent per-agent skill directory).

**5.1 `yaaml-recall`**
- Reads `.yaaml/recall.md` in the current working directory and returns its contents.
- If the file does not exist or is empty, returns a message indicating no memories are available.
- Intended to be invoked at session start via a `CLAUDE.md` instruction or `UserPromptSubmit` hook.

**5.2 `yaaml-transcript`**
- Accepts optional args: `--session <id>`, `--since <ISO date>`, `--project <path>`.
- Default (no args): returns the transcript for the current session (matched by cwd + recent activity).
- Returns formatted Markdown of stored turns including tool calls.

---

### 6. Integration Points

**6.1 Claude Code**
- **Passive** (required): Daemon watches `~/.claude/projects/` for new JSONL entries. No configuration needed.
- **Active** (optional, improves turn boundary detection): A `Stop` hook in `settings.json` pipes a turn-complete signal to the daemon's Unix socket. `yaaml init` emits the hook config snippet.

**6.2 Codex CLI**
- **Passive** (required): Daemon watches `~/.codex/sessions/` recursively for new JSONL entries. Turn boundaries detected via explicit `TurnComplete` / `TurnAborted` events.
- **Active** (optional): Codex `Stop` hook (configured in `~/.codex/hooks.json` or `.codex/hooks.json`) fires after each turn and provides `transcript_path` directly — useful for bootstrapping cursor tracking. Note: the `Stop` hook may not fire reliably on Esc-interrupted turns (`TurnAborted` is written to JSONL instead); file watching handles this case correctly regardless.

**6.3 Generic CLI**
- `yaaml init` — install skills, emit CLAUDE.md snippet, configure Stop hook, prompt backlog ingestion.
- `yaaml ingest <file>` — ingest a plain-text or JSON conversation log.
- `yaaml recall [--query "..."]` — run recall and write the recall file.
- `yaaml daemon` — start the background worker.
- `yaaml status` — show memory count, last creation timestamp, last recall timestamp.
- `yaaml memories list` — list active memories with IDs, titles, and timestamps.
- `yaaml path` — print current recall file path.

---

### 7. Configuration

All configuration lives in `~/.yaaml/config.toml` (user-level) with optional project overrides in `.yaaml/config.toml`.

| Key | Default | Description |
|-----|---------|-------------|
| `turns_between_memory` | `10` | Turn pairs before auto-creating a memory |
| `consolidation_dark_period_seconds` | `300` | Inactivity seconds before consolidation runs |
| `recall_result_limit` | `5` | Max memories returned per recall query |
| `recall_candidate_pool` | `20` | Candidates fetched before project reranking |
| `recall_distance_threshold` | `1.4` | L2 distance cutoff for vector search |
| `recall_project_boost` | `1.3` | Score multiplier for current-project memories |
| `recall_file_path` | `.yaaml/recall.md` | Where recalled memories are written |
| `db_path` | `~/.yaaml/yaaml.db` | SQLite database path |
| `chroma_path` | `~/.yaaml/chroma` | ChromaDB persistence path |
| `embedding_model` | `text-embedding-3-small` | Embedding model identifier |
| `summary_model` | `claude-haiku-4-5-20251001` | Fast model for memory formulation |
| `consolidation_model` | `claude-haiku-4-5-20251001` | Model for consolidation |
| `max_memory_length` | `12000` | Max characters per memory body |
| `max_formulation_tokens` | `32000` | Token cap for memory formulation input |
| `tool_call_truncation_chars` | `500` | Max chars per tool call at formulation time |
| `recall_classifier_enabled` | `true` | Gate recall with heuristics/LLM classifier |

---

## Non-Functional Requirements

- **Latency**: Async operations must not add measurable latency to the agent's primary execution. Recall file write should complete within 2 seconds for <1000 memories.
- **Portability**: Default storage (SQLite + ChromaDB local) requires no external services. Local embedding model fallback supported.
- **Privacy**: All data stays local by default. LLM/embedding endpoints are configurable.
- **Idempotency**: Re-running `yaaml recall` with the same context produces the same recall file.
- **Observability**: Events logged to `~/.yaaml/yaaml.log`.

---

## Open Questions

### Memory Creation
1. **Formulation chunking strategy**: When the context since the last memory exceeds `max_formulation_tokens`, processing oldest-to-newest in chunks may produce redundant or overlapping memories. Should chunks use overlap (sliding window), or be non-overlapping with a brief summary of the prior chunk prepended as context?
2. **Project ID in remote/container environments**: `cwd` as project_id breaks when the same project is accessed from a container (different path) or remote environment. Is this a v1 concern, or should we add an optional `project_alias` config key to override?

### Architecture
5. **Schema migration**: What is the upgrade story for the SQLite schema and ChromaDB collections between YAAML versions? Options: (a) Alembic-style versioned migrations in SQLite; (b) version field in DB with migration scripts; (c) nuke-and-reindex on schema change (acceptable since source transcripts are preserved).

### Memory Quality
6. **Memory management CLI**: `yaaml memories list` to inspect active memories is desirable. Delete is out of scope for v1 — deferred.
