# Source-Overlap Redundancy Check

Date: 2026-07-07

## Question

The selection-tuning replay found that allowing candidates dropped by
`drop:source_turn_already_in_query` back into selection adds +40
useful-capture runs on screening (CI +29 to +52) and +21 on holdout (CI +13
to +29) under the dense oracle. But the dense adjudicator scores (query,
memory) pairs without penalizing redundancy, and source-overlap suppression
exists precisely because those memories' source turns are in the current
query. Is the gain real or a labeling artifact?

## Method

Re-scored all 82 restored selections (screening + holdout) with the same
adjudicator prompt plus an explicit incremental-value instruction: if the
memory's content is already visible verbatim or near-verbatim in the query
text, score 1-2 regardless of topical relevance.

## Results

- Restored selections: 82
- Dense-oracle useful (>=4): 70
- Still useful under redundancy-aware scoring: 31 (44.3%)
- Demoted to low (<=2) as redundant: 39

## Interpretation

Roughly half the apparent useful gain from removing the source-overlap gate
is redundancy-blind labeling artifact; the other half is genuinely
incremental context that production currently discards. Two consequences:

1. Do not ship a blanket removal of the source-overlap drop. Scale the
   confirmed deltas by ~0.44: still positive (~+18 useful runs on
   screening), but the unmeasured context cost of the redundant half argues
   for a conditional gate instead.
2. The dense oracle inherits this redundancy blindness for any memory whose
   content is visible in the query. Experiments that change
   source-overlap/dedup behavior must not rely on raw dense labels alone;
   use a redundancy-aware re-score of the differing selections, as here.

## Caveats

The redundancy-aware instruction is a new, uncalibrated instrument (the
calibration study validated the plain adjudicator prompt). Treat the 44%
survival rate as an estimate, not a precise scaling factor.

## Artifacts

- `cases.jsonl`: per-restored-selection dense score, redundancy-aware score,
  and rationale.
- `manifest.json`: question, inputs, method, results.
