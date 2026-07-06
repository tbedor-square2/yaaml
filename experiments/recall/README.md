# Recall Experiments

Recall experiments are stored as timestamped directories so strategy results
can be compared over time.

This file is the methodology contract. Every recall/memory experiment should
follow the sampling, significance, and holdout rules below before its result
is treated as a decision input.

## Where Experiment Work Is Tracked

- `../../EXPERIMENTS_LOG.md` — the experiment backlog (Phase 0 validation
  infrastructure plus the technique backlog) and dated decision records.
  New experiment ideas are appended to the backlog with a hypothesis, target
  metric, and measurement mode; completed experiments delete their backlog
  entry and add a decision record. See the "How to use this backlog" rules at
  the top of that file.
- `experiments/recall/<date>-<name>/` — per-run artifacts (`manifest.json`,
  `summary.json`, `details.jsonl`, `REPORT.md`).
- `../../ROADMAP.md` — product direction only; experiment-sized ideas go to
  the backlog, not the roadmap.

## Methodology

### 1. Paired replay design

Compare strategies on the same frozen anchors, never on different anchor sets.
All deltas are paired per-anchor differences against a named baseline. Record
the baseline label and input artifact paths in `manifest.json`.

### 2. Anchor sampling and sample size

Size the replay to the decision, not to the corpus:

- **Screening runs** use a frozen library of ~200 anchors, stratified so at
  least 60 anchors have known oracle labels. Deltas smaller than the paired
  bootstrap confidence interval (see below) are "no detectable effect," not
  wins.
- **Do not reprocess inputs that did not change.** If the strategy only alters
  reranking or selection, replay saved candidates with no provider calls. If
  it changes candidate generation or query construction, rerun retrieval for
  the anchor set but reuse cached query embeddings whenever query text is
  unchanged.
- **Full-corpus replays are exceptional.** Run them only when measuring
  corpus-wide lifecycle effects (consolidation, deactivation, backfill
  density), and say why in the report.
- If a screening delta is promising but its interval includes zero, grow the
  anchor sample (or the known-label density) before rerunning variants —
  more strategy variants on the same underpowered sample is the failure mode
  to avoid.

### 3. Holdout discipline

Keep a holdout anchor set (~100 anchors) that is never used while iterating
on strategies. A strategy ships only after its screening win is confirmed on
the holdout. Refresh both libraries periodically: the memory corpus drifts,
and old oracle rows stop covering newly selected memories (known-score
coverage decays). Record known-score coverage in every report; if coverage
drops below roughly 30% of selected memories, refresh the anchor library
before drawing conclusions.

### 4. Significance

Report a paired bootstrap 95% confidence interval (resample anchors,
≥2000 resamples) for each primary metric delta, not just the point delta.
Decision rules:

- CI excludes zero → real effect; eligible for holdout confirmation.
- CI includes zero → record as "no detectable effect." Do not ship it and do
  not iterate on it as if it were a signal.

Past experiments made calls on ±2-useful deltas over 200 anchors; treat those
historical readouts as directional, not confirmed.

### 5. Retrieval ceiling check

Before any reranking/selection experiment, know the pool ceiling: for anchors
with a known-useful memory, measure how often that memory appears in the
candidate pool at all (pool-recall oracle). No selection policy can exceed
pool recall. If pool recall is the binding constraint, run a candidate
generation experiment instead of another rerank variant.

### 6. Judge validity

All scores come from the LLM judge. Maintain a small hand-labeled calibration
set (~100 recall runs) and report judge agreement when the judge model or
prompt changes. Do not tune ranking against a judge whose agreement is
unknown.

### 7. Abstention accounting

Treat empty recall as abstention, not failure. Track abstention metrics
separately from score metrics, and always report missed-useful empties —
strict strategies can inflate average score by over-abstaining.

## Metrics

Primary metrics for recall experiments:

- `average_known_score`
- `useful_known_selected`
- `low_known_selected`
- `useful_capture_runs`
- `low_selection_runs`
- `average_selected_per_anchor`

Abstention metrics, tracked separately:

- `empty_recall_rate`
- `empty_recall_runs`
- `clean_abstention_runs`
- `missed_useful_empty_runs`
- `missed_useful_empty_rate`

The overall objective is useful recall per context token: prefer policies
that reduce low-scoring injected memories without sharply increasing
missed-useful abstentions.

## Artifact Requirements

Each experiment directory should contain:

- `manifest.json`: run metadata, input paths, baseline label, strategy names,
  anchor library and holdout identifiers, and primary metrics.
- `summary.json`: strategy-level metrics plus deltas and bootstrap confidence
  intervals against the baseline.
- `details.jsonl`: one row per strategy per anchor with selected memory IDs
  and oracle comparison metrics.
- `REPORT.md`: human-readable readout generated from the structured files,
  including known-score coverage and CI-based conclusions.

After every experiment, append a decision-record entry to
`../../EXPERIMENTS_LOG.md`: date, sources, experiment, metrics, lessons,
decision.

## 5x5 Worktree Loop

Use `scripts/recall-5x5-worktree-metrics.py` for the agent-driven 5x5 loop:

1. Generate 5 distinct implementation approaches for improving memory or
   recall metrics.
2. Implement each approach in its own worktree.
3. Run the same recall backtest for baseline plus all candidate worktrees on
   the screening anchor library.
4. Collate comparable metrics, including paired bootstrap CIs, into
   `manifest.json`, `summary.json`, and `REPORT.md`.
5. Append the durable result to `EXPERIMENTS_LOG.md` on the primary branch,
   including links to the report directory and any worktrees/subtrees used.
6. Pick the most promising approach whose CI excludes zero and iterate on
   that approach up to 4 more times. If no approach clears the CI bar, record
   "no detectable effect" and stop instead of iterating on noise.
7. Confirm the final candidate on the holdout anchor set before shipping.

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
the repository eval tooling. Do not optimize average score alone; optimize
useful recall per context cost, with abstention tracked separately.

Procedure:

1. Read the methodology in experiments/recall/README.md and follow its
   sampling, significance, and holdout rules.
2. Inspect current recall metrics with `yaaml stats`, `yaaml eval summary`,
   and recent low-score examples.
3. Propose 5 materially different approaches to improve recall or memory
   quality.
4. Implement each approach in a separate worktree.
5. Run `scripts/recall-5x5-worktree-metrics.py` with baseline plus the 5
   candidate worktrees on the screening anchor library. Replay saved
   candidates when only reranking/selection changed; rerun retrieval only
   when candidate generation or query construction changed.
6. Review the generated `REPORT.md`, including paired bootstrap confidence
   intervals and known-score coverage.
7. Return to the primary branch and update `EXPERIMENTS_LOG.md` with the
   experiment date, candidates, metrics with confidence intervals, links to
   the generated artifacts, links or paths for the relevant
   worktrees/subtrees, lessons learned, and the selected next action.
8. Iterate only on approaches whose primary-metric CI excludes zero. Confirm
   the final candidate on the holdout anchor set before shipping.

Metrics to compare:

1. average_known_score
2. useful_known_selected
3. low_known_selected
4. useful_capture_runs
5. low_selection_runs
6. average_selected_per_anchor
7. empty_recall_rate
8. missed_useful_empty_rate

Treat empty recall as abstention, not failure. Treat deltas whose confidence
interval includes zero as no detectable effect. Commit only the selected
production change unless asked to preserve failed experiments. Always
preserve the experiment readout by committing the generated report artifacts
and the `EXPERIMENTS_LOG.md` entry on the primary branch.
```

## Script Inventory

- `scripts/backtest-recall-strategy.sh`: build a frozen anchor library and
  replay the production recall path over it.
- `scripts/recall-5x5-worktree-metrics.py`: agent-driven 5x5 loop over
  baseline plus candidate worktrees.
- `scripts/recall-5x5-experiment.py`: replay the original five-strategy
  exercise from a `backtest-recall-strategy.sh` output directory.
- `scripts/recall-10x10-experiment.py`: ten strategy variants over ten
  deterministic cohorts, with cohort stability reporting.
- `scripts/recall-health-10x10-experiment.py`: failure-mode-specific recall
  policies using frozen ranking/oracle inputs plus memory health diagnostics.
- `scripts/recall-candidate-5x5-experiment.py`: candidate-family comparison
  (pool sizing, dynamic count, query-signal proxies, hybrid-generation
  proxies, lifecycle suppression, abstention gates). Query-signal and
  hybrid-generation families replay already retrieved candidates; they do not
  measure candidates a fresh retrieval pass would newly find.
- `scripts/recall-techniques-5x5-experiment.py`: broader technique inventory
  across candidate generation, hard gating, reranking, selection budgeting,
  LLM-filter proxies, corpus quality, and eval feedback.
- `scripts/recall-segment-task-fit-5x5-experiment.py`: segment/task-fit
  selection proxies with wrong-context and stale-task low-selection metrics.
  Replays already retrieved candidates only.
- `scripts/recall-cluster-rerank.py`: eval-context cluster rerank backtest.
- `scripts/export-recall-training-data.py`: convert saved backtest artifacts
  into candidate-level JSONL for selection experiments.
- `scripts/recall-feature-model-experiment.py`: compare a small local feature
  model against production and heuristic selectors before considering any
  fine-tuned text model.

## Historical Notes

### 2026-06-26: Codex PreToolUse Hook Removed

The broad Codex PreToolUse hook experiment was removed from default
installation. It produced too much low-value context relative to
explicit/session recall and added friction when installing or updating Codex
hook configuration.

Observed eval shape before removal:

- `tool_pre_use` average score was about 2.40 across evaluated non-empty runs.
- Bash tool recall had many low-scored memories, including repeated build/test
  context that was plausible but often stale or not actionable for the current
  command.
- Empty tool recalls were usually acceptable abstentions, so forcing tool
  recall on every high-level command was not the right optimization target.
- Runtime friction was visible during normal YAAML development: the hook fired
  repeatedly for service/debug commands and surfaced context that was not
  worth the additional interruption.

Decision: keep manual/session recall as the MVP behavior, keep historical
`tool_pre_use` stats readable, and revisit tool-triggered recall only as a
separate experiment with deterministic activation signals and separate
metrics.
