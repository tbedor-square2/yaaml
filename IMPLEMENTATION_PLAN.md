# YAAML Rust Implementation Plan

## Goals

Build YAAML as a Rust CLI and daemon that observes agent transcript files, creates local memories through remote model providers, stores memory metadata and embeddings locally, and materializes recall into daemon-owned global per-project files under `~/.yaaml/recall/`.

Implementation starts Codex-first, then adds Claude Code once the core transcript and memory pipeline is stable.

## Workspace Layout

Use a Rust workspace with a small crate set up front. Keep higher-level features as modules until they justify separate crates.

| Crate | Responsibility |
| --- | --- |
| `yaaml` | Binary crate. CLI entrypoint and `clap` command wiring only. Commands: `daemon`, `init`, `status`, `recall`, `memories`, `ingest`, `service`, `eval recall`. |
| `yaaml-core` | Shared domain types and utilities: config, paths, agent/session/turn/memory structs, project normalization, project descriptor derivation, errors, time helpers. |
| `yaaml-store` | SQLite access with `rusqlite`: migrations, repositories, daemon lock, file cursors, session/turn/memory tables, task queues, backlog progress, eval results, embedding blob persistence. |
| `yaaml-transcript` | Transcript discovery and parsing. MVP is Codex JSONL parsing and turn-pair assembly. Later adds Claude Code parsing while ignoring `subagents/` by default. |
| `yaaml-llm` | Remote provider clients: OpenAI embeddings, Anthropic summary/consolidation/judge calls, retry classification, provider config. |

Initial modules inside `yaaml-core` or a future `yaaml-engine` crate:

| Module | Responsibility |
| --- | --- |
| `memory` | Memory formulation, chunking, structured JSON output parsing, project/global scope handling, source turn references. |
| `recall` | `VectorIndex` trait, SQLite exact vector backend, cosine similarity, project-aware reranking, global per-project recall file rendering/path resolution. |
| `daemon` | Watchers, background workers, Unix socket, backlog scheduler, graceful shutdown. |
| `skills` | Copy Claude/Codex skill directories, emit snippets, optional hook config. |
| `service` | macOS LaunchAgent and Linux systemd user-service install/start/stop/status. |
| `eval` | Historical recall replay using actual memories only, judge scoring, ranking comparisons. |

## Core Data Model

The SQLite database lives at `~/.yaaml/yaaml.db`. Project-level databases are out of scope for MVP.

Key tables:

| Table | Purpose |
| --- | --- |
| `schema_version` | Single-row migration version. |
| `daemon_lock` or lock file metadata | Records owner PID/process metadata for stale-lock recovery. |
| `file_cursors` | One cursor per transcript file: path, byte offset, last processed time. |
| `sessions` | One row per JSONL transcript file: agent type, transcript path, project id, started/last seen timestamps. |
| `turns` | One row per complete turn pair: session id, turn id/ordinal, byte start/end, observed time, status, optional extracted display text. |
| `memories` | Title, body, scope, project id, project descriptor, source turn refs, lineage, active flag, timestamps. |
| `embeddings` | Memory id, embedding model, vector dimension, `f32` blob, embedded text hash. |
| `tasks` | Durable background task queue for formulation, embedding, consolidation, recall, backlog, eval. |
| `backlog_progress` | Discovered files, processed files/turns, queued jobs, failures, last activity. |
| `eval_runs`, `eval_results` | Recall replay runs and per-turn/per-memory judge results. |

## Vector Index

Define a `VectorIndex` trait so the backend can change later without affecting recall:

```rust
trait VectorIndex {
    fn upsert(&self, memory_id: MemoryId, embedding: &[f32], embedded_text_hash: &str) -> Result<()>;
    fn remove(&self, memory_id: MemoryId) -> Result<()>;
    fn search(&self, query: &[f32], limit: usize, threshold: f32) -> Result<Vec<VectorHit>>;
}
```

MVP backend: `sqlite-exact`.

The backend stores embeddings as `f32` blobs in SQLite and performs exact cosine similarity scans in Rust. This is stable, dependency-light, and adequate for expected local memory counts. Indexed backends can be added later behind the same trait.

## Transcript Strategy

### Codex First

Start with Codex transcript files:

```text
~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl
```

Codex turn boundaries are explicit via `TurnComplete` / `TurnAborted`, making this the lowest-risk parser target.

The parser should:

- Read from the byte offset stored in `file_cursors`.
- Parse line-delimited JSON incrementally.
- Detect session metadata from the first `session_meta` record.
- Track current turn context and byte start/end.
- Store one `turns` row per complete turn pair.
- Preserve byte ranges into the original transcript rather than duplicating raw transcript JSON.

### Claude Later

Add Claude Code after Codex pipeline tests are green:

```text
~/.claude/projects/<encoded-path>/<session-id>.jsonl
```

Claude `subagents/` JSONL files are ignored by default for MVP.

## Backlog Behavior

Backlog ingestion runs automatically in the background on first daemon startup.

Rules:

- Process newest sessions first.
- Live transcript writes always outrank backlog work.
- Cap backlog remote calls with `backlog_max_concurrent_remote_jobs`.
- Align backlog memory formulation with session boundaries when possible.
- Prefer small, granular memories. A 10-turn backlog batch may produce multiple memories if the content covers separable topics.
- If a session has fewer than `backlog_formulation_turn_window` turns, create memory after the session appears dead.

Session-dead detection:

- A session is considered idle when no new transcript bytes have appeared for `session_idle_memory_seconds` (default: 600).
- When idle, if there are unformulated turns since the last memory for that session/project, queue a formulation job even if the turn count is below the normal threshold.
- Codex explicit turn completion still defines turn boundaries; the idle timer only decides when to flush partial accumulated memory context.

## Recall Query Strategy

Live recall uses a compact synthetic query document derived from recent context:

- Default: last 3 completed turn pairs.
- Include user text, assistant final text, tool names, command names, file paths touched, and short error snippets.
- Exclude long command output, large file reads, reasoning/private metadata, and transcript bookkeeping.
- Cap with `recall_query_max_chars`.

Backlog formulation may batch more turns because it creates memories, not live recall output. Live recall should stay tighter to preserve relevance.

## Service Strategy

Add service management commands:

```text
yaaml service install
yaaml service uninstall
yaaml service start
yaaml service stop
yaaml service status
```

Implement:

- macOS: user LaunchAgent under `~/Library/LaunchAgents`.
- Linux: systemd user service.
- `service install` runs the same idempotent setup as `yaaml init` when required, so installing the service also ensures Claude/Codex skills and snippets are present.

Daemon safety:

- One daemon per user data directory.
- Lock file under `~/.yaaml/`.
- Lock records PID and startup metadata.
- Stale locks are recoverable after verifying the owning process is gone.
- SIGINT/SIGTERM trigger graceful shutdown: persist task state, finish/rollback active DB transaction, close watchers, release lock.

## Provider Failure Handling

Missing API keys or remote failures must not stop observation.

Behavior:

- Daemon enters observation-only mode for unavailable providers.
- Failed summary/embedding/consolidation/eval jobs retry with exponential backoff.
- After a max retry count, jobs are parked.
- `yaaml status` and `yaaml status --json` show parked jobs and recent failures.
- V1 does not include a manual `jobs retry` command. Parked jobs remain visible through status.

## Consolidation

Use the Elroy-style clustering approach, implemented directly in Rust:

- Pairwise cosine-distance matrix over active memory embeddings.
- DBSCAN-style clustering with `memory_cluster_distance_threshold`.
- Scope partitioning before clustering:
  - Global memories cluster only with global memories.
  - Project memories cluster only with memories from the same `project_id`.
- Sort clusters by larger size first, then lower mean intra-cluster distance.
- Cap large clusters to densest `memory_cluster_max_size` memories.
- Send capped cluster to the consolidation model.
- Create consolidated memory and mark source memories inactive with lineage preserved.

No external clustering crate is required for MVP.

## Milestones

### Milestone 1: Workspace, Config, Store

Deliver:

- Rust workspace and crate skeleton.
- Config loading from `~/.yaaml/config.toml` plus optional project overrides.
- SQLite migrations through `rusqlite`.
- Core domain structs.
- `yaaml status` and `yaaml status --json`.
- Daemon lock acquisition/release and stale lock detection.

Tests:

- Unit: config defaults match `REQUIREMENTS.md`.
- Unit: config project override precedence.
- Unit: migrations create expected tables.
- Unit: daemon lock rejects a second active owner.
- Unit: stale lock is recoverable when PID is gone.
- CLI: `yaaml status --json` emits valid JSON against an empty DB.

### Milestone 2: Codex Transcript Parser

Deliver:

- Codex transcript file discovery.
- Incremental JSONL reads from cursor offsets.
- Session metadata persistence.
- Turn-pair assembly from explicit Codex turn boundaries.
- One `turns` row per complete turn pair.

Tests:

- Fixture: parse Codex `session_meta`.
- Fixture: assemble one complete turn with tool calls and outputs.
- Fixture: `TurnAborted` stores a turn with aborted status.
- Unit: cursor resumes without duplicating turns.
- Unit: byte ranges re-read original transcript content exactly.
- Unit: malformed JSONL line records failure without advancing past unread safe offset.

### Milestone 3: Backlog Scheduler

Deliver:

- Backlog discovery on first daemon startup.
- Newest-first processing.
- Live work priority over backlog.
- Session-boundary aligned formulation windows.
- Idle-session flush for sessions below threshold.
- Backlog progress persistence.

Tests:

- Unit: backlog discovery ignores already-cursored files.
- Unit: newest-first ordering.
- Unit: live task priority beats backlog tasks.
- Unit: session with fewer than threshold turns queues formulation after idle timeout.
- Unit: 10-turn batch can produce multiple memory creation requests from structured model output.
- CLI: `yaaml status --json` reports backlog discovered/processed/failed counts.

### Milestone 4: Provider Layer

Deliver:

- OpenAI embedding client.
- Anthropic summary/consolidation/judge client.
- API key env var handling.
- Retry classification and parked job state.
- Observation-only mode when providers are unavailable.

Tests:

- Unit: missing API key parks remote job but does not fail transcript ingestion.
- Unit: retryable HTTP status schedules backoff.
- Unit: non-retryable provider error parks after max retries.
- Contract/mock: OpenAI embedding response parses vector dimensions.
- Contract/mock: Anthropic structured JSON response parses memory output.

### Milestone 5: Memory Creation

Deliver:

- Project descriptor derivation from local metadata.
- Per-project formulation context.
- Structured memory JSON output parsing: `title`, `body`, `scope`, `project_descriptor`.
- No confidence field.
- Source turn references.
- Multiple memories from one batch.
- Embedding text construction and embedding persistence.

Tests:

- Unit: project descriptor does not include git remote/domain by default.
- Unit: global scope accepted only as `global`; project scope default applied when absent/invalid.
- Unit: memory body length cap enforced.
- Unit: multiple memories from one formulation response persist separately with shared source refs.
- Unit: embedding text includes title, body, scope, and project descriptor.

### Milestone 6: VectorIndex and Recall

Deliver:

- `VectorIndex` trait.
- SQLite exact cosine backend.
- Live recall query construction.
- Current-project reranking bonus.
- Global per-project recall file path resolution and rendering under `~/.yaaml/recall/<project-hash>.md`.
- Zero-result recall noop.
- `yaaml recall [--query "..."]`.
- `yaaml path`.

Tests:

- Unit: cosine similarity ranks known vectors correctly.
- Unit: same-project bonus cannot beat a clearly more relevant memory.
- Unit: recall query excludes long tool output.
- Unit: zero-result recall preserves existing file.
- Unit: repeated identical top-N recall avoids file rewrite.
- CLI: manual `yaaml recall --query` writes expected markdown to the resolved global recall path.

### Milestone 7: Daemon and Watchers

Deliver:

- Filesystem watchers for Codex sessions.
- Background workers for live parsing, formulation, embedding, recall.
- Unix socket signal endpoint.
- Graceful shutdown.
- Status worker counts and recent failures.

Tests:

- Integration: appending a Codex JSONL turn creates a `turns` row.
- Integration: completing enough turns queues memory creation.
- Integration: recall file is written after memory exists and new turn completes.
- Unit: SIGTERM persists task state and releases lock.
- Unit: interrupted daemon can be restarted without cursor regression.

### Milestone 8: Init, Skills, Service

Deliver:

- `yaaml init` copies Claude and Codex skills when agent homes exist.
- Optional hook snippets.
- Service install/start/stop/status for macOS LaunchAgent and Linux systemd user service.
- Service install runs idempotent init behavior when skills or snippets are missing.

Tests:

- Unit: skill files are copied, not symlinked.
- Unit: init is idempotent and preserves unrelated skill directories.
- Unit: generated hook snippets include daemon socket path.
- Unit: LaunchAgent plist renders expected binary/config paths.
- Unit: systemd user unit renders expected binary/config paths.
- Unit: service install invokes idempotent init behavior when skills are missing.

### Milestone 9: Consolidation

Deliver:

- Scoped DBSCAN-style clustering.
- Densest-N cluster cap.
- Consolidation LLM call.
- Inactive source memories and lineage.
- Dark-period trigger.

Tests:

- Unit: global and project memories do not cluster together.
- Unit: different project memories do not cluster together.
- Unit: similar same-project vectors cluster.
- Unit: noise points are ignored.
- Unit: clusters sort by size then tightness.
- Unit: densest-N selection picks lowest mean-distance members.
- Integration/mock: consolidation creates new memory and marks sources inactive.

### Milestone 10: Claude Parser

Deliver:

- Claude transcript discovery.
- Main-session JSONL parsing.
- Pattern-based turn completion.
- `subagents/` ignored by default.

Tests:

- Fixture: main Claude session parsed into turn pairs.
- Fixture: assistant text-only message completes a turn.
- Fixture: tool_use/tool_result cycles remain in same turn.
- Unit: `subagents/` files are ignored by default.

### Milestone 11: Recall Evaluation

Deliver:

- `yaaml eval recall`.
- Replay historical turns using only actual memories with `created_at` before replay turn.
- LLM judge score.
- Counterfactual citation score.
- Ranking strategy comparison.

Tests:

- Unit: replay query excludes future transcript content.
- Unit: memories created after replay turn are excluded.
- Mock judge: useful/neutral/distracting scores persist.
- CLI: `yaaml eval recall --limit 1 --json` emits valid result JSON.

## Remaining Open Questions

No known product-level open questions remain. Implementation may surface parser or provider details as work begins.
