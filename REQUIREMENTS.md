# YAAML: Yet Another Agent Memory Layer — Requirements

## Overview

YAAML is a memory layer for AI coding agents (Claude Code, Codex, etc.) that operates on a fundamentally different principle from existing systems like Elroy: rather than injecting recalled memories as synthetic messages into the agent's live context window, YAAML writes recalled content to a file on disk that the agent can reference explicitly. Both memory creation and recall are asynchronous processes that run outside the agent's main execution path.

### Core Design Philosophy

- **File-first recall**: Recalled memories are materialized as a file. The agent reads the file as it would any project file — no synthetic context injection, no polluted conversation history.
- **Async everything**: Memory creation and recall triggering happen in background processes, not in the hot path of an agent turn.
- **Agent-agnostic interface**: The file boundary makes YAAML usable by any agent that can read files (Claude Code, Codex, Cursor, etc.) without requiring SDK-level integration.
- **Passive observation**: YAAML observes agent activity (conversations, tool calls, file edits) to build memories without the agent needing to call memory APIs explicitly.
- **User-global, project-weighted**: Memories are stored globally per user but recall is weighted toward the current project context.

---

## Reference: Elroy Memory System

Elroy (the reference implementation) uses:
- **Dual storage**: ChromaDB for vector embeddings + SQLite/PostgreSQL for metadata
- **Creation trigger**: Auto-creates memory every N user messages (default: 10); uses a fast LLM to summarize recent conversation into a titled memory
- **Recall mechanism**: Two-stage gate (heuristics → LLM classifier) → vector search → inject as synthetic TOOL messages into the context window
- **Consolidation**: DBSCAN clustering on embeddings; merges similar memories via LLM; runs every M new memories (default: 5)
- **File backing (optional)**: Syncs memories to Markdown files for Obsidian integration
- **Recall types**: Fast (formatted text in tool call) vs. reflective (LLM-generated first-person reflection)

YAAML borrows the storage and creation architecture from Elroy but replaces the context injection step with file-based materialization.

---

## Functional Requirements

### 1. Memory Creation

**1.1 Observation Input**
- YAAML must be able to observe agent conversation turns (user messages + assistant responses).
- YAAML must support multiple observation sources (Claude Code conversation logs, Codex interaction logs, generic stdin/stdout).
- Each observed unit is a "turn" consisting of: timestamp, role (user/assistant/tool), content, session_id, project_id.
- Tool calls (file reads, bash commands, etc.) are included in turn content but **truncated** to avoid bloating memory context. Truncation limit and strategy are TBD (see Open Questions).
- A turn pair is one user message + the subsequent assistant response (including any interleaved tool calls).

**1.2 Creation Trigger**
- Memory creation is triggered automatically after a configurable number of observed turn pairs (default: 10).
- Memory creation may also be triggered manually via CLI or API.
- Creation runs asynchronously — it must not block the agent's response generation.
- **Consolidation trigger**: Consolidation runs after a configurable "dark period" — a fixed duration with no new observed messages (default: 5 minutes). Each new message resets the timer. This models end-of-conversation consolidation rather than count-based triggering.

**1.3 Memory Formulation**
- A fast LLM call summarizes recent conversation turns into a titled memory.
- Memory title: concise, includes date when time-relevant (ISO 8601 format).
- Memory body: plain prose summary, max ~12,000 characters.
- Memory should capture facts, decisions, user preferences, and project state — not raw transcript.
- **Context scoping**: The memory formulation uses all turns since the last memory was created (unbounded window). For very long sessions this may produce large prompts; see Open Questions on truncation strategy.

**1.4 Memory Storage**
- Memories are persisted to a local SQLite database (portable, no external services required by default).
- Each memory record stores: id, title, body, source turn range, created_at, updated_at, is_active, **session_id**, **project_id**, **turn_ids[]** (lineage back to source turns).
- Memories are **user-global** — scoped to the user, not to a single project or session. Project and session metadata are stored as attributes for filtering and recall weighting.
- Vector embeddings (for semantic search) are stored in ChromaDB (local persistent instance by default).
- Embedding model is configurable (default: `text-embedding-3-small` or local equivalent).

**1.5 Consolidation**
- After the dark period timer fires (default: 5 min of inactivity), a consolidation job runs asynchronously.
- DBSCAN clustering on memory embeddings identifies overlapping memories.
- An LLM merges clustered memories into a single consolidated memory.
- Original memories are marked inactive (soft delete), not physically removed.
- Consolidated memory preserves lineage: references to all source memory ids.

---

### 2. Memory Recall

**2.1 Recall Trigger**
- Recall is triggered **on every agent turn**, asynchronously and best-effort (does not block the turn).
- A recall classifier (heuristics + optional LLM gate) may determine whether a recall is useful given recent context, to avoid unnecessary writes.

**2.2 Recall Query**
- Recall uses vector similarity search against stored memory embeddings.
- Query is derived from recent context (last N turns) embedded as a single vector.
- Configurable result limit (default: top 5 memories by similarity).
- Configurable similarity distance threshold (default: L2 ≤ 1.4, matching Elroy).
- Already-expired or inactive memories are excluded.
- **Project weighting**: Memories tagged with the current project_id are boosted in ranking relative to memories from other projects. Exact weighting strategy is TBD (see Open Questions).

**2.3 Recall Output File**
- Retrieved memories are written to a well-known file on disk (the "recall file").
- Default location: `.yaaml/recall.md` (relative to the project root or working directory).
- Format: Markdown, with one memory per section. Each section includes: memory title (as heading), body, timestamp, and originating project (if different from current).
- The recall file is overwritten on each recall run (not appended), so it always reflects current relevance.
- A metadata block at the top of the file records: query timestamp, number of memories retrieved, query source.
- **Persistence**: The recall file is preserved between sessions — it is not cleared on daemon startup. It is only overwritten when a new recall run completes.

**2.4 Agent Discovery of Recall File**
- The primary mechanism for surfacing the recall file to the agent is a **Claude Code skill** that the agent can invoke. The skill reads the recall file and returns its contents directly. See §5 (Skills).
- Secondary: a CLI flag `yaaml path` that prints the current recall file path.
- YAAML provides a CLI command to emit a ready-made `CLAUDE.md` snippet.

---

### 3. Session and Transcript Model

**3.1 Session Metadata**
- A "session" corresponds to a single continuous agent invocation (e.g., one `claude` process run).
- Sessions are not the primary unit of memory organization — memories are user-global. However, session_id is stored on all turns and memories for lineage and debugging.
- Each session record stores: id, started_at, ended_at (nullable), project_id, agent_type.

**3.2 Transcript Access**
- YAAML retains raw turn data (with truncated tool call content) for the lifetime of the database.
- A **transcript skill** allows the agent to read back the exact stored transcript for a session or time range. See §5 (Skills).
- Transcript access is read-only via the skill; raw turns are not modified after ingestion.

---

### 4. Async Architecture

**4.1 Background Workers**
- Memory creation, embedding indexing, consolidation, and recall all run in background worker processes or threads, separate from any agent process.
- Workers are started by a long-running YAAML daemon or invoked on-demand by a CLI command.

**4.2 Observation Interface**
- YAAML exposes a lightweight input interface (stdin pipe, local HTTP endpoint, or Unix socket) through which conversation turns can be submitted for processing.
- This avoids YAAML needing to parse proprietary agent log formats.

**4.3 Durability**
- Background tasks that fail (LLM errors, embedding errors) are retried with exponential backoff.
- Task state survives daemon restarts (tracked in SQLite).

---

### 5. Skills

YAAML ships two Claude Code skills (invocable via `/skill-name` in Claude Code):

**5.1 Recall Skill (`yaaml-recall`)**
- Reads the current `.yaaml/recall.md` file and surfaces it to the agent.
- If the recall file does not exist or is empty, the skill returns a message indicating no memories are available yet.
- Intended to be invoked at session start (e.g., via `CLAUDE.md` instruction or `UserPromptSubmit` hook).

**5.2 Transcript Skill (`yaaml-transcript`)**
- Accepts optional args: session id, date range, or project id.
- Returns the stored raw turn transcript for the specified scope.
- Useful for the agent to reconstruct prior context from a previous session.

---

### 6. Integration Points

**6.1 Claude Code**
- A Claude Code hook configuration (for `CLAUDE.md` or `settings.json`) automatically:
  - Pipes each completed conversation turn to the YAAML observation interface.
  - Triggers a recall run at session start and writes the recall file before the first agent turn.

**6.2 Codex**
- A similar integration for OpenAI Codex (or `codex` CLI), using Codex's equivalent hook mechanism.

**6.3 Generic CLI**
- `yaaml ingest <file>` — ingest a plain-text or JSON conversation log.
- `yaaml recall [--query "..."]` — run recall and write the recall file (optionally with an explicit query string instead of deriving from recent context).
- `yaaml daemon` — start the background worker.
- `yaaml status` — show memory count, last creation timestamp, last recall timestamp.
- `yaaml path` — print the current recall file path.

---

### 7. Configuration

All configuration lives in `.yaaml/config.toml` (project-level) or `~/.yaaml/config.toml` (user-level, lower precedence).

| Key | Default | Description |
|-----|---------|-------------|
| `turns_between_memory` | `10` | Turn pairs before auto-creating a memory |
| `consolidation_dark_period_seconds` | `300` | Inactivity seconds before consolidation runs |
| `recall_result_limit` | `5` | Max memories returned per recall query |
| `recall_distance_threshold` | `1.4` | L2 distance cutoff for vector search |
| `recall_project_weight_boost` | TBD | Score multiplier for current-project memories |
| `recall_file_path` | `.yaaml/recall.md` | Where recalled memories are written |
| `db_path` | `~/.yaaml/yaaml.db` | SQLite database path (user-global) |
| `chroma_path` | `~/.yaaml/chroma` | ChromaDB persistence path (user-global) |
| `embedding_model` | `text-embedding-3-small` | Embedding model identifier |
| `summary_model` | `claude-haiku-4-5-20251001` | Fast model for memory formulation |
| `consolidation_model` | `claude-haiku-4-5-20251001` | Model for consolidation |
| `max_memory_length` | `12000` | Max characters per memory body |
| `tool_call_truncation_chars` | TBD | Max chars per tool call in turn content |
| `recall_classifier_enabled` | `true` | Gate recall with heuristics/LLM classifier |

---

## Non-Functional Requirements

- **Latency**: Async operations must not add measurable latency to the agent's primary execution. The recall file write should complete in under 2 seconds for typical memory stores (<1000 memories).
- **Portability**: Default storage (SQLite + ChromaDB local) requires no external services. Users must be able to run YAAML on a laptop with no network access (using a local embedding model as fallback).
- **Privacy**: All data stays local by default. LLM calls for summarization/embedding go to configurable endpoints; users can point to local models.
- **Idempotency**: Re-running `yaaml recall` with the same context produces the same recall file (deterministic given fixed embeddings).
- **Observability**: YAAML logs memory creation, recall, and consolidation events to `~/.yaaml/yaaml.log`.

---

## Open Questions

### Memory Creation
1. **Tool call truncation**: What is the truncation limit and strategy for tool call content included in turns? Options: (a) fixed character cap (e.g., 500 chars) per tool call, (b) include only tool name + first line of output, (c) LLM-summarize long tool outputs before storing. Does truncation apply at ingestion time (stored truncated) or only at memory formulation time?
2. **Large context window for memory formulation**: When everything since the last memory is used, long sessions could generate very large prompts. Is there a hard cap on input tokens to the summarization model? If so, how is the window trimmed (oldest-first drop, or summarize-the-summary)?

### Recall
3. **Project weighting mechanism**: How is "project" identified for weighting purposes? Options: (a) git remote URL, (b) working directory path hash, (c) user-assigned project name in config. And how is the weight boost implemented — reranking after vector search, or a hybrid score?
4. **Per-turn recall performance**: Recall runs on every agent turn. For users with many memories (>1000), this could become slow. Is there a minimum interval between recall runs (e.g., no more than once per 30s)?
5. **Should recall include Agenda/reminder-style memories** (like Elroy's `trigger_context` items), or only general factual memories?

### Skills
6. **Skill distribution and installation**: How are the YAAML skills (`yaaml-recall`, `yaaml-transcript`) packaged and installed into Claude Code? Options: (a) shipped as files in the YAAML repo that users copy to `~/.claude/skills/`, (b) auto-installed by `yaaml init`, (c) emitted by a CLI command. What is the canonical install path?
7. **Transcript skill scope**: When the transcript skill is invoked with no args, what is the default scope — current session only, last N sessions, or all sessions for the current project?

### Integration
8. **How does YAAML observe Claude Code conversations?** Claude Code does not expose a public streaming API for conversation turns. Options: (a) parse transcript files from `~/.claude/projects/`, (b) use a `PostToolUse` or `Stop` hook to pipe the turn, (c) require the user to manually instrument their `CLAUDE.md`. Which is the intended mechanism?
9. **Should YAAML emit a Claude Code MCP server** so the agent can call `yaaml_recall(query)` as a tool and get memories inline? This would give more query flexibility but reintroduces context injection.

### Architecture
10. **Should the daemon be always-on or invoked per-session?** An always-on daemon simplifies real-time observation (especially for the dark-period consolidation timer) but adds system overhead. Per-session invocation is simpler but misses cross-session background consolidation.
11. **What is the migration/upgrade story for the SQLite schema and ChromaDB collections?** Elroy doesn't document this well; YAAML should.
12. **Is there a web UI or Obsidian integration like Elroy's?** Out of scope for v1, or a priority?

### Memory Quality
13. **How do we evaluate memory quality?** What signals indicate a memory is useful vs. noise? Elroy doesn't have explicit quality metrics — should YAAML?
14. **Should YAAML support explicit user feedback on memories** (e.g., "this memory is wrong", "delete this") via a CLI, and how does that feed back into the creation/consolidation pipeline?
15. **Should consolidation be more aggressive (merge everything above a threshold) or conservative (only merge near-duplicates)?** Elroy's DBSCAN threshold (0.85 cosine similarity) may be too aggressive for coding-specific memories where two superficially similar memories can record distinct decisions.
