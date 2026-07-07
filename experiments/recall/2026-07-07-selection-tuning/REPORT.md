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
| production | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| replay_default | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| abstain_055 | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| abstain_040 | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| second_075 | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| health_neg_half | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| combo_soft | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| allow_source_overlap | 3.89 | 113 | 43 | 105 | 41 | 0.79 | 53 | 46 |
| overlap_plus_soft | 3.89 | 113 | 43 | 105 | 41 | 0.79 | 53 | 46 |
| llm_t2 | 3.66 | 64 | 31 | 62 | 31 | 0.48 | 106 | 98 |

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
| allow_source_overlap | `average_known_score` | +0.240 | [+0.094, +0.404] | confirmed |
| allow_source_overlap | `useful_known_selected` | +43.000 | [+31.000, +56.000] | confirmed |
| allow_source_overlap | `low_known_selected` | +8.000 | [+3.000, +14.000] | confirmed |
| allow_source_overlap | `useful_capture_runs` | +40.000 | [+29.000, +52.000] | confirmed |
| allow_source_overlap | `low_selection_runs` | +8.000 | [+3.000, +14.000] | confirmed |
| allow_source_overlap | `average_selected_per_anchor` | +0.255 | [+0.190, +0.320] | confirmed |
| allow_source_overlap | `empty_recall_runs` | -48.000 | [-61.000, -36.000] | confirmed |
| allow_source_overlap | `missed_useful_empty_runs` | -47.000 | [-60.000, -35.000] | confirmed |
| allow_source_overlap | `clean_abstention_runs` | -1.000 | [-3.000, +0.000] | no detectable effect |
| overlap_plus_soft | `average_known_score` | +0.240 | [+0.094, +0.404] | confirmed |
| overlap_plus_soft | `useful_known_selected` | +43.000 | [+31.000, +56.000] | confirmed |
| overlap_plus_soft | `low_known_selected` | +8.000 | [+3.000, +14.000] | confirmed |
| overlap_plus_soft | `useful_capture_runs` | +40.000 | [+29.000, +52.000] | confirmed |
| overlap_plus_soft | `low_selection_runs` | +8.000 | [+3.000, +14.000] | confirmed |
| overlap_plus_soft | `average_selected_per_anchor` | +0.255 | [+0.190, +0.320] | confirmed |
| overlap_plus_soft | `empty_recall_runs` | -48.000 | [-61.000, -36.000] | confirmed |
| overlap_plus_soft | `missed_useful_empty_runs` | -47.000 | [-60.000, -35.000] | confirmed |
| overlap_plus_soft | `clean_abstention_runs` | -1.000 | [-3.000, +0.000] | no detectable effect |
| llm_t2 | `average_known_score` | +0.013 | [-0.051, +0.090] | no detectable effect |
| llm_t2 | `useful_known_selected` | -6.000 | [-11.000, -2.000] | confirmed |
| llm_t2 | `low_known_selected` | -4.000 | [-8.000, -1.000] | confirmed |
| llm_t2 | `useful_capture_runs` | -3.000 | [-7.000, +0.000] | needs larger sample |
| llm_t2 | `low_selection_runs` | -2.000 | [-5.000, +0.000] | needs larger sample |
| llm_t2 | `average_selected_per_anchor` | -0.050 | [-0.080, -0.020] | confirmed |
| llm_t2 | `empty_recall_runs` | +5.000 | [+1.000, +10.000] | confirmed |
| llm_t2 | `missed_useful_empty_runs` | +5.000 | [+1.000, +10.000] | confirmed |
| llm_t2 | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `average_known_score` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `useful_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `useful_capture_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `average_selected_per_anchor` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `empty_recall_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `missed_useful_empty_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| replay_default_vs_production | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |

## LLM Arm Coverage

- Eligible candidates with a cached LLM score: 100 of 106 (94.3%). Scores were cached from wide-v2 pools; uncovered candidates cannot be selected by this arm.

## Decision

No arm improved useful-capture runs with a CI excluding zero; record as no detectable effect and do not ship.

## Artifacts

- `manifest.json`: parameters, summaries, paired deltas.
- `<strategy>.details.jsonl`: per-anchor selections per arm.
