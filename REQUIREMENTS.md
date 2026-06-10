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

The daemon watches both global root directories recursively via filesystem events (inotify on Linux, FSEvents on macOS). Agent transcript files are not stored inside the project checkout itself: Codex stores sessions under `~/.codex/sessions/...`, while Claude Code stores sessions under `~/.claude/projects/<encoded-path>/...`. No per-project hook configuration is required for observation. Project identity is derived from transcript `cwd` metadata, not from the transcript file path alone.

Claude Code subagent transcripts under `subagents/` are ignored by default for MVP because they are often lower-signal implementation detail and can add noise. Their files may still be referenced indirectly through the parent session transcript. A future option can enable subagent ingestion explicitly.

Each observed unit is a "turn pair": one user message + the subsequent assistant response, including all interleaved tool calls and tool results.

**Cursor model**: The daemon tracks its read position in each JSONL file via a `file_cursors` table in SQLite: `(file_path TEXT PRIMARY KEY, last_byte_offset INTEGER, last_processed_at TIMESTAMP)`. On each filesystem change event, the daemon reads only bytes from the stored offset to EOF, then updates the cursor. This ensures no lines are re-processed after daemon restarts and handles multiple concurrent sessions naturally — each file has its own independent cursor.

**Turn boundary detection** differs by agent:
- **Codex**: Explicit `EventMsg/TurnComplete` (or `TurnAborted`) marker in the JSONL. Unambiguous — no inference needed.
- **Claude Code**: Pattern-based — a turn is complete when an assistant message line appears whose `content` array contains only `text` blocks (no `tool_use` blocks), after any number of tool_use/tool_result cycles.

Both agents also support a `Stop` hook that fires after a turn completes. YAAML can optionally use this as a flush signal:
- **Claude Code `Stop` hook** payload: `session_id`, `cwd`, `transcript_path`, turn context.
- **Codex `Stop` hook** payload: `session_id`, `cwd`, `transcript_path`, `turn_id`, `last_assistant_message`.
The `transcript_path` field in both payloads is convenient for bootstrapping cursor tracking on new session files. Hook configuration is opt-in via `yaaml init`; file watching alone is sufficient for normal operation.

Turn content extracted for formulation: timestamp, role, text content, tool name + truncated output (for tool calls). Tool call content is read from the original transcript file at full fidelity and **truncated to a fixed character limit (default: 500 chars per tool call) at memory formulation time**. YAAML does not modify or duplicate the raw transcript.

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
- Memory formulation output is structured JSON with `title`, `body`, `scope`, and `project_descriptor`. Do not include a model-generated confidence field; source coverage, eval scores, and later recall usefulness are better quality signals.
- **Context scoping**: Memory formulation is per project by default: it uses observed turns for the same normalized `project_id` since that project's last memory was created. Memories are marked project-specific by default. The formulation prompt may mark a memory as global only when it captures durable user preferences, cross-project workflow patterns, or agent/tool behavior that is not project-specific.
- **Max context window**: A configurable token cap (default: 32,000 tokens) limits the input to the summarization model. When the window since the last memory exceeds this cap, turns are split into non-overlapping chunks processed oldest-to-newest. Each chunk after the first is prepended with a one-paragraph summary of all prior chunks in this formulation pass, providing continuity without re-sending the full prior content.
- **Backlog ingestion**: On first startup, the daemon discovers existing transcript JSONL files in the agent home directories and processes them in the background by default. Backlog processing is lower priority than new transcript writes and must not block live observation, memory recall, or CLI commands. Historical ingestion can also be rerun later via CLI.
- **Backlog progress tracking**: Backlog discovery and ingestion progress are persisted in SQLite so `yaaml status` can report discovered files, processed files, processed turns, queued memory-creation jobs, failures, and last activity across daemon restarts.
- **Backlog throttling**: Backlog jobs are rate limited separately from live observation. The daemon processes backlog newest-first, caps concurrent remote embedding/summary calls, and records retryable failures without blocking newer work. This avoids large first-run API spikes, rate-limit churn, and stale backlog work competing with current-session recall.

**1.4 Memory Storage**
- Memories are persisted to a local SQLite database at `~/.yaaml/yaaml.db` (user-global).
- Each memory record stores: id, title, body, scope (project/global), source_turn_refs[], created_at, updated_at, is_active, session_id, project_id, project_descriptor.
- **Project ID** is the absolute path of the working directory (`cwd`) at the time of the conversation, normalized (resolved symlinks, trailing slash stripped).
- **Project descriptor** is a compact human-readable string used for embedding and display, derived from local project metadata such as repo basename, normalized path basename, package/crate name, detected ecosystem, and a short formulation-derived project phrase (for example: `yaaml, Rust CLI memory daemon`). Absolute paths remain structured metadata and should not be the only project signal embedded into vectors. Git remotes and domain labels are not included by default.
- Vector embeddings are stored in a local vector index under `~/.yaaml/` (user-global). The implementation exposes a `VectorIndex` abstraction so the backend can change without affecting memory creation or recall. The MVP backend is an exact scan over embeddings stored in SQLite as `f32` vectors; Chroma compatibility is not required.
- Embedding provider and model are configurable (default provider: OpenAI; default model: `text-embedding-3-small`).
- The embedded text for a memory includes its title, body, scope, and compact project descriptor. Structured metadata is still used separately for ranking and display.

**1.5 Consolidation**
- After the dark period timer fires, a consolidation job runs asynchronously.
- DBSCAN-style clustering on memory embeddings identifies overlapping memories using cosine distance. Consolidation is scoped: global memories cluster only with global memories, and project memories cluster only with memories from the same `project_id`.
- Cluster ranking follows the Elroy approach: sort candidate clusters by larger cluster size first, then tighter mean intra-cluster distance. Large clusters are capped to the densest N memories before LLM consolidation.
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
- Query is derived from recent context embedded as a single compact synthetic document rather than raw JSONL. The default live query window is the last 3 completed turn pairs, capped by size. The query includes user text, assistant final text, tool names, command names, file paths touched, and short error snippets; it excludes long command output, large file reads, reasoning/private metadata, and unrelated transcript bookkeeping.
- Backlog and eval workflows may batch more turns into a single query/formulation window to process historical transcripts efficiently. Larger windows improve backlog throughput but can dilute recall specificity, so the live recall window and backlog formulation window are separately configurable.
- Result limit: top 4–5 memories by score (configurable, default: 5).
- Similarity threshold: cosine similarity ≥ 0.3 (configurable). OpenAI embeddings are normalized, so cosine and L2 produce equivalent rankings for those embeddings, but YAAML uses cosine terminology and thresholds explicitly.
- Inactive memories excluded.
- **Project weighting**: Recall retrieves global candidates by cosine similarity, then applies a bounded same-project reranking nudge. Same-project memories receive a small additive score bonus only when they are already semantically close; irrelevant same-project memories must not beat clearly relevant global or other-project memories. Memories from other projects may still be recalled when semantically relevant; recall output preserves the originating project when it differs from the current project.

**2.3 Recall Output File**
- Retrieved memories are written to a daemon-owned global per-project recall file under `~/.yaaml/recall/<project-hash>.md`. For daemon-triggered recall, the project hash is derived from the observed transcript metadata (`cwd` in Codex/Claude session or turn context). For manual `yaaml recall`, the project hash is derived from the CLI process cwd.
- Format: Markdown. Each memory is one section: title (heading), body, timestamp, originating project (if different from current).
- The recall file is overwritten when a recall run returns at least one memory.
- Metadata block at top: query timestamp, memory count, query source.
- **Persistence**: Recall file is preserved between sessions. A recall run with zero results is a noop and preserves the previous file contents.

**2.4 Agent Discovery of Recall File**
- Primary: a `yaaml-recall` skill installed by `yaaml init` for Claude Code and Codex. See §5.
- Secondary: `yaaml path` CLI command prints the recall file path.
- Agents should access recalled memory through the installed skill rather than assuming a project-local path. The skill resolves the current project to the correct global recall file and returns its contents.
- YAAML provides a CLI command to emit ready-made agent instruction snippets.

---

### 3. Session and Transcript Model

**3.1 Session Metadata**
- A session corresponds to a single JSONL file in the agent's transcript directory.
- Session record stores: id, agent_type (claude-code/codex), project_id, transcript_file_path, started_at, last_seen_at.
- Memories reference source session IDs and turn ranges for full lineage.

**3.2 Transcript References**
- YAAML does not duplicate full raw transcript events. Claude Code and Codex transcript JSONL files remain the source of truth for full-fidelity conversation content.
- Observed turn metadata is stored in a `turns` table: id, session_id, turn_id or ordinal, byte_start, byte_end, observed_at, status, and optional extracted display text for indexing/debugging.
- Tool call content is read from the original transcript byte range at memory formulation time and truncated only in the LLM input (default: 500 chars per tool call). The original transcript file is never modified.
- If an agent deletes or compacts a transcript file, YAAML retains memory records and source references but may no longer be able to reconstruct the full original turn content.

**3.3 Transcript Skill**
- A `yaaml-transcript` skill (installed by `yaaml init`) allows the agent to read back the referenced transcript content for a session or time range.
- Default scope when called with no args: current session (matched by `cwd` + recent timestamp).
- See §5 for skill details.

---

### 4. Async Architecture

**4.1 Background Workers**
- Memory creation, embedding indexing, consolidation, and recall all run in background worker processes or threads.
- Workers are managed by a long-running, always-on YAAML daemon (`yaaml daemon`). The daemon must be always-on to: (a) maintain filesystem watchers across sessions, (b) track the dark-period consolidation timer across session boundaries, (c) maintain file cursors.
- Only one daemon instance may run per user data directory. The daemon acquires a lock file under `~/.yaaml/` on startup, releases it on normal shutdown, and treats stale locks from interrupted processes as recoverable after verifying the owning process is gone.

**4.2 Observation Interface**
- Daemon watches `~/.claude/projects/` and `~/.codex/sessions/` via filesystem events (inotify/FSEvents). Watchers cover the full directory tree so new session files are picked up automatically.
- Also accepts lightweight `{session_id, timestamp}` signals via a Unix socket for the optional Stop hook integration. No full turn content is piped via the socket — the daemon reads the JSONL directly.

**4.3 Durability**
- Failed tasks (LLM errors, embedding errors) are retried with exponential backoff.
- Task state survives daemon restarts (tracked in SQLite).

**4.4 Schema Migration**
- Each SQLite database contains a `schema_version` table with a single integer row.
- On daemon startup, the current schema version is compared against the expected version for the running binary. If behind, migrations run sequentially in-process before the daemon accepts any work.
- Migration scripts are embedded in the binary (no external files required).
- Vector index schemas are versioned (e.g., `memories_v2`) so a schema-breaking change can coexist with the old index during migration, then the old index is dropped.
- The user-global `~/.yaaml/yaaml.db` follows this versioning scheme. Project-level databases are out of scope for MVP.

---

### 5. Skills

Installed by `yaaml init` into both `~/.claude/skills/` and `~/.codex/skills/` when those agent homes exist, with equivalent skill content for each agent.

**5.1 `yaaml-recall`**
- Resolves the current working directory to the corresponding global recall file under `~/.yaaml/recall/` and returns its contents.
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
- `yaaml init` — install skills, emit CLAUDE.md snippet, configure Stop hook.
- `yaaml ingest <file>` — ingest a plain-text or JSON conversation log.
- `yaaml recall [--query "..."]` — run recall and write the recall file.
- `yaaml daemon` — start the background worker.
- `yaaml status` — show memory count, last creation timestamp, last recall timestamp, backlog ingestion progress, active worker counts, and recent task failures. Supports `--json` for machine-readable output.
- `yaaml memories list [--project <path>] [--since <date>]` — tabular output: ID, title, project (basename), created date. Add `--verbose` to include memory body.
- `yaaml path` — print current recall file path.
- `yaaml service install|uninstall|start|stop|status` — install and manage the daemon as a user service (macOS LaunchAgent or Linux systemd user service). `service install` also runs the idempotent `init` setup if skills or hook snippets are missing.
- `yaaml eval recall [--project <path>] [--since <date>] [--limit N]` — replay historical transcript turns and evaluate whether recalled memories would have been useful for subsequent agent behavior.

---

### 7. Recall Evaluation

YAAML provides an offline evaluation workflow that replays historical transcripts without using future context in the recall query:

1. For each eligible historical turn, derive the recall query from only the context available before that turn.
2. Run recall against actual memories whose `created_at` is before that turn.
3. Compare the recalled memories to the subsequent transcript content: the user's next message, the assistant's next response, and relevant tool calls.
4. Record per-memory and per-turn evaluation results in SQLite for comparison across ranking strategies.

Initial evaluation metrics:
- **LLM judge score**: A configured judge model classifies each recalled memory as `useful`, `neutral`, or `distracting` for the subsequent transcript, with a short rationale.
- **Counterfactual citation score**: A judge checks whether the subsequent assistant response or tool plan used facts, commands, preferences, or project state present in the recalled memory but absent from the immediate pre-turn context.
- **Ranking comparison**: The eval command can compare ranking strategies such as no project bias, embedded project descriptor only, metadata tiebreak only, and combined project-aware reranking.

---

### 8. Configuration

All configuration lives in `~/.yaaml/config.toml` (user-level) with optional project overrides in `.yaaml/config.toml`.

| Key | Default | Description |
|-----|---------|-------------|
| `turns_between_memory` | `10` | Turn pairs before auto-creating a memory |
| `session_idle_memory_seconds` | `600` | Idle seconds after which a below-threshold session is flushed for memory formulation |
| `consolidation_dark_period_seconds` | `300` | Inactivity seconds before consolidation runs |
| `recall_result_limit` | `5` | Max memories returned per recall query |
| `recall_candidate_pool` | `20` | Candidates fetched before final trimming |
| `recall_live_turn_window` | `3` | Completed turn pairs included in live recall query construction |
| `recall_query_max_chars` | `12000` | Max characters in synthesized recall query text |
| `recall_similarity_threshold` | `0.3` | Cosine similarity cutoff for vector search |
| `recall_project_tiebreaker` | `true` | Prefer current-project memories when similarity scores are otherwise close |
| `recall_project_score_bonus` | `0.05` | Maximum additive reranking bonus for same-project memories |
| `recall_dir` | `~/.yaaml/recall` | Directory containing global per-project recall files |
| `db_path` | `~/.yaaml/yaaml.db` | SQLite database path |
| `vector_index_backend` | `sqlite-exact` | Vector index backend; MVP uses exact scan over SQLite-stored embeddings |
| `vector_index_path` | `~/.yaaml/vector-index` | Local vector index persistence path |
| `embedding_provider` | `openai` | Provider for embedding calls |
| `embedding_model` | `text-embedding-3-small` | Embedding model identifier |
| `embedding_api_key_env` | `OPENAI_API_KEY` | Environment variable containing the embedding provider API key |
| `embedding_base_url` | provider default | Optional override for embedding API base URL |
| `summary_provider` | `anthropic` | Provider for memory formulation calls |
| `summary_model` | `claude-haiku-4-5-20251001` | Fast model for memory formulation |
| `summary_api_key_env` | `ANTHROPIC_API_KEY` | Environment variable containing the summary provider API key |
| `summary_base_url` | provider default | Optional override for summary API base URL |
| `consolidation_provider` | `anthropic` | Provider for consolidation calls |
| `consolidation_model` | `claude-haiku-4-5-20251001` | Model for consolidation |
| `consolidation_api_key_env` | `ANTHROPIC_API_KEY` | Environment variable containing the consolidation provider API key |
| `consolidation_base_url` | provider default | Optional override for consolidation API base URL |
| `memory_cluster_distance_threshold` | `0.21125` | DBSCAN cosine-distance threshold for consolidation clustering |
| `memory_cluster_min_size` | `3` | Minimum memories required for a consolidation cluster |
| `memory_cluster_max_size` | `5` | Maximum densest memories sent to the consolidation LLM |
| `max_memory_length` | `12000` | Max characters per memory body |
| `max_formulation_tokens` | `32000` | Token cap for memory formulation input |
| `tool_call_truncation_chars` | `500` | Max chars per tool call at formulation time |
| `recall_classifier_enabled` | `true` | Gate recall with heuristics/LLM classifier |
| `backlog_max_concurrent_remote_jobs` | `1` | Max concurrent backlog embedding/summary calls |
| `backlog_newest_first` | `true` | Process discovered historical transcripts from newest to oldest |
| `backlog_formulation_turn_window` | `10` | Completed turn pairs batched into a backlog memory formulation job |
| `eval_judge_provider` | same as summary | Provider for recall relevance judging |
| `eval_judge_model` | same as summary | Model for recall relevance judging |

---

## Non-Functional Requirements

- **Latency**: Async operations must not add measurable latency to the agent's primary execution. Recall file write should complete within 2 seconds for <1000 memories.
- **Portability**: Default memory storage is local. LLM and embedding calls may use external providers configured by the user.
- **Privacy**: Memory records, transcript references, task state, and recall files are stored locally. Text sent for summarization, consolidation, and embedding may be sent to configured remote providers.
- **Idempotency**: Re-running `yaaml recall` with the same context produces the same recall file.
- **Observability**: Events logged to `~/.yaaml/yaaml.log`.

---

## Deferred / Future Work

- **Project ID in remote/container environments**: `cwd` as project_id breaks when a project is accessed from different paths (container, remote). Future: optional `project_alias` key in `.yaaml/config.toml` to override.
- **Memory delete**: `yaaml memories delete <id>` deferred to post-v1.
- **Web UI / Obsidian integration**: Out of scope for v1.
