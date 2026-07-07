# Adjudicator-Gated Source-Overlap Restoration (Offline Simulation)

Date: 2026-07-07
Gate: restore `drop:source_turn_already_in_query` candidates with cached redundancy-aware score >= 4; unscored candidates stay dropped.
Gate-restored selections: 16

## Summary (raw dense oracle and redundancy-adjusted oracle)

| Strategy | Avg score | Useful sel | Low sel | Useful runs | Low runs | Avg mem | Empty | Missed-useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| replay_default_raw | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 93 |
| allow_source_overlap_raw | 3.89 | 113 | 43 | 105 | 41 | 0.79 | 53 | 46 |
| gated_overlap_raw | 3.81 | 86 | 35 | 79 | 33 | 0.61 | 87 | 79 |
| replay_default_adjusted | 3.65 | 70 | 35 | 65 | 33 | 0.53 | 101 | 90 |
| allow_source_overlap_adjusted | 3.19 | 86 | 70 | 79 | 67 | 0.79 | 53 | 46 |
| gated_overlap_adjusted | 3.75 | 86 | 35 | 79 | 33 | 0.61 | 87 | 76 |

## Significance (paired bootstrap vs `replay_default`, 95% CI)

| Arm x oracle | Metric | Delta | 95% CI | Verdict |
| --- | --- | ---: | ---: | --- |
| allow_source_overlap_raw | `average_known_score` | +0.240 | [+0.094, +0.404] | confirmed |
| allow_source_overlap_raw | `useful_known_selected` | +43.000 | [+31.000, +56.000] | confirmed |
| allow_source_overlap_raw | `low_known_selected` | +8.000 | [+3.000, +14.000] | confirmed |
| allow_source_overlap_raw | `useful_capture_runs` | +40.000 | [+29.000, +52.000] | confirmed |
| allow_source_overlap_raw | `low_selection_runs` | +8.000 | [+3.000, +14.000] | confirmed |
| allow_source_overlap_raw | `average_selected_per_anchor` | +0.255 | [+0.190, +0.320] | confirmed |
| allow_source_overlap_raw | `empty_recall_runs` | -48.000 | [-61.000, -36.000] | confirmed |
| allow_source_overlap_raw | `missed_useful_empty_runs` | -47.000 | [-60.000, -35.000] | confirmed |
| allow_source_overlap_raw | `clean_abstention_runs` | -1.000 | [-3.000, +0.000] | no detectable effect |
| gated_overlap_raw | `average_known_score` | +0.154 | [+0.077, +0.246] | confirmed |
| gated_overlap_raw | `useful_known_selected` | +16.000 | [+9.000, +24.000] | confirmed |
| gated_overlap_raw | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_raw | `useful_capture_runs` | +14.000 | [+7.000, +22.000] | confirmed |
| gated_overlap_raw | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_raw | `average_selected_per_anchor` | +0.080 | [+0.045, +0.120] | confirmed |
| gated_overlap_raw | `empty_recall_runs` | -14.000 | [-22.000, -7.000] | confirmed |
| gated_overlap_raw | `missed_useful_empty_runs` | -14.000 | [-22.000, -7.000] | confirmed |
| gated_overlap_raw | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| allow_source_overlap_adjusted | `average_known_score` | -0.464 | [-0.667, -0.280] | confirmed |
| allow_source_overlap_adjusted | `useful_known_selected` | +16.000 | [+9.000, +24.000] | confirmed |
| allow_source_overlap_adjusted | `low_known_selected` | +35.000 | [+24.000, +47.000] | confirmed |
| allow_source_overlap_adjusted | `useful_capture_runs` | +14.000 | [+7.000, +22.000] | confirmed |
| allow_source_overlap_adjusted | `low_selection_runs` | +34.000 | [+23.000, +45.000] | confirmed |
| allow_source_overlap_adjusted | `average_selected_per_anchor` | +0.255 | [+0.190, +0.320] | confirmed |
| allow_source_overlap_adjusted | `empty_recall_runs` | -48.000 | [-61.000, -36.000] | confirmed |
| allow_source_overlap_adjusted | `missed_useful_empty_runs` | -44.000 | [-56.000, -32.000] | confirmed |
| allow_source_overlap_adjusted | `clean_abstention_runs` | -4.000 | [-8.000, -1.000] | confirmed |
| gated_overlap_adjusted | `average_known_score` | +0.096 | [+0.036, +0.170] | confirmed |
| gated_overlap_adjusted | `useful_known_selected` | +16.000 | [+9.000, +24.000] | confirmed |
| gated_overlap_adjusted | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_adjusted | `useful_capture_runs` | +14.000 | [+7.000, +22.000] | confirmed |
| gated_overlap_adjusted | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_adjusted | `average_selected_per_anchor` | +0.080 | [+0.045, +0.120] | confirmed |
| gated_overlap_adjusted | `empty_recall_runs` | -14.000 | [-22.000, -7.000] | confirmed |
| gated_overlap_adjusted | `missed_useful_empty_runs` | -14.000 | [-22.000, -7.000] | confirmed |
| gated_overlap_adjusted | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |

## Caveats

- The gate and the adjusted oracle share the redundancy-aware adjudicator, so adjusted deltas partially evaluate the gate with its own judge. The independent evidence is the raw dense-label deltas plus two-instrument agreement; forward production evals are the confirmation path before any default-on ship.
- The lexical-containment alternative was invalidated by calibration: incremental mean containment 0.444 vs redundant 0.481 on the 82 labeled restored selections — no usable separation at any threshold.
- Unscored source-overlap candidates stay dropped in this simulation; a runtime gate would score them live (async daemon path).

## Decision

Gated restoration is promising: adjusted useful-capture CI excludes zero without a confirmed low-selection increase. Next step is a runtime implementation behind an env flag (async daemon-side adjudicator call for source-overlap-dropped candidates) with forward production evals as the independent confirmation, given the shared-instrument caveat.
