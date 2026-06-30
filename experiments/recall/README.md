# Recall Experiments

Recall experiments are stored as timestamped directories so strategy results can
be compared over time.

Cross-experiment lessons and decisions are summarized in
`../../EXPERIMENTS_LOG.md`.

## 2026-06-26: Codex PreToolUse Hook Removed

The broad Codex PreToolUse hook experiment was removed from default installation.
It produced too much low-value context relative to explicit/session recall and
added friction when installing or updating Codex hook configuration.

Observed eval shape before removal:

- `tool_pre_use` average score was about 2.40 across evaluated non-empty runs.
- Bash tool recall had many low-scored memories, including repeated build/test
  context that was plausible but often stale or not actionable for the current
  command.
- Empty tool recalls were usually acceptable abstentions, so forcing tool recall
  on every high-level command was not the right optimization target.
- Runtime friction was visible during normal YAAML development: the hook fired
  repeatedly for service/debug commands and surfaced context that was not worth
  the additional interruption.

Decision: keep manual/session recall as the MVP behavior, keep historical
`tool_pre_use` stats readable, and revisit tool-triggered recall only as a
separate experiment with deterministic activation signals and separate metrics.

Each experiment directory should contain:

- `manifest.json`: run metadata, input paths, strategy names, and primary metrics.
- `summary.json`: strategy-level metrics plus deltas against the baseline.
- `details.jsonl`: one row per strategy per anchor with selected memory IDs and
  oracle comparison metrics.
- `REPORT.md`: human-readable readout generated from the structured files.

Primary metrics for recall experiments:

- `average_known_score`
- `useful_known_selected`
- `low_known_selected`
- `useful_capture_runs`

Abstention metrics are tracked separately from score metrics:

- `empty_recall_rate`
- `empty_recall_runs`
- `clean_abstention_runs`
- `missed_useful_empty_runs`
- `missed_useful_empty_rate`

## 5x5 Worktree Loop

Use `scripts/recall-5x5-worktree-metrics.py` for the agent-driven 5x5 loop:

1. Generate 5 distinct implementation approaches for improving memory or recall
   metrics.
2. Implement each approach in its own worktree.
3. Run the same recall backtest for baseline plus all candidate worktrees.
4. Collate comparable metrics into `manifest.json`, `summary.json`, and
   `REPORT.md`.
5. Append the durable result to `EXPERIMENTS_LOG.md` on the primary branch,
   including links to the report directory and any worktrees/subtrees used.
6. Pick the most promising approach, then iterate on that approach 4 more times.

Command shape:

```bash
python3 scripts/recall-5x5-worktree-metrics.py \
  --baseline baseline=/Users/tbedor/Development/yaaml \
  --candidate approach-a=/path/to/worktree-a \
  --candidate approach-b=/path/to/worktree-b \
  --candidate approach-c=/path/to/worktree-c \
  --candidate approach-d=/path/to/worktree-d \
  --candidate approach-e=/path/to/worktree-e \
  --out-dir experiments/recall/$(date +%F)-5x5-round-1
```

Shareable prompt:

```text
Run a YAAML 5x5 recall-improvement experiment.

Goal:
Improve YAAML memory/recall metrics using experimental worktrees, measured by
the repository eval tooling. Do not optimize average score alone; optimize useful
recall per context cost, with abstention tracked separately.

Procedure:

1. Inspect current recall metrics with `yaaml stats`, `yaaml eval summary`, and
   recent low-score examples.
2. Propose 5 materially different approaches to improve recall or memory
   quality.
3. Implement each approach in a separate worktree.
4. Run `scripts/recall-5x5-worktree-metrics.py` with baseline plus the 5
   candidate worktrees.
5. Review the generated `REPORT.md`.
6. Return to the primary branch and update `EXPERIMENTS_LOG.md` with the
   experiment date, candidates, metrics, links to the generated artifacts, links
   or paths for the relevant worktrees/subtrees, lessons learned, and the
   selected next action.
7. Choose the most promising approach and iterate on that approach 4 more times.

Metrics to compare:

1. average_known_score
2. useful_known_selected
3. low_known_selected
4. useful_capture_runs
5. low_selection_runs
6. average_selected_per_anchor
7. empty_recall_rate
8. missed_useful_empty_rate

Treat empty recall as abstention, not failure. Commit only the selected
production change unless asked to preserve failed experiments. Always preserve
the experiment readout by committing the generated report artifacts and the
`EXPERIMENTS_LOG.md` entry on the primary branch.
```

Use `scripts/recall-5x5-experiment.py` to replay the original five-strategy
exercise from a `scripts/backtest-recall-strategy.sh` output directory.

Use `scripts/recall-10x10-experiment.py` for the expanded exercise. It compares
ten strategy variants over ten deterministic cohorts, then reports aggregate
results plus cohort stability.

Use `scripts/recall-health-10x10-experiment.py` for the health-aware exercise.
It compares failure-mode-specific recall policies using the same frozen ranking
and oracle inputs plus memory health diagnostics from the local YAAML database.

Use `scripts/recall-candidate-5x5-experiment.py` to compare recall-improvement
candidate families. It runs five strategies across five cohorts for candidate
pool sizing, dynamic recall count, query-signal proxies, hybrid-generation
proxies, memory lifecycle suppression, and abstention gates. Query-signal and
hybrid-generation families replay over already retrieved candidates; they do
not measure candidates that a fresh query embedding or lexical retrieval pass
would newly retrieve.

Use `scripts/recall-techniques-5x5-experiment.py` for a broader technique
inventory. It runs five strategies across five cohorts for each recall
technique category: candidate generation, hard gating, reranking, selection
budgeting, LLM-filter proxies, memory corpus quality, and eval feedback. The
report is intended to compare useful recall against context volume, not just
average recall score.

Use `scripts/recall-segment-task-fit-5x5-experiment.py` for segment/task-fit
selection experiments. It compares context/task/provenance proxies over saved
recall candidates and adds wrong-context plus stale-task low-selection metrics.
It does not test segment-aware candidate generation because it replays already
retrieved candidates.

Use `scripts/export-recall-training-data.py` to convert saved recall backtest
artifacts into candidate-level JSONL for selection experiments. Use
`scripts/recall-feature-model-experiment.py` to compare a small local feature
model against production and simple heuristic selectors before considering any
fine-tuned text model.
