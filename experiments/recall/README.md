# Recall Experiments

Recall experiments are stored as timestamped directories so strategy results
can be compared over time.

This file is the methodology contract. Every recall/memory experiment should
follow the sampling, significance, and holdout rules below before its result
is treated as a decision input.

## Scope and Flexibility

The contract governs *decision inputs*, not exploration. Rules of thumb:

- **Exploration is cheap and unrestricted.** Prototyping a strategy, eyeballing
  a handful of anchors, mining transcripts, or running a quick diagnostic
  needs no anchor library, CI, or holdout. The rules bind at the moment a
  result is used to ship, revert, or kill an idea.
- **The 5x5 worktree loop is one harness, not the only one.** Single-candidate
  experiments, forward memory-write experiments, and novel harness shapes are
  all valid. For *comparative* experiments (anything claiming one policy beats
  another) the invariants are paired comparison against a named baseline,
  CI-labeled deltas, abstention tracked separately, and a decision record.
  *Diagnostics* — experiments that answer a question rather than compare
  policies, like formation-miss mining or the pool-recall oracle — need only
  a stated question, the raw cases behind the answer, a report, and a
  decision record; paired deltas and CIs apply only if they make comparative
  claims.
- **The primary metrics are a floor, not a ceiling.** Experiments may add
  metrics (context-token cost, latency, wrong-context lows, anything the
  hypothesis needs). Changing the primary set itself is a methodology change
  and gets its own decision record.
- **Metrics are not the only ship rationale.** A change may ship despite
  neutral metrics for product-safety, privacy, or maintainability reasons
  (precedent: removing hardcoded topic classifiers shipped with worse
  abstention). The decision record must then say explicitly that the metric
  case was neutral and name the non-metric rationale — what is not allowed is
  presenting a neutral result as a metric win.

## Where Experiment Work Is Tracked

- `../../EXPERIMENTS_LOG.md` — the experiment backlog (Phase 0 validation
  infrastructure plus the technique backlog) and dated decision records.
  New experiment ideas are appended to the backlog with a hypothesis, target
  metric, and measurement mode; addressed items follow that file's "When an
  item is addressed" policy (completed Phase 0 prerequisites stay listed with
  a DONE date; completed experiments are replaced by decision records).
- `experiments/recall/<date>-<name>/` — per-run artifacts. The artifact shape
  depends on the experiment type (replay, forward, or diagnostic); see
  "Artifact Requirements" below.
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

**Tooling status**: the shared implementation lives in
`scripts/recall_experiment_stats.py` (paired bootstrap over anchors keyed by
`run_id`, deterministic seed, verdict classification).
`recall-5x5-worktree-metrics.py` emits CI fields and report verdicts; the
older standalone `recall-*-experiment.py` replay scripts still emit point
deltas only and must import the shared helper before their results are used
for decisions. `summary.json` must include per-strategy CI fields shaped
like:

```json
"delta_ci_95": {
  "useful_capture_runs": [-3, 5],
  "low_known_selected": [-12, -2],
  "average_known_score": [-0.04, 0.18]
}
```

and `REPORT.md` must label each primary-metric delta as `confirmed`
(CI excludes zero), `no detectable effect` (CI includes zero), or
`needs larger sample` (CI includes zero but is wide enough that a real effect
of decision-relevant size cannot be ruled out). The per-metric
decision-relevant effect sizes live in `DECISION_RELEVANT_EFFECT` in
`scripts/recall_experiment_stats.py`; they are judgment calls, and revising
them is allowed with a decision record explaining the change.

### 5. Retrieval ceiling check

Before any reranking/selection experiment, know the pool ceiling: for anchors
with a known-useful memory, measure how often that memory appears in the
candidate pool at all (pool-recall oracle). No selection policy can exceed
pool recall. If pool recall is the binding constraint, run a candidate
generation experiment instead of another rerank variant.

### 6. Judge validity

All scores come from the LLM judge. Maintain a small adjudicated calibration
set (~100 recall candidates) and report agreement when the judge model or
prompt changes. The default adjudicator is an independent LLM pass that sees
only the query, memory, and rubric — not the production judge score or
rationale. This validates judge-vs-adjudicator agreement for tuning
discipline, not human ground truth. Do not tune ranking against a judge whose
agreement is unknown.

### 7. Abstention accounting

Treat empty recall as abstention, not failure. Track abstention metrics
separately from score metrics, and always report missed-useful empties —
strict strategies can inflate average score by over-abstaining.

## Phase 0 Runbook: Anchor Libraries

Frozen anchor libraries are the substrate for every backtest. Canonical
layout:

- `experiments/recall/anchor-libraries/<YYYY-MM>-screening.tsv` (~200 anchors)
- `experiments/recall/anchor-libraries/<YYYY-MM>-holdout.tsv` (~100 anchors)
- `experiments/recall/anchor-libraries/<YYYY-MM>-manifest.json` (generation
  date, database snapshot info, selection query parameters, known-score
  coverage for each set)

Anchor TSV schema (tab-separated, no header — the format
`scripts/backtest-recall-strategy.sh` consumes):

```text
run_id <TAB> session_id <TAB> turn_ordinal <TAB> case_label
```

Build procedure:

```bash
# 1. Generate a fresh anchor pool from the eval library (newest-first,
#    leakage-filtered). This runs the production replay once and writes
#    anchors.tsv plus oracle/recall artifacts.
BACKTEST_ANCHOR_SOURCE=eval-library \
BACKTEST_ANCHOR_LIMIT=300 \
BACKTEST_OUT_DIR=target/anchor-refresh-$(date +%Y-%m) \
scripts/backtest-recall-strategy.sh . anchor-refresh

# 2. Split the pool into screening and holdout: deterministic shuffle,
#    stratified so each set keeps a proportional share of anchors with
#    known oracle labels, zero overlap, coverage reported in the manifest.
python3 scripts/build-anchor-library.py \
  --input-dir target/anchor-refresh-$(date +%Y-%m) \
  --label $(date +%Y-%m) \
  --screening 200 --holdout 100

# 3. Commit both TSVs and the manifest under
#    experiments/recall/anchor-libraries/.
```

The builder refuses to overwrite existing library files and warns when a
set's known-score coverage falls below the 30% done-criteria threshold.

All subsequent screening runs must pass the frozen file explicitly:

```bash
BACKTEST_ANCHORS_FILE=experiments/recall/anchor-libraries/<YYYY-MM>-screening.tsv \
BACKTEST_OUT_DIR=target/recall-backtests/<experiment-label> \
scripts/backtest-recall-strategy.sh . <experiment-label>
```

Never regenerate anchors dynamically inside an experiment (that silently
changes the sample). The holdout file is only ever passed for a final
ship/no-ship confirmation.

Done criteria for a refresh: screening and holdout TSVs plus manifest
committed; known-score coverage reported for both sets and ≥30% of anchors
carrying oracle labels; zero anchor overlap between the sets.

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

### Memory-Write Experiment Metrics

The metrics above are recall-selection metrics; they replay retrieval over a
fixed corpus. Experiments that change what gets *written* — formulation
prompts, activation conditions, consolidation policy, rewrite-vs-suppression,
formation-miss mining — need a different contract, because saved-candidate
replay cannot measure them. For memory-write experiments report:

- **Source faithfulness**: does the memory accurately reflect its source
  turns (judged against the transcript, not the query)?
- **Durability classification**: was transient task state written as durable
  (or vice versa)? Kind and scope classification accuracy against a labeled
  sample.
- **Specificity/actionability**: judged 1–5 on whether the memory is concrete
  enough to act on, using the same judge-calibration discipline as recall
  scores.
- **False-positive creation rate**: memories written that no later turn ever
  makes useful.
- **Missed-creation rate**: repeated corrections/mistakes in later transcripts
  with no corresponding memory (the formation-miss denominator).
- **Downstream recall usefulness**: after enough forward exposure, the
  standard recall metrics segmented by memory cohort (written under the new
  policy vs. old).
- **Corpus lifecycle impact**: memory count, consolidation rate, supersession
  rate, and dedup rate — a write policy that doubles corpus size changes
  retrieval behavior even if per-memory quality is flat.

Memory-write experiments are forward experiments by default: they need new
formulation runs and time for eval evidence to accumulate. State the exposure
window in the report and do not compare cohorts with materially different
exposure.

For formation-time activation-condition experiments, cohort membership is
defined by rows in `memory_activation_conditions`: new-policy memories have
non-empty `activation_triggers_json` or `activation_anti_triggers_json`.
Reports should include condition coverage rate, trigger/anti-trigger examples,
and downstream recall usefulness for memories written after the policy became
active. While the cohort is still maturing, also report pending recall-eval
tasks that already selected activation-condition memories, separating tasks
that have later completed turns from tasks still waiting for future context.

Use `scripts/activation-condition-forward-report.py --policy-start <timestamp>`
to generate the forward-readiness artifact set for this experiment. The report
is read-only against the database; run any normal YAAML CLI command first if
the local database still needs the schema migration that creates
`memory_activation_conditions`.

## Artifact Requirements

### Replay (recall-selection) experiments

Each experiment directory should contain:

- `manifest.json`: run metadata, input paths, baseline label, strategy names,
  anchor library and holdout identifiers, and primary metrics.
- `summary.json`: strategy-level metrics plus deltas and bootstrap confidence
  intervals against the baseline.
- `details.jsonl`: one row per strategy per anchor with selected memory IDs
  and oracle comparison metrics.
- `REPORT.md`: human-readable readout generated from the structured files,
  including known-score coverage and CI-based conclusions.

### Forward (memory-write) experiments

Forward experiments have no anchor replay, so the per-anchor `details.jsonl`
is replaced by per-memory cohort data. Minimum artifact set:

- `manifest.json`: run metadata plus, required for forward experiments, the
  exposure window (start/end timestamps for each cohort) and the cohort
  definitions (what policy or config distinguishes new-policy memories from
  the comparison cohort, and how membership is determined).
- `cohorts.jsonl`: one row per memory with cohort assignment, creation
  metadata, and the memory-write metrics that apply (faithfulness,
  classification, downstream eval outcomes as they accumulate).
- `REPORT.md`: readout against the memory-write metric contract above,
  stating the exposure window and explicitly flagging any cohort-exposure
  imbalance. `summary.json` is optional; when cohorts are compared
  statistically, include it with the same CI fields as replay experiments.

### Diagnostic experiments

Diagnostics answer a question rather than compare policies (formation-miss
mining, the pool-recall oracle). Minimum artifact set:

- `manifest.json`: the question, input data paths, and how cases were
  gathered.
- `cases.jsonl`: one row per observed case — the raw evidence behind the
  answer (for formation-miss mining: missed opportunity, transcript refs,
  whether a memory should have existed).
- `REPORT.md`: the answer, with the counts/rates derived from the case rows.
  Paired deltas and CI fields apply only if the report makes a comparative
  claim.

### In all cases

After every experiment, append a decision-record entry to
`../../EXPERIMENTS_LOG.md` following its "When an item is addressed" policy:
date, sources, experiment, metrics, lessons, decision.

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
  --anchors-file experiments/recall/anchor-libraries/<YYYY-MM>-screening.tsv \
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

Live tooling:

- `scripts/backtest-recall-strategy.sh`: replay the production recall path
  over an anchor set (fixed, eval-library, or `BACKTEST_ANCHORS_FILE`).
- `scripts/build-anchor-library.py`: split a backtest anchor pool into
  frozen screening/holdout libraries (Phase 0 runbook above).
- `scripts/recall_experiment_stats.py`: shared paired-bootstrap CI and
  verdict classification; import it from any new experiment runner.
- `scripts/recall-5x5-worktree-metrics.py`: agent-driven 5x5 loop over
  baseline plus candidate worktrees, with CI output.
- `scripts/export-recall-training-data.py`: convert saved backtest artifacts
  into candidate-level JSONL for selection experiments.
- `scripts/recall-feature-model-experiment.py`: run the learned weight
  calibration backlog item as a saved-candidate replay with logistic scoring,
  isotonic calibration, threshold tuning, per-anchor details, paired
  bootstrap CIs, and a report.
- `scripts/llm-rerank-recall-candidates.py`: run a saved-candidate LLM
  scoring rerank over cached recall candidates, with resumable score caching,
  fold-tuned thresholds, paired bootstrap CIs, and a report.
- `scripts/formation-miss-mining.py`: scan indexed transcript files for
  repeated correction-like user turns across sessions, group possible
  missed-formation clusters, check apparent active-memory coverage, and write
  diagnostic artifacts.
- `scripts/session-context-dedup-experiment.py`: replay saved selections with
  a session/context visibility suppression policy, emit paired bootstrap CIs,
  and write drop diagnostics for context-dedup analysis.

One-shot replay harnesses for concluded experiments (the 5x5/10x10,
candidate, techniques, health, segment-task-fit, cluster-rerank, and
tool-cooldown scripts) have been deleted; their results live in the dated
decision records in `../../EXPERIMENTS_LOG.md` and the scripts remain in git
history if a readout ever needs re-deriving. New experiments should build on
the live tooling above rather than reviving them, since none of the deleted
scripts met the confidence-interval methodology.

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
