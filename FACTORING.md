# Factoring & Test Improvements

Tracks planned code reorganization, test additions, and coverage tooling. Items are roughly priority-ordered within each section.

---

## Code Factoring

### 1. Split `crates/yaaml/src/daemon.rs` (1,763 lines) into submodules

Current file mixes transcript ingestion, task dispatch, recall writing, eval orchestration, and error recovery. Target layout:

```
crates/yaaml/src/daemon/
  mod.rs          # DaemonState, main run loop, signal socket
  ingestion.rs    # process_*_changes, process_*_backlog, ingest_*_file
  tasks.rs        # queue_memory_formulation_if_due, queue_consolidation_if_due, run_queued_tasks
  recall.rs       # refresh_recall_with_embedding, query construction
  eval.rs         # eval task dispatch and scoring
  recovery.rs     # recover_running_tasks, dedupe_active_memories
```

### 2. Split `crates/yaaml/src/main.rs` (2,116 lines) into command modules

Keep `main.rs` for argument parsing and dispatch only. Move implementations to:

```
crates/yaaml/src/commands/
  mod.rs
  eval.rs       # eval recall, eval list, eval show, eval summary
  recall.rs     # manual recall query
  remember.rs   # manual memory creation
  status.rs     # status display and formatting
  init.rs       # project initialization
```

### 3. Extract submodules from `crates/yaaml-core/src/context.rs` (519 lines)

```
crates/yaaml-core/src/context/
  mod.rs        # public API, ContextResult, merge logic
  inference.rs  # infer_context_from_path, infer_context_from_text, infer_context_from_memories
  scoring.rs    # context_score, high_signal_tag, overlap detection
  tags.rs       # KNOWN_PHRASES and tag normalization (move to config-driven eventually)
```

Implemented the mechanical module split while preserving the public `yaaml_core::context`
API. Follow-up work remains for making the phrase/tag lists configuration-driven.

### 4. Split `crates/yaaml-store/src/database.rs` (1,872 lines) by record type

```
crates/yaaml-store/src/database/
  mod.rs          # Database struct, connection management
  sessions.rs     # SessionRecord CRUD
  turns.rs        # TurnRecord CRUD, cursor tracking
  memories.rs     # MemoryRecord CRUD, deduplication queries
  embeddings.rs   # EmbeddingRecord CRUD, f32 serialization
  tasks.rs        # TaskRecord CRUD, state machine transitions
  evals.rs        # EvalRecord CRUD, score aggregation
```

### 5. Strongly-type task payloads

Replace `payload_json: String` with a typed enum deserialized at dispatch:

```rust
enum TaskPayload {
    MemoryFormulation { session_id: String, turn_range: (i64, i64) },
    Consolidation { scope: MemoryScope, project_id: Option<String> },
    Embedding { memory_id: i64 },
    Recall { project_id: String, query_text: String },
    Eval { session_id: String, turn_ordinal: i64 },
}
```

### 6. Store `display_text` in the database at ingestion

`turn_hydration.rs` currently re-reads original JSONL files on every turn query. If the transcript is deleted or moved, `display_text` becomes unavailable. Store it in the `turns` table at ingestion time instead. Modest storage cost (~10-20% larger DB); eliminates silent data loss.

### 7. Parameterize Square-internal project names in `context.rs`

`KNOWN_PHRASES` and high-signal tag lists contain hardcoded Square-internal project names (`elroy`, `aida-docs`, `sss`, `dumbo`). Move these to optional user configuration or remove them from open-source builds.

### 8. Add `yaaml config --effective`

Expose the merged user-level + project-level configuration so users can debug config drift without reading TOML files directly.

---

## Test Coverage Tooling

### Strict code quality gate

Implemented:

- Added `cargo strict` alias for `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- Added `scripts/check-quality.sh` to run formatting, strict clippy, the full workspace test suite, and coverage.
- Added GitHub Actions workflow enforcing fmt, clippy-as-errors, tests, and the 70% coverage floor.

Current assessment: the repo now has a strict local and CI quality gate. Remaining quality work is structural, not gate-related: the largest files are still `daemon.rs`, `main.rs`, and `database.rs`.

The local entrypoint is `just quality`, which delegates to `scripts/check-quality.sh`.

### Add `cargo-tarpaulin` and enforce a minimum coverage threshold

Install tarpaulin and add a CI step:

```toml
# .cargo/config.toml or Makefile
[alias]
coverage = "tarpaulin --workspace --timeout 120 --out Html --output-dir target/coverage"
```

CI check (fail below threshold):

```bash
cargo tarpaulin --workspace --timeout 120 --fail-under 70
```

Start with a 70% line-coverage floor and raise it as gaps are closed. Track per-crate coverage to prevent individual crates from regressing.

Implemented:

- Added `.cargo/config.toml` with a `cargo coverage` alias for tarpaulin.
- Added `scripts/check-coverage.sh`, which enforces `YAAML_COVERAGE_MIN` (default 70). It prefers tarpaulin when installed and falls back to `cargo llvm-cov` on macOS/Homebrew Rust.
- Current coverage check passes at 88.06% total line coverage.

---

## Test Additions

### yaaml-transcript: standalone unit tests (highest priority gap)

`claude.rs` and `codex.rs` are only exercised through `daemon.rs` integration tests. Add `crates/yaaml-transcript/tests/` with:

- **`claude_parsing.rs`**: Unit tests for Claude Code JSONL parsing
  - Basic user → assistant turn pair
  - Multi-step tool-use loop followed by text-only assistant close
  - Subagent transcript detection and skip
  - Partial write mid-turn (cursor does not advance past incomplete turn)
  - Malformed JSON line in middle of valid stream
  - Assistant message with no content blocks
  - Tool result with `is_error: true`

- **`codex_parsing.rs`**: Unit tests for Codex JSONL parsing
  - Single TurnComplete event
  - TurnAborted event (turn should be discarded or marked)
  - Cursor resume from mid-file offset
  - SessionMeta event before first turn
  - Unknown event types ignored gracefully

- **Fuzz targets** (via `cargo-fuzz`): arbitrary UTF-8 input to both parsers; must not panic

Implemented standalone parser tests for the listed Claude and Codex JSONL cases. Fuzz targets remain open.

### yaaml-store: database migration and edge case tests

- **`migrations.rs`**: Verify schema v1 creates all expected tables; ensure no-op on repeat apply
- **`concurrency.rs`**: Two connections reading/writing simultaneously; verify no data corruption
- **`task_state_machine.rs`**: Full lifecycle (pending → running → complete/failed → requeued); verify invalid transitions are rejected

Implemented migration, concurrency, and supported task lifecycle coverage. Strict invalid-transition rejection remains open because the current store API does not enforce transition guards.

### yaaml-llm: provider error path tests

- Connection timeout returns retryable error
- HTTP 429 returns retryable error with `Retry-After` header parsed
- HTTP 401 returns non-retryable auth error
- Partial/truncated JSON response body is handled gracefully
- Verify request bodies contain expected fields (model, messages, etc.) — current mock servers only check that a request arrived

Implemented retryable transport error coverage, HTTP 429/401 classification, truncated response parse errors, missing text content handling, and request body/header assertions. `Retry-After` parsing remains open because provider errors currently do not model response headers.

### Daemon: error path and edge case tests

- Missing transcript file at path stored in cursor record (session file deleted between runs)
- Corrupt JSONL in the middle of a real turn (truncated mid-object)
- Filesystem permission error on recall file write
- Task dispatch when embedding server is unreachable (parked, not crashed)
- Backlog processing respects newest-first ordering

Implemented deleted cursored transcript handling and recall embedding-provider parking coverage. Corrupt JSONL and newest-first ordering have existing parser/discovery coverage; filesystem permission errors remain open.

### Consolidation: cluster edge cases

- Single-memory "cluster" (no merge needed, memory passes through unchanged)
- Two identical memories (lexical Jaccard ≥ threshold → merged)
- Empty embedding vector (should not panic or produce NaN similarity)
- Cluster exceeds densest-member size threshold (verify truncation)
- Cross-scope memories (global + per-project) must not be merged

Implemented coverage for all listed consolidation edge cases.

### CLI commands: integration tests for uncovered commands

- `yaaml init` — verify recall file skeleton is written
- `yaaml status` — verify JSON and human-readable output match DB state
- `yaaml remember` — verify a manual memory is stored and retrievable
- `yaaml service install` / `service uninstall` — verify plist/systemd unit file is written and removed

Implemented command-level tests for `init`, human and JSON `status`, `remember`, and `service install` / `service uninstall`.

### Existing test quality fixes

- Replace hardcoded timestamps (e.g., `2026-06-08`) in test fixtures with relative durations or `OffsetDateTime::now_utc()` minus a delta
- Upgrade fake TCP servers in `daemon.rs` to validate request body contents, not just that a request arrived
- Make magic turn counts (10, 25) named constants with explanatory doc comments

---

## Summary Checklist

- [ ] Split `daemon.rs` into submodules
- [ ] Split `main.rs` into command modules
- [x] Split `context.rs` into submodules
- [ ] Split `database.rs` by record type
- [ ] Strongly-type task payloads
- [ ] Store `display_text` in DB at ingestion
- [ ] Parameterize Square-internal project names
- [ ] Add `yaaml config --effective`
- [x] Add strict lint / code quality gate
- [x] Add coverage checker with ≥70% coverage floor
- [x] Add `yaaml-transcript` standalone unit tests
- [ ] Add fuzz targets for JSONL parsers
- [x] Add database migration and state machine tests
- [x] Add LLM provider error path tests
- [x] Add daemon error path tests
- [x] Add consolidation edge case tests
- [x] Add CLI integration tests for `init`, `status`, `remember`, `service`
- [ ] Fix test fixture brittleness (timestamps, magic numbers, mock servers)
