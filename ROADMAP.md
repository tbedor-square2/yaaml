# YAAML Roadmap

This roadmap tracks likely next work after the Rust MVP. `REQUIREMENTS.md` remains the product contract; this file is for forward-looking priorities and sequencing.

## Current Baseline

- Rust CLI and daemon.
- SQLite-backed memory, embeddings, tasks, status, recall evals, and backlog progress.
- Codex transcript ingestion from `~/.codex/sessions`.
- Session-aware recall files under `~/.yaaml/recall`.
- Installed Codex and Claude skills for recall and manual remember workflows.
- macOS LaunchAgent service management.
- Manual `yaaml remember` for durable user preferences and problem-solving lessons.

## Near-Term

### Claude Code Support

Add first-class Claude Code transcript ingestion.

- Discover Claude Code JSONL transcripts under `~/.claude/projects/<encoded-path>/`.
- Ignore `subagents/` transcripts by default.
- Parse Claude user, assistant, tool-use, and tool-result events into complete turn pairs.
- Derive project identity from transcript `cwd` metadata.
- Reuse the same memory formulation, recall, and eval pipeline as Codex.
- Add fixtures covering multi-step tool-use loops and text-only final assistant messages.

### Agent-Native Memory Import

Make YAAML recall a superset of agent-native memories.

- Import Codex native memory artifacts from `~/.codex/memories/`.
- Prefer generated Markdown surfaces such as `MEMORY.md`, `memory_summary.md`, `raw_memories.md`, and selected `rollout_summaries/`.
- Treat `~/.codex/memories_1.sqlite` as diagnostic or opportunistic unless its schema proves stable.
- Preserve provenance: originating agent, source file, source line range when available, rollout thread id, and import timestamp.
- Make import idempotent using normalized content/source hashes.
- Classify imported memories as project or global rather than importing everything globally.
- Avoid writing back to Codex native memory files; import into YAAML only.

### Parked Job Recovery

Improve provider failure recovery.

- Add `yaaml jobs list`.
- Add `yaaml jobs retry --parked` for parked jobs after API keys or provider config are fixed.
- Show provider key source and launchd/systemd environment freshness without exposing secrets.
- Consider a service restart helper that reloads login-shell provider env on macOS.

## Recall Quality

### Better Project Bias

Keep same-project recall helpful without making it noisy.

- Continue embedding compact project descriptors into memory text.
- Tune same-project reranking against recall eval results.
- Record when cross-project memories are recalled and whether they helped.

### Recall Eval Iteration

Use evals to guide ranking and memory formation changes.

- Track 1-5 recall scores over time.
- Compare background recall, manual query recall, and imported native-memory recall.
- Surface low-scoring recall patterns in `yaaml eval` output.
- Use subsequent transcript evidence to identify memories that were relevant but not recalled.

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

- Recall skill should prefer daemon-maintained background recall before query refresh.
- Remember skill should prefer `yaaml remember` for YAAML memory storage when it applies.
- Avoid relying on agent-native memory write commands unless the user explicitly asks for native memory.

### Optional Hooks

File watching should remain the primary integration path.

- Optional hooks can improve latency or observability.
- Hooks should not become required for normal transcript ingestion.
- Hook payloads should carry pointers and metadata, not full transcript content.

## Non-Goals For Now

- Bidirectional sync into Codex or Claude native memory stores.
- Project-local YAAML databases.
- External vector databases.
- Per-turn synthetic context injection into agent conversations.
- Storing full raw transcript events in YAAML.
