# YAAML Roadmap

This roadmap tracks likely next work after the Rust MVP. `REQUIREMENTS.md` remains the product contract; this file is for forward-looking priorities and sequencing.

Scope note: this file holds product direction. Anything experiment-sized — a change with a hypothesis and a recall/memory metric it should move — belongs in the experiment backlog in `EXPERIMENTS_LOG.md`, run under the methodology in `experiments/recall/README.md`. The "Recall Quality" and "Memory Quality" sections below describe direction; the backlog is the authoritative list of what to run next.

## Current Baseline

- Rust CLI and daemon.
- SQLite-backed memory, embeddings, tasks, status, recall evals, and backlog progress.
- Codex transcript ingestion from `~/.codex/sessions`.
- Claude Code transcript ingestion from `~/.claude/projects/<encoded-path>/`, ignoring `subagents/` transcripts by default and parsing tool-use loops into completed turns.
- Session-aware cached results for explicit recall queries under `~/.yaaml/recall`.
- Installed Codex and Claude skills for on-demand recall and manual remember workflows.
- macOS LaunchAgent service management.
- Manual `yaaml remember` for durable user preferences and problem-solving lessons.
- Task queue inspection, retry, and clearing through `yaaml tasks`.
- Automatic background recall has been removed; transcript ingestion and memory formation continue without generating recall after completed turns.
- Formation-time activation-condition metadata remains available to explicit recall ranking.

## Near-Term

### Agent-Native Memory Import

Make YAAML recall a superset of agent-native memories.

- Import Codex native memory artifacts from `~/.codex/memories/`.
- Prefer generated Markdown surfaces such as `MEMORY.md`, `memory_summary.md`, `raw_memories.md`, and selected `rollout_summaries/`.
- Treat `~/.codex/memories_1.sqlite` as diagnostic or opportunistic unless its schema proves stable.
- Preserve provenance: originating agent, source file, source line range when available, rollout thread id, and import timestamp.
- Make import idempotent using normalized content/source hashes.
- Classify imported memories as project or global rather than importing everything globally.
- Avoid writing back to Codex native memory files; import into YAAML only.

### Provider Recovery Polish

Improve provider failure recovery beyond the existing `yaaml tasks` commands.

- Show provider key source and launchd/systemd environment freshness without exposing secrets.
- Consider a service restart helper that reloads login-shell provider env on macOS.

## Recall Quality

### Recall Value Per Context Token

Improve explicit recall by reducing low-value context before increasing recall volume.

- Track useful recall separately from context cost: useful selected memories, low-scoring selected memories, empty recall rate, missed-useful empties, and recall character/token volume.
- Prefer policies that reduce low-scoring injected memories without sharply increasing missed-useful abstentions.
- Treat empty recall as acceptable when the available memory set has no useful match.
- Use recent evals and replay experiments to compare narrowing strategies before changing runtime recall behavior.

### Better Project Bias

Keep same-project recall helpful without making it noisy.

- Continue embedding compact project descriptors into memory text.
- Tune same-project reranking against recall eval results.
- Record when cross-project memories are recalled and whether they helped.

### Recall Eval Iteration

Use evals to guide ranking and memory formation changes.

- Track 1-5 recall scores over time.
- Compare explicit query recall and imported native-memory recall; retain historical background-recall metrics only for analysis.
- Surface low-scoring recall patterns in `yaaml eval` output.
- Use subsequent transcript evidence to identify memories that were relevant but not recalled.
- Prefer feature-level recall experiments before fine-tuning a tiny text model; only revisit fine-tuning after feature models plateau on denser per-candidate labels and error analysis shows text-level judgment is the missing signal.

### Future Direction: Recall Cooldowns

Avoid repeatedly surfacing the same memory across explicit queries while it is likely already in the agent context.

- Consider a session-level per-memory cooldown before recall injection.
- Backtest cooldown windows against useful captures, low-scoring selections, missed-useful abstentions, and recall volume.
- Keep cooldowns context-budget oriented: the goal is not fewer recalls for its own sake, but fewer repeated memories that add little incremental value.
- Revisit command-chain awareness for adjacent verification steps where the same memory may remain useful across repeated commands.

### Future Direction: Activation Metadata Signals

Use deterministic tool and command signals when they can narrow recall without adding another lossy classifier.

- Evaluate formation-time activation triggers and anti-triggers only in explicit recall.
- Prefer signals already present in explicit queries before adding new runtime hook surfaces.
- Keep activation metadata evidence-driven; it must not reintroduce automatic recall.
- Avoid broad semantic labels such as `situation:pr-comment` until evals show they improve useful recall per context token.
- Do not reintroduce broad pre-tool hooks without a separate experiment, deterministic activation rules, and metrics that segment hook recall from ordinary session recall.

## Memory Quality

### Manual Remember Workflow

Keep manual memories focused and durable.

- Prefer memories for user preferences, repeated corrections, and problem-solving lessons where an initial approach failed.
- Avoid storing transient task status, secrets, large transcript excerpts, or facts obvious from checked-in code.
- Consider adding dry-run validation for `yaaml remember` to warn about overly broad or transient memories.

### Consolidation

Continue tightening memory consolidation.

- Use clustering only within compatible scopes: global with global, project with same project.
- Preserve source lineage and inactive source memories.
- Evaluate consolidated memories against recall usefulness before making consolidation more aggressive.

## Agent Integration

### Skill Wording

Keep skill instructions explicit about YAAML surfaces.

- Recall skill should run a focused explicit query only when prior context is materially relevant.
- Remember skill should prefer `yaaml remember` for YAAML memory storage when it applies.
- Avoid relying on agent-native memory write commands unless the user explicitly asks for native memory.

### Optional Hooks

File watching should remain the primary integration path.

- The broad Codex PreToolUse hook experiment was removed because it fired too often and added low-value context.
- Future hooks, if any, should be narrow observability hooks with deterministic activation rules, not general-purpose context injection.
- Hooks should not become required for normal transcript ingestion or recall.

## Non-Goals For Now

- Bidirectional sync into Codex or Claude native memory stores.
- Project-local YAAML databases.
- External vector databases.
- Per-turn synthetic context injection into agent conversations.
- Storing full raw transcript events in YAAML.
