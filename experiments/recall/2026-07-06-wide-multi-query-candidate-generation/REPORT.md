# Wide Multi-Query Candidate Generation

Date: 2026-07-06
Anchors: `experiments/recall/anchor-libraries/2026-07-screening.tsv` (200 anchors)

## Experiment

Tested an env-gated wider retrieval pool with `YAAML_EXPERIMENT_WIDE_RECALL=1`.
The variant used pool 64, similarity threshold 0.20, separate query embeddings
for the current recall query, active segment summary, and active identity keys,
and widened the selector/debug pool to 64 while leaving the final selection
limit unchanged.

## Results

| Metric | Production | Wide v2 | Delta | 95% CI | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `average_known_score` | 4.667 | 4.667 | 0.000 | [0.000, 0.000] | no detectable effect |
| `useful_known_selected` | 3.000 | 3.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `low_known_selected` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `useful_capture_runs` | 3.000 | 3.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `low_selection_runs` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `average_selected_per_anchor` | 0.530 | 0.610 | 0.080 | [0.035, 0.125] | confirmed |
| `empty_recall_runs` | 101.000 | 89.000 | -12.000 | [-20.000, -4.000] | confirmed |
| `missed_useful_empty_runs` | 45.000 | 42.000 | -3.000 | [-8.000, 2.000] | needs larger sample |
| `clean_abstention_runs` | 56.000 | 47.000 | -9.000 | [-16.000, -2.000] | confirmed |

## Pool Ceiling

- Raw Phase 0 production pool recall: 18/102 (17.6%).
- Raw hybrid v2 pool recall: 21/102 (20.6%).
- Raw wide v2 pool recall: 22/102 (21.6%).
- Active-only production pool recall: 18/28 (64.3%).
- Active-only wide v2 pool recall: 22/28 (78.6%).
- Active-only wide v2 known-useful selected: 3/28 (10.7%).
- Active-only remaining misses: 6 retrieval misses and 19 selection misses.

## Decision

Do not ship this wide multi-query variant. It confirmed lower abstention and
more selected context, but it did not improve useful-known selection or useful
capture.

The active-only pool oracle changes the bottleneck diagnosis: most raw
retrieval misses are inactive memories that production recall intentionally
excludes. Among active useful memories, selection is now the larger miss class.
The next work should prioritize selection/ranking calibration before another
broad candidate-generation heuristic.

## Artifacts

- `manifest.json` records inputs, summaries, deltas, and pool-oracle metrics.
- `production.details.jsonl` is the 200-anchor filtered production baseline.
- `wide-multi-query-candidate-generation-v2.details.jsonl` is the 200-anchor wide candidate run.
- `../2026-07-06-wide-multi-query-candidate-generation-v2-pool-oracle/` records the pool oracle cases.
