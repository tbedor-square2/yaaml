# YAAML Requirements

YAAML is a local, file-first memory layer for coding agents. It watches native agent transcripts, formulates durable memories in the background, stores them in a user-global local database, and materializes relevant recall as Markdown files that agents can read through installed skills.

## Design Principles

1. **File-first recall**: Recall is written to daemon-owned Markdown files under `~/.yaaml/recall`; agents read those files instead of receiving synthetic injected chat messages.
2. **Async by default**: Transcript ingestion, memory formulation, embedding, consolidation, recall refresh, and evaluation run outside the agent turn path.
3. **Agent-native transcripts remain source of truth**: YAAML stores session, turn, byte-range, and lightweight display metadata, but it does not duplicate full raw transcript event streams.
4. **User-global memory store**: Memories are shared across projects, with ranking that favors relevant project/task context without making project path the only signal.
5. **Measurable recall quality**: Recall behavior is evaluated with explicit quality, abstention, volume, and usefulness metrics.

## Transcript Observation

1. YAAML watches agent transcript roots:
   - Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`
   - Claude Code: `~/.claude/projects/<encoded-path>/<session-id>.jsonl`
2. Codex and Claude Code transcript ingestion are implemented. Codex remains the most exercised integration path; any remaining Claude parity gaps should be tracked as roadmap work.
3. Each session maps to one transcript file. The session record stores agent type, session id, project id, transcript path, start time, and last-seen time.
4. The daemon tracks file cursors by byte offset so restart and backlog ingestion can resume without reprocessing complete files.
5. Stored turn metadata includes session id, turn id or ordinal, byte range, observed timestamp, completion status, optional display text, optional cwd, and optional inferred context metadata.
6. Full transcript content is re-read from the agent transcript file when needed. Tool content is truncated only when constructing model input.

## Memory Creation

1. Memory creation runs after `turns_between_memory` completed turns, and also after a session is idle for `session_idle_memory_seconds`.
2. A formulation job may emit zero, one, or multiple small memories for a turn batch.
3. The memory formulation prompt should prefer concise, durable memories covering:
   - problem-solving insights
   - user redirections or durable preferences
   - reusable workflow constraints
   - project facts that are not obvious from checked-in files
4. The formulation model may refine an existing memory instead of creating a new one by returning `refine_memory_id` or `existing_memory_id`.
5. The memory schema stores: title, body, scope, kind, task keys, activation triggers and anti-triggers, source turn refs, created/updated timestamps, active flag, session id, project id, project descriptor, and lineage refs.
6. Memory kinds are:
   - `preference`
   - `lesson`
   - `workflow`
   - `project_fact`
   - `task_state`
7. Memory scope is `project` by default. The model should use `global` only for durable preferences, agent behavior, or workflow lessons that apply across projects.
8. Project descriptors are compact, human-readable project signals derived from local metadata and model phrasing. Absolute cwd remains structured metadata but should not be the only embedded project signal.
9. Task keys are deterministic-ish labels extracted from memory and query text, such as tool names, PR ids, ticket ids, commands, and other explicit anchors. They are used to boost or suppress task-specific recall.

## Memory Storage and Indexing

1. The canonical database is a local SQLite database at `~/.yaaml/yaaml.db`.
2. The vector index is abstracted behind `VectorIndex`.
3. The current backend is `sqlite-exact`, which stores embeddings locally and performs exact cosine-similarity search.
4. The default embedding provider is OpenAI `text-embedding-3-small`.
5. The default summary and consolidation provider is Anthropic.
6. Inactive memories must not be eligible for recall. Consolidated source memories are marked inactive and linked through lineage refs.

## Consolidation

1. Consolidation runs after `consolidation_dark_period_seconds` without new observed messages.
2. Candidate memories are clustered by embedding distance, scoped by compatible memory scope/project context.
3. Consolidation sends the densest cluster members to an LLM and creates a merged active memory.
4. Original memories are marked inactive. The consolidated memory records lineage refs so the graph can be walked back to source memories and source transcript turns.
5. Consolidation should use the same memory update/refinement semantics as formulation where practical.

## Recall

1. Recall is session-aware.
2. `yaaml recall` chooses the recall file in this order:
   - `CODEX_THREAD_ID`, when present
   - newest known session for the current project
   - project fallback file
3. `yaaml path` resolves the same session/project recall-file path.
4. Background recall is generated from recent completed turns for the session.
5. Manual recall can be run with `yaaml recall --query "<query>"`.
6. Historical replay can be run with `yaaml recall --session <id> --turn <ordinal>` or `--turn-id <id>`.
7. Recall queries are compact synthetic text, not raw JSONL. They include recent user/assistant text, tool names, command names, file paths, short errors, inferred context, and task keys.
8. Recall ranking combines:
   - vector similarity
   - explicit context score
   - bounded same-project bonus
   - task-key bonus
   - activation-trigger and anti-trigger metadata when present
   - global durable-memory bonus
   - penalties for task-state memories without task-key overlap
   - memory-health reranking from prior evals
9. Recall selection filters out inactive memories and can suppress memories recalled very recently.
10. LLM recall filtering is available but disabled by default.
11. Empty recall is a valid abstention. Empty recall should preserve an existing recall file unless a command explicitly refreshes a session file.
12. Bare recall validates that cached recall files still refer to active memories and does not surface inactive or consolidated-away memories.

## Tool-Specific Recall

1. `yaaml init` does not install a Codex PreToolUse hook.
2. Broad tool-triggered recall is deferred because early tool-hook evals showed low relevance, frequent repeated context, and install/update friction.
3. Historical tool recall evals remain segmented by `recall_origin=tool_pre_use` and `tool_name` for analysis.
4. Any future tool-triggered recall should be treated as an experiment with deterministic activation signals, clear abstention behavior, and separate metrics from session recall.

## Skills

1. `yaaml init` installs two skills for Codex and Claude homes when those homes exist:
   - `yaaml`
   - `yaaml-remember`
2. The `yaaml` skill tells the agent to run `yaaml recall` first, then use `yaaml recall --query` only when background recall is missing or stale.
3. The `yaaml-remember` skill stores one concise durable memory with `yaaml remember`.
4. Agents should use the skill or CLI command to resolve recall. They should not assume a project-local `.yaaml/recall.md`.
5. A transcript-reading skill is not part of the current implemented MVP.

## CLI Requirements

The CLI exposes these primary commands:

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
yaaml path         Resolve the current session or project's recall file path
yaaml recall       Print existing recall, or update it from user input
yaaml remember     Store a concise durable memory
```

Commands that report structured state should support `--json`.

## Metrics and Evals

YAAML tracks recall as an evaluated retrieval system, not just as a memory database.

### Eval Scoring

1. Numeric recall evals use a 1-5 scale. Per-memory results are judged with
   the aligned pre-injection instrument (current turn + stored memory +
   rubric), single-sourced in `llm_judge.rs` and shared between the online
   daemon judge and offline eval judging:
   - `5`: directly useful and actionable for the current turn
   - `4`: useful context with minor gaps or extra filtering needed
   - `3`: mixed or marginal; some relevance but not clearly worth recall
   - `2`: weak, stale, or mostly irrelevant
   - `1`: distracting, wrong-context, or actively harmful
2. `insufficient_context` means no later completed turns were available, so usefulness could not be scored.
3. Empty recall runs are scored as abstentions:
   - `clean_abstention`: recall returned nothing and no useful memory appears to have been missed
   - `missed_useful_abstention`: recall returned nothing but a useful memory likely existed
4. Numeric quality metrics exclude abstentions and insufficient-context results. Abstention is tracked separately.
5. Eval runs store session id, turn ordinal, recall origin, tool name when applicable, selected memory ids, and rationale.

### Stats Command

`yaaml stats` reports:

1. **Recall rate**
   - eligible completed turns
   - recall runs per eligible turn
   - non-empty recall runs per eligible turn
   - non-empty recall runs per recall run
2. **Recall volume**
   - average memories per non-empty run
   - p50/p90 memories per non-empty run
   - average and p50/p90 recall characters
   - memory-count buckets: `0`, `1-2`, `3-5`, `>5`
3. **Useful recall**
   - evaluated recall runs
   - runs with at least one score `4` or `5`
   - useful run rate per eligible turn
   - useful run rate per evaluated recall
   - per-memory good and low-score rates
4. **Abstention**
   - empty recall runs
   - evaluated empty recall runs
   - clean abstention count/rate
   - missed-useful abstention count/rate
   - unjudged empty recall runs
5. **LLM filter telemetry**
   - runs with filter telemetry
   - attempted/applied/error counts
   - average candidate count
   - average dropped memories when applied
6. **Segments**
   - by recall origin, such as `session_background`, `manual_query`, and `tool_pre_use`
   - by tool name for tool-pre-use recall

The main quality tradeoff metrics are:

1. `useful_run_rate_per_eligible_turn`
2. `average_recall_chars_per_non_empty_run`
3. `missed_useful_abstention_rate_per_evaluated_empty_recall`
4. `low_memory_rate`

## Configuration Defaults

| Key | Default | Description |
|-----|---------|-------------|
| `turns_between_memory` | `10` | Completed turns before automatic memory formulation |
| `session_idle_memory_seconds` | `600` | Idle seconds before below-threshold sessions are flushed |
| `consolidation_dark_period_seconds` | `300` | Inactivity seconds before consolidation runs |
| `recall_result_limit` | `2` | Max memories returned per recall query |
| `recall_candidate_pool` | `16` | Vector candidates fetched before final selection |
| `recall_live_turn_window` | `3` | Completed turns included in live recall query construction |
| `recall_query_max_chars` | `12000` | Max characters in synthesized recall query text |
| `recall_similarity_threshold` | `0.3` | Cosine similarity cutoff for vector search |
| `recall_project_tiebreaker` | `true` | Enables same-project reranking bonus |
| `recall_project_score_bonus` | `0.05` | Maximum additive same-project bonus |
| `recall_llm_filter_enabled` | `false` | Enables LLM filtering after deterministic recall |
| `recall_llm_filter_candidate_limit` | `16` | Max candidates sent to the LLM filter |
| `recall_llm_filter_prompt_max_chars` | `12000` | Max prompt chars for LLM recall filter |
| `recall_memory_cooldown_seconds` | `1200` | Suppress memories recalled very recently |
| `recall_dir` | `~/.yaaml/recall` | Directory containing recall files |
| `db_path` | `~/.yaaml/yaaml.db` | SQLite database path |
| `vector_index_backend` | `sqlite-exact` | Local vector backend |
| `vector_index_path` | `~/.yaaml/vector-index` | Vector index persistence path |
| `embedding_provider` | `openai` | Embedding provider |
| `embedding_model` | `text-embedding-3-small` | Embedding model |
| `embedding_api_key_env` | `OPENAI_API_KEY` | Embedding API key environment variable |
| `summary_provider` | `anthropic` | Memory formulation provider |
| `summary_model` | `claude-haiku-4-5-20251001` | Memory formulation model |
| `summary_api_key_env` | `ANTHROPIC_API_KEY` | Summary API key environment variable |
| `consolidation_provider` | `anthropic` | Consolidation provider |
| `consolidation_model` | `claude-haiku-4-5-20251001` | Consolidation model |
| `consolidation_api_key_env` | `ANTHROPIC_API_KEY` | Consolidation API key environment variable |
| `memory_cluster_distance_threshold` | `0.21125` | Consolidation cosine-distance threshold |
| `memory_cluster_min_size` | `3` | Minimum cluster size |
| `memory_cluster_max_size` | `5` | Maximum dense memories sent to consolidation |
| `max_memory_length` | `12000` | Max characters per memory body |
| `max_formulation_tokens` | `32000` | Token cap for memory formulation input |
| `tool_call_truncation_chars` | `500` | Max chars per tool call in formulation input |
| `recall_classifier_enabled` | `true` | Enables recall query gating |
| `backlog_max_concurrent_remote_jobs` | `1` | Max concurrent backlog remote jobs |
| `backlog_newest_first` | `true` | Process historical transcript backlog newest-first |
| `backlog_formulation_turn_window` | `10` | Completed turns batched into backlog formulation |
| `eval_judge_provider` | `anthropic` | Recall eval judge provider |
| `eval_judge_model` | `claude-haiku-4-5-20251001` | Recall eval judge model |
| `eval_judge_api_key_env` | `ANTHROPIC_API_KEY` | Eval judge API key environment variable |

## Non-Functional Requirements

1. Agent hot-path latency must remain effectively unchanged.
2. Read commands should tolerate daemon writes and transient SQLite locking.
3. Memory data, recall files, task state, and transcript references are local by default.
4. Text sent for embedding, formulation, consolidation, filtering, or judging may go to configured remote providers.
5. Service logs are written under `~/.yaaml/`.

## Deferred Work

1. Remaining Claude Code parity gaps beyond transcript ingestion.
2. Agent-native memory ingestion so YAAML recall can become a superset of Codex/Claude native memories.
3. Dynamic conversation segments that are decoupled from session id and cwd.
4. Tool-triggered recall experiments using deterministic activation signals rather than a default broad Codex PreToolUse hook.
5. Web or Obsidian UI.
