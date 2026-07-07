# Session-Level Context Dedup

Date: 2026-07-07
Baseline: `wide-multi-query-candidate-generation-v2`
Anchors: `experiments/recall/anchor-libraries/2026-07-screening.tsv`

## Experiment

Replay saved selections and drop a selected memory when it was already selected earlier in the same session, or when the memory text has strong visible overlap with the current recall query.

## Results

| Metric | Baseline | Dedup | Delta | 95% CI | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `average_known_score` | 4.667 | 4.500 | -0.167 | [-0.667, 0.000] | needs larger sample |
| `useful_known_selected` | 3.000 | 2.000 | -1.000 | [-3.000, 0.000] | no detectable effect |
| `low_known_selected` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `useful_capture_runs` | 3.000 | 2.000 | -1.000 | [-3.000, 0.000] | no detectable effect |
| `low_selection_runs` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `average_selected_per_anchor` | 0.610 | 0.180 | -0.430 | [-0.505, -0.365] | confirmed |
| `empty_recall_runs` | 89.000 | 166.000 | 77.000 | [64.000, 90.000] | confirmed |
| `missed_useful_empty_runs` | 42.000 | 81.000 | 39.000 | [29.000, 50.000] | confirmed |
| `clean_abstention_runs` | 47.000 | 85.000 | 38.000 | [27.000, 49.000] | confirmed |

## Dedup Drops

- Selected memories dropped: 86
- Previously recalled same session: 44
- Visible in current query: 42

## Decision

Do not ship this drop-only context-dedup policy as a recall-quality change. It reduces selected context, but the screening set has too few known selected labels to prove it preserves useful recall; use these drops as diagnostic cases for a richer session-context feature rather than a hard gate.

## Artifacts

- `manifest.json` records inputs, summaries, deltas, and drop counts.
- `baseline.details.jsonl` is the copied baseline detail rows.
- `session_context_dedup.details.jsonl` is the replayed candidate rows.
- `drops.jsonl` records dropped memory ids and reasons.
