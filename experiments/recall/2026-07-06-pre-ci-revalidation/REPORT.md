# Pre-CI Decision Revalidation

## Health Action Rerank

Baseline: `no_health_action_rerank_proxy`
Candidate: `production_current`

| Metric | Baseline | Candidate | Delta | CI 95% | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| average_known_score | 2.25 | 4.67 | +2.42 | [0.67, 4.00] | confirmed |
| useful_known_selected | 1 | 3 | +2.00 | [0.00, 5.00] | no detectable effect |
| low_known_selected | 3 | 0 | -3.00 | [-7.00, 0.00] | needs larger sample |
| useful_capture_runs | 1 | 3 | +2.00 | [0.00, 5.00] | needs larger sample |
| low_selection_runs | 3 | 0 | -3.00 | [-7.00, 0.00] | needs larger sample |
| average_selected_per_anchor | 0.89 | 0.53 | -0.35 | [-0.44, -0.27] | confirmed |
| empty_recall_runs | 35 | 101 | +66.00 | [52.00, 80.00] | confirmed |
| missed_useful_empty_runs | 11 | 45 | +34.00 | [23.00, 45.00] | confirmed |
| clean_abstention_runs | 24 | 56 | +32.00 | [22.00, 42.00] | confirmed |

Decision: tune `health_action_rerank`, not a clean keep. The refreshed screening replay confirms lower context volume and better average known score, but it also confirms a large missed-useful-empty regression; future work should reduce over-abstention before treating this as fully validated.

Note: this is a saved-candidate proxy that reverses encoded health deltas; noisy-metadata task-key restoration is not reconstructable from saved JSON, so the no-health baseline is conservative rather than exact.

## Recall Query Cleaning

Baseline: `no_query_cleaning`
Candidate: `production_current`

| Metric | Baseline | Candidate | Delta | CI 95% | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| average_known_score | 4.50 | 4.67 | +0.17 | [0.00, 0.67] | needs larger sample |
| useful_known_selected | 4 | 3 | -1.00 | [-3.00, 0.00] | no detectable effect |
| low_known_selected | 0 | 0 | +0.00 | [0.00, 0.00] | no detectable effect |
| useful_capture_runs | 4 | 3 | -1.00 | [-3.00, 0.00] | no detectable effect |
| low_selection_runs | 0 | 0 | +0.00 | [0.00, 0.00] | no detectable effect |
| average_selected_per_anchor | 0.53 | 0.53 | +0.00 | [-0.03, 0.03] | no detectable effect |
| empty_recall_runs | 101 | 101 | +0.00 | [-6.00, 5.00] | needs larger sample |
| missed_useful_empty_runs | 43 | 45 | +2.00 | [0.00, 5.00] | needs larger sample |
| clean_abstention_runs | 58 | 56 | -2.00 | [-7.00, 3.00] | needs larger sample |

Decision: keep recall-query cleaning. The refreshed screening replay shows no detectable effect on useful selected memories, useful capture runs, low selections, or average selected memories; the small point regression in useful capture is not confirmed by CI.

Note: this comparison reruns retrieval because query construction changes the embedding query.
