# Recall Candidate 5x5 Experiments

Date: 2026-06-22
Input: `target/strict-kind-production`
Anchors: 200
Cohorts: 5

## Method

Each family compares five strategies across five deterministic cohorts using saved `yaaml recall --debug-ranking` outputs and saved eval oracle labels.
No embedding or LLM provider calls are made during replay.
Scoring only uses selected memories with saved eval labels. Empty recall is reported as abstention, not as a low score.

Important limitation: candidate-pool variants can only use the 16 candidates saved in the input artifacts. Query-signal and hybrid-generation families are replay proxies over existing candidates; they do not measure candidates that a different query embedding or lexical retrieval pass would have newly retrieved.

## Readout

- candidate_pool: best balanced `pool_5` (score=3.19, useful_runs=53, low=56, missed_empty=6). Highest raw score `pool_8` (3.21); lowest lows `pool_5` (low=56, useful=74). Baseline `pool_3` score=3.15, useful_runs=46, low=49.
- dynamic_recall_count: best balanced `top_2` (score=3.42, useful_runs=54, low=40, missed_empty=6). Highest raw score `top_2` (3.42); lowest lows `top_1` (low=23, useful=36). Baseline `production_health_action` score=3.21, useful_runs=54, low=57.
- query_signal_proxy: best balanced `project_context_proxy` (score=3.26, useful_runs=56, low=50, missed_empty=3). Highest raw score `vector_only_proxy` (4.00); lowest lows `vector_only_proxy` (low=0, useful=1). Baseline `production_health_action` score=3.21, useful_runs=54, low=57. `vector_only_proxy` has a misleading high score because it missed 87 useful-empty cases.
- hybrid_generation_proxy: best balanced `durable_kind_boost` (score=3.15, useful_runs=55, low=59, missed_empty=5). Highest raw score `strong_task_first` (3.17); lowest lows `strong_task_first` (low=57, useful=74). Baseline `production_health_action` score=3.21, useful_runs=54, low=57.
- memory_lifecycle: best balanced `suppress_vague` (score=3.22, useful_runs=54, low=56, missed_empty=6). Highest raw score `suppress_vague` (3.22); lowest lows `suppress_vague` (low=56, useful=75). Baseline `production_health_action` score=3.21, useful_runs=54, low=57.
- abstention_gate: best balanced `score_floor_1_05` (score=3.35, useful_runs=54, low=45, missed_empty=7). Highest raw score `score_floor_1_15` (3.44); lowest lows `score_floor_1_15` (low=39, useful=67). Baseline `production_health_action` score=3.21, useful_runs=54, low=57.

## Candidate Pool

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| pool_3 | 3.15 | 58 | 49 | 46 | 38 | 1.47 | 23.5% | 7.4% |
| pool_5 | 3.19 | 74 | 56 | 53 | 44 | 1.72 | 22.5% | 6.4% |
| pool_8 | 3.21 | 75 | 57 | 54 | 44 | 1.81 | 22.5% | 6.4% |
| pool_12 | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| pool_16 | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| pool_5 | +0.04 | +16 | +7 | +7 | +6 | +0.25 | -1 |
| pool_8 | +0.05 | +17 | +8 | +8 | +6 | +0.34 | -1 |
| pool_12 | +0.05 | +17 | +8 | +8 | +6 | +0.40 | -1 |
| pool_16 | +0.05 | +17 | +8 | +8 | +6 | +0.40 | -1 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| pool_3 | n/a | n/a | 2.94-3.43 |
| pool_5 | 5 | 1 | 2.93-3.60 |
| pool_8 | 5 | 2 | 2.83-3.60 |
| pool_12 | 5 | 2 | 2.83-3.60 |
| pool_16 | 5 | 2 | 2.83-3.60 |

## Dynamic Recall Count

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| top_1 | 3.27 | 36 | 23 | 36 | 23 | 0.78 | 22.5% | 6.4% |
| top_2 | 3.42 | 70 | 40 | 54 | 32 | 1.44 | 22.5% | 6.4% |
| within_0_15_of_top | 3.34 | 58 | 36 | 45 | 27 | 1.10 | 22.5% | 6.4% |
| stop_on_0_20_gap | 3.33 | 59 | 39 | 46 | 29 | 1.24 | 22.5% | 6.4% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| top_1 | +0.06 | -39 | -34 | -18 | -21 | -1.09 | +0 |
| top_2 | +0.21 | -5 | -17 | +0 | -12 | -0.43 | +0 |
| within_0_15_of_top | +0.13 | -17 | -21 | -9 | -17 | -0.76 | +0 |
| stop_on_0_20_gap | +0.12 | -16 | -18 | -8 | -15 | -0.62 | +0 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.83-3.60 |
| top_1 | 0 | 4 | 3.00-3.73 |
| top_2 | 0 | 4 | 3.11-3.79 |
| within_0_15_of_top | 0 | 4 | 3.04-3.62 |
| stop_on_0_20_gap | 0 | 4 | 2.98-3.62 |

## Query Signal Proxy

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| vector_only_proxy | 4.00 | 1 | 0 | 1 | 0 | 0.06 | 95.5% | 92.6% |
| context_heavy_proxy | 3.20 | 78 | 54 | 57 | 42 | 2.21 | 17.0% | 3.2% |
| task_key_heavy_proxy | 3.15 | 75 | 56 | 55 | 45 | 2.12 | 17.5% | 3.2% |
| project_context_proxy | 3.26 | 77 | 50 | 56 | 40 | 2.20 | 16.5% | 3.2% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| vector_only_proxy | +0.79 | -74 | -57 | -53 | -44 | -1.81 | +81 |
| context_heavy_proxy | -0.00 | +3 | -3 | +3 | -2 | +0.35 | -3 |
| task_key_heavy_proxy | -0.05 | +0 | -1 | +1 | +1 | +0.25 | -3 |
| project_context_proxy | +0.05 | +2 | -7 | +2 | -4 | +0.34 | -3 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.83-3.60 |
| vector_only_proxy | 0 | 5 | 0.00-4.00 |
| context_heavy_proxy | 3 | 3 | 2.94-3.56 |
| task_key_heavy_proxy | 1 | 3 | 2.83-3.47 |
| project_context_proxy | 2 | 3 | 2.98-3.72 |

## Hybrid Generation Proxy

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| task_signal_boost | 3.15 | 75 | 60 | 54 | 47 | 1.90 | 22.5% | 6.4% |
| strong_task_first | 3.17 | 74 | 57 | 53 | 45 | 1.85 | 22.5% | 6.4% |
| project_bonus_boost | 3.07 | 76 | 64 | 55 | 51 | 2.05 | 17.0% | 3.2% |
| durable_kind_boost | 3.15 | 76 | 59 | 55 | 47 | 1.91 | 20.5% | 5.3% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| task_signal_boost | -0.06 | +0 | +3 | +0 | +3 | +0.03 | +0 |
| strong_task_first | -0.04 | -1 | +0 | -1 | +1 | -0.01 | +0 |
| project_bonus_boost | -0.13 | +1 | +7 | +1 | +7 | +0.18 | -3 |
| durable_kind_boost | -0.05 | +1 | +2 | +1 | +3 | +0.04 | -1 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.83-3.60 |
| task_signal_boost | 0 | 0 | 2.75-3.50 |
| strong_task_first | 0 | 1 | 2.83-3.40 |
| project_bonus_boost | 1 | 0 | 2.75-3.35 |
| durable_kind_boost | 1 | 0 | 2.83-3.50 |

## Memory Lifecycle

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| suppress_stale_and_low | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| suppress_likely_low | 3.20 | 76 | 59 | 54 | 45 | 1.80 | 23.5% | 6.4% |
| suppress_vague | 3.22 | 75 | 56 | 54 | 43 | 1.86 | 22.5% | 6.4% |
| suppress_all_bad_modes | 3.21 | 76 | 58 | 54 | 44 | 1.80 | 23.5% | 6.4% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| suppress_stale_and_low | +0.00 | +0 | +0 | +0 | +0 | +0.00 | +0 |
| suppress_likely_low | -0.01 | +1 | +2 | +0 | +1 | -0.06 | +0 |
| suppress_vague | +0.01 | +0 | -1 | +0 | -1 | -0.00 | +0 |
| suppress_all_bad_modes | +0.01 | +1 | +1 | +0 | +0 | -0.06 | +0 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.83-3.60 |
| suppress_stale_and_low | 0 | 0 | 2.83-3.60 |
| suppress_likely_low | 1 | 1 | 2.93-3.47 |
| suppress_vague | 0 | 1 | 2.88-3.60 |
| suppress_all_bad_modes | 1 | 1 | 2.98-3.47 |

## Abstention Gate

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.21 | 75 | 57 | 54 | 44 | 1.86 | 22.5% | 6.4% |
| score_floor_0_95 | 3.21 | 75 | 57 | 54 | 44 | 1.81 | 23.5% | 6.4% |
| score_floor_1_05 | 3.35 | 75 | 45 | 54 | 37 | 1.56 | 26.0% | 7.4% |
| score_floor_1_15 | 3.44 | 67 | 39 | 52 | 32 | 1.30 | 32.5% | 12.8% |
| evidence_or_1_05 | 3.32 | 75 | 46 | 54 | 38 | 1.61 | 25.0% | 7.4% |

Deltas vs first strategy in family:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| score_floor_0_95 | +0.00 | +0 | +0 | +0 | +0 | -0.05 | +0 |
| score_floor_1_05 | +0.15 | +0 | -12 | +0 | -7 | -0.30 | +1 |
| score_floor_1_15 | +0.23 | -8 | -18 | -2 | -12 | -0.56 | +6 |
| evidence_or_1_05 | +0.12 | +0 | -11 | +0 | -6 | -0.25 | +1 |

Stability:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.83-3.60 |
| score_floor_0_95 | 0 | 0 | 2.83-3.60 |
| score_floor_1_05 | 0 | 5 | 2.98-3.73 |
| score_floor_1_15 | 0 | 5 | 2.98-4.00 |
| evidence_or_1_05 | 0 | 5 | 2.98-3.57 |

## Structured Artifacts

- Manifest: `experiments/recall/2026-06-22-candidate-5x5/manifest.json`
- Summary JSON: `experiments/recall/2026-06-22-candidate-5x5/summary.json`
- Per-anchor JSONL: `experiments/recall/2026-06-22-candidate-5x5/details.jsonl`
