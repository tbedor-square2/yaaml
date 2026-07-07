# Selection/Health-Rerank Tuning on Dense Labels

Date: 2026-07-07
Input directory: `target/anchor-refresh-2026-07`
Oracle: `experiments/recall/oracle-labels/2026-07/dense-oracles` (dense adjudicator labels)

## Experiment

Replayed saved production candidate rankings through parameterized
strict-kind selection variants (abstention threshold, second-slot
threshold, scaled negative health-rerank deltas) plus a cached
LLM-score arm, scored against the dense oracle. Saved-candidate
replay; no provider calls.

Replay fidelity: the default-parameter replay reproduced production
selections on 100.0% of anchors.

## Summary

| Strategy | Avg score | Useful sel | Low sel | Useful runs | Low runs | Avg mem | Empty | Missed-useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| replay_default | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| abstain_055 | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| abstain_040 | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| second_075 | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| health_neg_half | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| combo_soft | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| allow_source_overlap | 3.74 | 58 | 26 | 50 | 24 | 0.84 | 26 | 24 |
| overlap_plus_soft | 3.74 | 58 | 26 | 50 | 24 | 0.84 | 26 | 24 |

## Significance (paired bootstrap vs `replay_default`, 95% CI)

Parameter arms are compared proxy-vs-proxy against the faithful
default-parameter replay so replay drift does not confound the
deltas. `replay_default_vs_production` shows the residual replay
gap against true saved production selections.

| Strategy | Metric | Delta | 95% CI | Verdict |
| --- | --- | ---: | ---: | --- |
| abstain_055 | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_055 | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| abstain_040 | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| second_075 | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| health_neg_half | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| combo_soft | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| allow_source_overlap | `average_known_score` | +0.363 | [+0.134, +0.602] | confirmed |
| allow_source_overlap | `useful_known_selected` | +27.000 | [+18.000, +37.000] | confirmed |
| allow_source_overlap | `low_known_selected` | +4.000 | [+1.000, +8.000] | confirmed |
| allow_source_overlap | `useful_capture_runs` | +21.000 | [+13.000, +29.000] | confirmed |
| allow_source_overlap | `low_selection_runs` | +3.000 | [+0.000, +7.000] | needs larger sample |
| allow_source_overlap | `average_selected_per_anchor` | +0.310 | [+0.220, +0.410] | confirmed |
| allow_source_overlap | `empty_recall_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap | `missed_useful_empty_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| overlap_plus_soft | `average_known_score` | +0.363 | [+0.134, +0.602] | confirmed |
| overlap_plus_soft | `useful_known_selected` | +27.000 | [+18.000, +37.000] | confirmed |
| overlap_plus_soft | `low_known_selected` | +4.000 | [+1.000, +8.000] | confirmed |
| overlap_plus_soft | `useful_capture_runs` | +21.000 | [+13.000, +29.000] | confirmed |
| overlap_plus_soft | `low_selection_runs` | +3.000 | [+0.000, +7.000] | needs larger sample |
| overlap_plus_soft | `average_selected_per_anchor` | +0.310 | [+0.220, +0.410] | confirmed |
| overlap_plus_soft | `empty_recall_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| overlap_plus_soft | `missed_useful_empty_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| overlap_plus_soft | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |

## Decision

Promising on screening: `allow_source_overlap`, `overlap_plus_soft` improved useful-capture runs with a CI excluding zero. Confirm the best arm on the holdout before shipping, and weigh the confirmed low-selection and context-volume costs.

## Artifacts

- `manifest.json`: parameters, summaries, paired deltas.
- `<strategy>.details.jsonl`: per-anchor selections per arm.
