# Hybrid Candidate Generation

Date: 2026-07-06
Anchors: `experiments/recall/anchor-libraries/2026-07-screening.tsv` (200 anchors)

## Experiment

Tested an env-gated hybrid retrieval pool: primary active-segment vector hits, segment-summary/task-key vector hits, and exact identity-key memory hits. The v2 rerun also extracted identity keys from memory title/body text before exact matching.

## Results

| Metric | Production | Hybrid v2 | Delta | 95% CI | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `average_known_score` | 4.667 | 4.250 | -0.417 | [-1.000, 0.000] | needs larger sample |
| `useful_known_selected` | 3.000 | 4.000 | 1.000 | [-2.000, 4.000] | no detectable effect |
| `low_known_selected` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `useful_capture_runs` | 3.000 | 4.000 | 1.000 | [-2.000, 4.000] | no detectable effect |
| `low_selection_runs` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `average_selected_per_anchor` | 0.530 | 0.750 | 0.220 | [0.150, 0.295] | confirmed |
| `empty_recall_runs` | 101.000 | 73.000 | -28.000 | [-39.000, -18.000] | confirmed |
| `missed_useful_empty_runs` | 45.000 | 36.000 | -9.000 | [-16.000, -2.000] | confirmed |
| `clean_abstention_runs` | 56.000 | 37.000 | -19.000 | [-27.000, -11.000] | confirmed |

## Pool Ceiling

- Phase 0 production pool recall: 18/102 (17.6%).
- Hybrid v2 pool recall: 21/102 (20.6%).
- Hybrid v2 known-useful selected: 4/102 (3.9%).
- Remaining misses: 81 retrieval misses and 17 selection misses.

## Decision

Do not ship this hybrid variant. It confirmed lower abstention, including fewer missed-useful empty recalls, but it did not confirm useful-known selected/capture gains and increased selected context volume. The candidate-generation ceiling moved only from 18 to 21 known-useful anchors in pool, so retrieval remains the binding problem.

## Artifacts

- `manifest.json` records inputs, summaries, deltas, and pool-oracle metrics.
- `production.details.jsonl` is the 200-anchor filtered production baseline.
- `hybrid-candidate-generation-v2.details.jsonl` is the 200-anchor hybrid candidate run.
- `../2026-07-06-hybrid-candidate-generation-v2-pool-oracle/` records the pool oracle cases.
