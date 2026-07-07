# Adjudicator-Gated Source-Overlap Restoration (Offline Simulation)

Date: 2026-07-07
Gate: restore `drop:source_turn_already_in_query` candidates with cached redundancy-aware score >= 4; unscored candidates stay dropped.
Gate-restored selections: 15

## Summary (raw dense oracle and redundancy-adjusted oracle)

| Strategy | Avg score | Useful sel | Low sel | Useful runs | Low runs | Avg mem | Empty | Missed-useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| replay_default_raw | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| allow_source_overlap_raw | 3.74 | 58 | 26 | 50 | 24 | 0.84 | 26 | 24 |
| gated_overlap_raw | 3.61 | 46 | 22 | 38 | 21 | 0.68 | 41 | 39 |
| replay_default_adjusted | 3.38 | 31 | 22 | 29 | 21 | 0.53 | 50 | 48 |
| allow_source_overlap_adjusted | 3.11 | 46 | 38 | 38 | 36 | 0.84 | 26 | 24 |
| gated_overlap_adjusted | 3.55 | 46 | 22 | 38 | 21 | 0.68 | 41 | 39 |

## Significance (paired bootstrap vs `replay_default`, 95% CI)

| Arm x oracle | Metric | Delta | 95% CI | Verdict |
| --- | --- | ---: | ---: | --- |
| allow_source_overlap_raw | `average_known_score` | +0.363 | [+0.134, +0.602] | confirmed |
| allow_source_overlap_raw | `useful_known_selected` | +27.000 | [+18.000, +37.000] | confirmed |
| allow_source_overlap_raw | `low_known_selected` | +4.000 | [+1.000, +8.000] | confirmed |
| allow_source_overlap_raw | `useful_capture_runs` | +21.000 | [+13.000, +29.000] | confirmed |
| allow_source_overlap_raw | `low_selection_runs` | +3.000 | [+0.000, +7.000] | needs larger sample |
| allow_source_overlap_raw | `average_selected_per_anchor` | +0.310 | [+0.220, +0.410] | confirmed |
| allow_source_overlap_raw | `empty_recall_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap_raw | `missed_useful_empty_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap_raw | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_raw | `average_known_score` | +0.230 | [+0.080, +0.411] | confirmed |
| gated_overlap_raw | `useful_known_selected` | +15.000 | [+8.000, +23.000] | confirmed |
| gated_overlap_raw | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_raw | `useful_capture_runs` | +9.000 | [+4.000, +15.000] | confirmed |
| gated_overlap_raw | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_raw | `average_selected_per_anchor` | +0.150 | [+0.080, +0.230] | confirmed |
| gated_overlap_raw | `empty_recall_runs` | -9.000 | [-15.000, -4.000] | confirmed |
| gated_overlap_raw | `missed_useful_empty_runs` | -9.000 | [-15.000, -4.000] | confirmed |
| gated_overlap_raw | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| allow_source_overlap_adjusted | `average_known_score` | -0.265 | [-0.545, -0.012] | confirmed |
| allow_source_overlap_adjusted | `useful_known_selected` | +15.000 | [+8.000, +23.000] | confirmed |
| allow_source_overlap_adjusted | `low_known_selected` | +16.000 | [+9.000, +24.000] | confirmed |
| allow_source_overlap_adjusted | `useful_capture_runs` | +9.000 | [+4.000, +15.000] | confirmed |
| allow_source_overlap_adjusted | `low_selection_runs` | +15.000 | [+8.000, +23.000] | confirmed |
| allow_source_overlap_adjusted | `average_selected_per_anchor` | +0.310 | [+0.220, +0.410] | confirmed |
| allow_source_overlap_adjusted | `empty_recall_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap_adjusted | `missed_useful_empty_runs` | -24.000 | [-33.000, -16.000] | confirmed |
| allow_source_overlap_adjusted | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_adjusted | `average_known_score` | +0.171 | [+0.037, +0.331] | confirmed |
| gated_overlap_adjusted | `useful_known_selected` | +15.000 | [+8.000, +23.000] | confirmed |
| gated_overlap_adjusted | `low_known_selected` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_adjusted | `useful_capture_runs` | +9.000 | [+4.000, +15.000] | confirmed |
| gated_overlap_adjusted | `low_selection_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |
| gated_overlap_adjusted | `average_selected_per_anchor` | +0.150 | [+0.080, +0.230] | confirmed |
| gated_overlap_adjusted | `empty_recall_runs` | -9.000 | [-15.000, -4.000] | confirmed |
| gated_overlap_adjusted | `missed_useful_empty_runs` | -9.000 | [-15.000, -4.000] | confirmed |
| gated_overlap_adjusted | `clean_abstention_runs` | +0.000 | [+0.000, +0.000] | no detectable effect |

## Caveats

- The gate and the adjusted oracle share the redundancy-aware adjudicator, so adjusted deltas partially evaluate the gate with its own judge. The independent evidence is the raw dense-label deltas plus two-instrument agreement; forward production evals are the confirmation path before any default-on ship.
- The lexical-containment alternative was invalidated by calibration: incremental mean containment 0.444 vs redundant 0.481 on the 82 labeled restored selections — no usable separation at any threshold.
- Unscored source-overlap candidates stay dropped in this simulation; a runtime gate would score them live (async daemon path).

## Decision

Gated restoration is promising: adjusted useful-capture CI excludes zero without a confirmed low-selection increase. Next step is a runtime implementation behind an env flag (async daemon-side adjudicator call for source-overlap-dropped candidates) with forward production evals as the independent confirmation, given the shared-instrument caveat.
