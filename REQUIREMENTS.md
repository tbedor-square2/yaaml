# YAAML: Yet Another Agent Memory Layer — Requirements

## Overview

YAAML is a memory layer for AI coding agents (Claude Code, Codex, etc.) that operates on a fundamentally different principle from existing systems like Elroy: rather than injecting recalled memories as synthetic messages into the agent's live context window, YAAML writes recalled content to a file on disk that the agent can reference explicitly. Both memory creation and recall are asynchronous processes that run outside the agent's main execution path.

### Core Design Philosophy

- **File-first recall**: Recalled memories are materialized as a file. The agent reads the file as it would any project file — no synthetic context injection, no polluted conversation history.
- **Async everything**: Memory creation and recall triggering happen in background processes, not in the hot path of an agent turn.
- **Agent-agnostic interface**: The file boundary makes YAAML usable by any agent that can read files (Claude Code, Codex, Cursor, etc.) without requiring SDK-level integration.
- **Passive observation**: YAAML observes agent activity (conversations, tool calls, file edits) to build memories without the agent needing to call memory APIs explicitly.

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
- Each observed unit is a "turn" consisting of at minimum: timestamp, role (user/assistant), content.

**1.2 Creation Trigger**
- Memory creation is triggered automatically after a configurable number of observed turns (default: 10 user messages, matching Elroy).
- Memory creation may also be triggered manually via CLI or API.
- Creation runs asynchronously — it must not block the agent's response generation.

**1.3 Memory Formulation**
- A fast LLM call summarizes recent conversation turns into a titled memory.
- Memory title: concise, includes date when time-relevant (ISO 8601 format).
- Memory body: plain prose summary, max ~12,000 characters.
- Memory should capture facts, decisions, user preferences, and project state — not raw transcript.

**1.4 Memory Storage**
- Memories are persisted to a local SQLite database (portable, no external services required by default).
- Each memory record stores: id, title, body, source turn range, created_at, updated_at, is_active.
- Vector embeddings (for semantic search) are stored in ChromaDB (local persistent instance by default).
- Embedding model is configurable (default: `text-embedding-3-small` or local equivalent).

**1.5 Consolidation**
- After every M new memories (default: 5), a consolidation job runs asynchronously.
- DBSCAN clustering on memory embeddings identifies overlapping memories.
- An LLM merges clustered memories into a single consolidated memory.
- Original memories are marked inactive (soft delete), not physically removed.

---

### 2. Memory Recall

**2.1 Recall Trigger**
- Recall is triggered asynchronously, either on a schedule (e.g., at the start of a session) or by an explicit query.
- Recall does not run per-turn (unlike Elroy's inline recall) to avoid adding latency.
- A recall classifier (heuristics + optional LLM gate) may determine whether a recall is useful given recent context.

**2.2 Recall Query**
- Recall uses vector similarity search against stored memory embeddings.
- Query is derived from recent context (last N turns) embedded as a single vector.
- Configurable result limit (default: top 5 memories by similarity).
- Configurable similarity distance threshold (default: L2 ≤ 1.4, matching Elroy).
- Already-expired or inactive memories are excluded.

**2.3 Recall Output File**
- Retrieved memories are written to a well-known file on disk (the "recall file").
- Default location: `.yaaml/recall.md` (relative to the project root or working directory).
- Format: Markdown, with one memory per section. Each section includes: memory title (as heading), body, and timestamp.
- The recall file is overwritten on each recall run (not appended), so it always reflects current relevance.
- A metadata block at the top of the file records: query timestamp, number of memories retrieved, query source.

**2.4 Agent Discovery of Recall File**
- The recall file path must be surfaced to the agent via at least one standard mechanism:
  - A line in `CLAUDE.md` or `AGENTS.md` instructing the agent to read the file at session start.
  - A Claude Code hook that injects the file path into context (e.g., `UserPromptSubmit` hook writing a note).
  - A CLI flag that prints the path.
- YAAML provides a CLI command to emit a ready-made `CLAUDE.md` snippet that agents can include.

---

### 3. Async Architecture

**3.1 Background Workers**
- Memory creation, embedding indexing, consolidation, and recall all run in background worker processes or threads, separate from any agent process.
- Workers are started by a long-running YAAML daemon or invoked on-demand by a CLI command.

**3.2 Observation Interface**
- YAAML exposes a lightweight input interface (stdin pipe, local HTTP endpoint, or Unix socket) through which conversation turns can be submitted for processing.
- This avoids YAAML needing to parse proprietary agent log formats.

**3.3 Durability**
- Background tasks that fail (LLM errors, embedding errors) are retried with exponential backoff.
- Task state survives daemon restarts (tracked in SQLite).

---

### 4. Integration Points

**4.1 Claude Code**
- A Claude Code hook configuration (for `CLAUDE.md` or `settings.json`) automatically:
  - Pipes each completed conversation turn to the YAAML observation interface.
  - Triggers a recall run at session start and writes the recall file before the first agent turn.

**4.2 Codex**
- A similar integration for OpenAI Codex (or `codex` CLI), using Codex's equivalent hook mechanism.

**4.3 Generic CLI**
- `yaaml ingest <file>` — ingest a plain-text or JSON conversation log.
- `yaaml recall [--query "..."]` — run recall and write the recall file (optionally with an explicit query string instead of deriving from recent context).
- `yaaml daemon` — start the background worker.
- `yaaml status` — show memory count, last creation timestamp, last recall timestamp.

---

### 5. Configuration

All configuration lives in `.yaaml/config.toml` (project-level) or `~/.yaaml/config.toml` (user-level, lower precedence).

| Key | Default | Description |
|-----|---------|-------------|
| `turns_between_memory` | `10` | User turns before auto-creating a memory |
| `memories_between_consolidation` | `5` | Memories created before consolidation runs |
| `recall_result_limit` | `5` | Max memories returned per recall query |
| `recall_distance_threshold` | `1.4` | L2 distance cutoff for vector search |
| `recall_file_path` | `.yaaml/recall.md` | Where recalled memories are written |
| `db_path` | `.yaaml/yaaml.db` | SQLite database path |
| `chroma_path` | `.yaaml/chroma` | ChromaDB persistence path |
| `embedding_model` | `text-embedding-3-small` | Embedding model identifier |
| `summary_model` | `claude-haiku-4-5-20251001` | Fast model for memory formulation |
| `consolidation_model` | `claude-haiku-4-5-20251001` | Model for consolidation |
| `max_memory_length` | `12000` | Max characters per memory body |
| `recall_classifier_enabled` | `true` | Gate recall with heuristics/LLM classifier |

---

## Non-Functional Requirements

- **Latency**: Async operations must not add measurable latency to the agent's primary execution. The recall file write should complete in under 2 seconds for typical memory stores (<1000 memories).
- **Portability**: Default storage (SQLite + ChromaDB local) requires no external services. Users must be able to run YAAML on a laptop with no network access (using a local embedding model as fallback).
- **Privacy**: All data stays local by default. LLM calls for summarization/embedding go to configurable endpoints; users can point to local models.
- **Idempotency**: Re-running `yaaml recall` with the same context produces the same recall file (deterministic given fixed embeddings).
- **Observability**: YAAML logs memory creation, recall, and consolidation events to `.yaaml/yaaml.log`.

---

## Open Questions

### Memory Creation
1. **What counts as a "turn" for purposes of the creation counter?** Elroy counts user messages; should YAAML count user+assistant pairs, or only user messages? Should tool calls (file reads, bash commands) count?
2. **How is "recent context" scoped for memory formulation?** Do we use the last N turns (fixed window), or everything since the last memory was created? For long sessions, the latter can be very large.
3. **Should memories be project-scoped or user-global?** Elroy scopes to a user. YAAML could scope to a git repo, a working directory, or the user globally. These have very different recall properties.

### Recall
4. **When exactly does recall run?** Options: (a) once at session start, (b) on every agent turn (async, best-effort), (c) on explicit agent request only, (d) on a timer. Each has different freshness/staleness tradeoffs.
5. **Does the agent need to explicitly re-read the recall file per turn, or is it loaded once at session start?** If once, stale recalls are a risk for long sessions. If per-turn, it adds file I/O overhead and requires the agent to always re-check.
6. **What happens to the recall file between sessions?** Should it be cleared on daemon startup, preserved until next recall run, or versioned?
7. **How does the recall query work when there is no recent conversation context yet (e.g., at session start)?** Should we use the working directory + open files as a proxy query?
8. **Should recall include AgendaItem/reminder-style memories (like Elroy's `trigger_context` items), or only general memories?**

### Integration
9. **How does YAAML observe Claude Code conversations?** Claude Code does not expose a public streaming API for conversation turns. Options: (a) parse transcript files from `~/.claude/projects/`, (b) use a `PostToolUse` or `Stop` hook to pipe the turn, (c) require the user to manually instrument their `CLAUDE.md`. Which is the intended mechanism?
10. **How does the recall file get surfaced to Claude Code?** Options: (a) always-on `CLAUDE.md` include, (b) `UserPromptSubmit` hook that injects a "read `.yaaml/recall.md` if it exists" instruction, (c) agent must be trained/prompted to look for it. What's the minimal-friction path?
11. **Should YAAML emit a Claude Code MCP server** so the agent can call `yaaml_recall(query)` as a tool and get memories inline? This would give more query flexibility but reintroduces context injection.

### Architecture
12. **Should the daemon be always-on or invoked per-session?** An always-on daemon simplifies real-time observation but adds system overhead. Per-session invocation is simpler but misses cross-session background consolidation.
13. **What is the migration/upgrade story for the SQLite schema and ChromaDB collections?** Elroy doesn't document this well; YAAML should.
14. **How do we handle multiple concurrent projects (different working directories) with a single user-level daemon?** Does each project get its own DB and ChromaDB collection, or is there one global store with project tags?
15. **Is there a web UI or Obsidian integration like Elroy's?** Out of scope for v1, or a priority?

### Memory Quality
16. **How do we evaluate memory quality?** What signals indicate a memory is useful vs. noise? Elroy doesn't have explicit quality metrics — should YAAML?
17. **Should YAAML support explicit user feedback on memories** (e.g., "this memory is wrong", "delete this") via a CLI, and how does that feed back into the creation/consolidation pipeline?
18. **Should consolidation be more aggressive (merge everything above a threshold) or conservative (only merge near-duplicates)?** Elroy's DBSCAN threshold (0.85 cosine similarity) may be too aggressive for coding-specific memories where two superficially similar memories can record distinct decisions.
