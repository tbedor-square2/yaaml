# Recall Techniques 5x5 Experiments

Date: 2026-06-23
Input: `target/strict-kind-production`
Anchors: 200
Cohorts: 5

## Method

Each category compares five strategies across five deterministic cohorts using saved `yaaml recall --debug-ranking` outputs and saved eval oracle labels.
No embedding or LLM provider calls are made during replay. Empty recall is reported as abstention, not as a low score.

Limitations:

1. Candidate-generation variants are bounded by the saved 16-candidate ranking for each anchor.
2. Query-signal and LLM-filter variants are deterministic proxies over already retrieved candidates.
3. Corpus-quality variants simulate suppression or boosting; they do not rewrite memory content.

## Synthesis

1. Candidate Generation: best balanced `pool_8` (score=3.24, useful_runs=53, low=53, avg_mem=1.79). Lowest lows `pool_8` (low=53, useful=74); lowest volume `pool_5` (avg_mem=1.70, useful_runs=53). Baseline `pool_3` score=3.16, useful_runs=45, low=47, avg_mem=1.43.
2. Hard Gating: best balanced `evidence_gate` (score=3.26, useful_runs=53, low=52, avg_mem=1.75). Lowest lows `evidence_gate` (low=52, useful=74); lowest volume `evidence_gate` (avg_mem=1.75, useful_runs=53). Baseline `production_health_action` score=3.24, useful_runs=53, low=53, avg_mem=1.85.
3. Reranking: best balanced `project_context_proxy` (score=3.18, useful_runs=55, low=53, avg_mem=2.08). Lowest lows `project_context_proxy` (low=53, useful=74); lowest volume `eval_ratio_rerank` (avg_mem=1.96, useful_runs=57). Baseline `production_health_action` score=3.24, useful_runs=53, low=53, avg_mem=1.85.
4. Selection Budgeting: best balanced `top_2` (score=3.40, useful_runs=53, low=41, avg_mem=1.42). Lowest lows `top_1` (low=22, useful=36); lowest volume `top_1` (avg_mem=0.77, useful_runs=36). Baseline `production_health_action` score=3.24, useful_runs=53, low=53, avg_mem=1.85.
5. LLM Filter Proxy: best balanced `abstain_if_weak_top` (score=3.28, useful_runs=53, low=49, avg_mem=1.79). Lowest lows `llm_one_best_proxy` (low=22, useful=36); lowest volume `llm_one_best_proxy` (avg_mem=0.74, useful_runs=36). Baseline `production_health_action` score=3.24, useful_runs=53, low=53, avg_mem=1.85.
6. Memory Corpus Quality: best balanced `suppress_vague` (score=3.24, useful_runs=53, low=53, avg_mem=1.85). Lowest lows `suppress_vague` (low=53, useful=74); lowest volume `suppress_likely_low` (avg_mem=1.80, useful_runs=53). Baseline `production_health_action` score=3.24, useful_runs=53, low=53, avg_mem=1.85.
7. Eval Feedback: best balanced `eval_ratio_rerank` (score=3.08, useful_runs=57, low=68, avg_mem=1.96). Lowest lows `precision_abstain` (low=42, useful=73); lowest volume `precision_abstain` (avg_mem=1.53, useful_runs=52). Baseline `strict_kind_no_eval` score=2.85, useful_runs=53, low=82, avg_mem=2.11.

## Directional Takeaways

1. The strongest context-bloat reducer is dynamic selection, especially `top_2`: it preserved useful-run coverage while reducing low selections and average memory count.
2. Moderate abstention/filtering helps when it removes weak candidates, but strict variants can inflate average score by missing useful recalls.
3. Project/context-heavy reranking improved useful coverage but did not reduce context volume in this replay; it is useful for relevance, not the primary bloat lever.
4. Broad hard gates are not clearly better than narrow evidence gates; most variants were neutral, and aggressive gates risk missed-useful abstentions.
5. Corpus-quality actions currently have small effects in replay, suggesting memory-health labels are useful as ranking inputs but not enough by themselves to solve context bloat.
6. Eval feedback is valuable: compared with no eval-informed scoring, failure-mode reranking sharply reduced low selections, while precision abstention reduced lows further at the cost of additional missed-useful empties.

## Recommended Next Runtime Experiment

1. Apply a default `top_2` dynamic selection cap after existing ranking and health rerank.
2. Add a moderate weak-top abstention gate, not a strict LLM-style filter, and track missed-useful empties separately.
3. Keep failure-mode reranking enabled as the eval-informed baseline.
4. Do not prioritize broad hard gates or corpus suppression until memory-health labels become more discriminating.
5. Re-run this report after adding session cooldown so context volume captures repeated-memory bloat as well as per-call bloat.

## Candidate Generation

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| pool_3 | 3.16 | 57 | 47 | 45 | 36 | 1.43 | 24.5% | 8.5% |
| pool_5 | 3.20 | 74 | 55 | 53 | 43 | 1.70 | 24.0% | 7.4% |
| pool_8 | 3.24 | 74 | 53 | 53 | 41 | 1.79 | 24.0% | 7.4% |
| pool_12 | 3.24 | 74 | 53 | 53 | 41 | 1.84 | 23.5% | 6.4% |
| pool_16 | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| pool_5 | +0.04 | +17 | +8 | +8 | +7 | +0.27 | -1 |
| pool_8 | +0.08 | +17 | +6 | +8 | +5 | +0.36 | -1 |
| pool_12 | +0.08 | +17 | +6 | +8 | +5 | +0.42 | -2 |
| pool_16 | +0.08 | +17 | +6 | +8 | +5 | +0.42 | -2 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| pool_3 | n/a | n/a | 2.94-3.43 |
| pool_5 | 5 | 0 | 2.95-3.60 |
| pool_8 | 5 | 1 | 2.93-3.60 |
| pool_12 | 5 | 1 | 2.93-3.60 |
| pool_16 | 5 | 1 | 2.93-3.60 |

## Hard Gating

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| task_state_requires_task | 3.20 | 74 | 55 | 53 | 43 | 1.80 | 23.5% | 6.4% |
| project_or_task_gate | 3.24 | 74 | 53 | 53 | 41 | 1.83 | 24.0% | 6.4% |
| evidence_gate | 3.26 | 74 | 52 | 53 | 40 | 1.75 | 25.0% | 7.4% |
| durable_or_context_gate | 3.24 | 74 | 53 | 53 | 41 | 1.84 | 24.0% | 6.4% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| task_state_requires_task | -0.04 | +0 | +2 | +0 | +2 | -0.05 | +0 |
| project_or_task_gate | +0.00 | +0 | +0 | +0 | +0 | -0.02 | +0 |
| evidence_gate | +0.02 | +0 | -1 | +0 | -1 | -0.10 | +1 |
| durable_or_context_gate | +0.00 | +0 | +0 | +0 | +0 | -0.01 | +0 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.93-3.60 |
| task_state_requires_task | 0 | 0 | 2.88-3.60 |
| project_or_task_gate | 0 | 0 | 2.93-3.60 |
| evidence_gate | 0 | 1 | 2.93-3.71 |
| durable_or_context_gate | 0 | 0 | 2.93-3.60 |

## Reranking

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| eval_ratio_rerank | 3.08 | 72 | 68 | 57 | 52 | 1.96 | 21.5% | 4.3% |
| context_heavy_proxy | 3.11 | 74 | 59 | 55 | 46 | 2.12 | 17.5% | 3.2% |
| task_key_heavy_proxy | 3.15 | 73 | 55 | 54 | 44 | 2.00 | 18.0% | 3.2% |
| project_context_proxy | 3.18 | 74 | 53 | 55 | 43 | 2.08 | 17.0% | 3.2% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| eval_ratio_rerank | -0.16 | -2 | +15 | +4 | +11 | +0.10 | -2 |
| context_heavy_proxy | -0.13 | +0 | +6 | +2 | +5 | +0.27 | -3 |
| task_key_heavy_proxy | -0.09 | -1 | +2 | +1 | +3 | +0.15 | -3 |
| project_context_proxy | -0.06 | +0 | +0 | +2 | +2 | +0.23 | -3 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.93-3.60 |
| eval_ratio_rerank | 2 | 0 | 2.79-3.27 |
| context_heavy_proxy | 2 | 0 | 2.83-3.41 |
| task_key_heavy_proxy | 1 | 0 | 2.86-3.44 |
| project_context_proxy | 2 | 2 | 2.94-3.53 |

## Selection Budgeting

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| top_1 | 3.27 | 36 | 22 | 36 | 22 | 0.77 | 23.5% | 6.4% |
| top_2 | 3.40 | 63 | 41 | 53 | 33 | 1.42 | 23.5% | 6.4% |
| within_0_15_of_top | 3.27 | 43 | 32 | 40 | 25 | 1.00 | 23.5% | 6.4% |
| stop_on_0_20_gap | 3.27 | 52 | 38 | 43 | 29 | 1.18 | 23.5% | 6.4% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| top_1 | +0.03 | -38 | -31 | -17 | -19 | -1.08 | +0 |
| top_2 | +0.15 | -11 | -12 | +0 | -8 | -0.43 | +0 |
| within_0_15_of_top | +0.03 | -31 | -21 | -13 | -16 | -0.85 | +0 |
| stop_on_0_20_gap | +0.03 | -22 | -15 | -10 | -12 | -0.68 | +0 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.93-3.60 |
| top_1 | 0 | 4 | 3.00-3.90 |
| top_2 | 0 | 4 | 3.06-3.79 |
| within_0_15_of_top | 0 | 4 | 3.00-3.64 |
| stop_on_0_20_gap | 0 | 4 | 2.98-3.62 |

## LLM Filter Proxy

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| abstain_if_weak_top | 3.28 | 74 | 49 | 53 | 39 | 1.79 | 27.0% | 8.5% |
| llm_conservative_proxy | 3.33 | 73 | 43 | 52 | 36 | 1.55 | 26.0% | 8.5% |
| llm_strict_proxy | 3.45 | 64 | 35 | 49 | 29 | 1.20 | 35.0% | 17.0% |
| llm_one_best_proxy | 3.27 | 36 | 22 | 36 | 22 | 0.74 | 26.0% | 7.4% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| abstain_if_weak_top | +0.04 | +0 | -4 | +0 | -2 | -0.06 | +2 |
| llm_conservative_proxy | +0.09 | -1 | -10 | -1 | -5 | -0.30 | +2 |
| llm_strict_proxy | +0.21 | -10 | -18 | -4 | -12 | -0.65 | +10 |
| llm_one_best_proxy | +0.03 | -38 | -31 | -17 | -19 | -1.11 | +1 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.93-3.60 |
| abstain_if_weak_top | 0 | 2 | 2.93-3.60 |
| llm_conservative_proxy | 0 | 5 | 3.03-3.68 |
| llm_strict_proxy | 0 | 5 | 3.08-4.00 |
| llm_one_best_proxy | 0 | 4 | 3.00-3.90 |

## Memory Corpus Quality

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| suppress_vague | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| suppress_likely_low | 3.24 | 75 | 56 | 53 | 42 | 1.80 | 24.5% | 6.4% |
| suppress_all_bad_modes | 3.24 | 75 | 56 | 53 | 42 | 1.80 | 24.5% | 6.4% |
| proven_useful_boost | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| suppress_vague | +0.00 | +0 | +0 | +0 | +0 | +0.00 | +0 |
| suppress_likely_low | -0.01 | +1 | +3 | +0 | +1 | -0.05 | +0 |
| suppress_all_bad_modes | -0.01 | +1 | +3 | +0 | +1 | -0.05 | +0 |
| proven_useful_boost | +0.00 | +0 | +0 | +0 | +0 | +0.00 | +0 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| production_health_action | n/a | n/a | 2.93-3.60 |
| suppress_vague | 0 | 0 | 2.93-3.60 |
| suppress_likely_low | 1 | 1 | 3.01-3.47 |
| suppress_all_bad_modes | 1 | 1 | 3.01-3.47 |
| proven_useful_boost | 0 | 0 | 2.93-3.60 |

## Eval Feedback

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| strict_kind_no_eval | 2.85 | 67 | 82 | 53 | 61 | 2.11 | 15.0% | 3.2% |
| eval_ratio_rerank | 3.08 | 72 | 68 | 57 | 52 | 1.96 | 21.5% | 4.3% |
| failure_mode_rerank | 3.24 | 74 | 53 | 53 | 41 | 1.85 | 23.5% | 6.4% |
| precision_abstain | 3.36 | 73 | 42 | 52 | 35 | 1.53 | 26.5% | 8.5% |
| proven_or_context_gate | 3.26 | 74 | 52 | 53 | 40 | 1.73 | 26.0% | 10.6% |

Deltas vs first strategy in category:

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| eval_ratio_rerank | +0.22 | +5 | -14 | +4 | -9 | -0.15 | +1 |
| failure_mode_rerank | +0.39 | +7 | -29 | +0 | -20 | -0.26 | +3 |
| precision_abstain | +0.51 | +6 | -40 | -1 | -26 | -0.57 | +5 |
| proven_or_context_gate | +0.40 | +7 | -30 | +0 | -21 | -0.38 | +7 |

Stability across five cohorts:

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| strict_kind_no_eval | n/a | n/a | 2.59-3.19 |
| eval_ratio_rerank | 4 | 4 | 2.79-3.27 |
| failure_mode_rerank | 3 | 4 | 2.93-3.60 |
| precision_abstain | 3 | 5 | 3.03-3.73 |
| proven_or_context_gate | 3 | 4 | 2.93-3.71 |

## Structured Artifacts

1. Manifest: `experiments/recall/2026-06-23-techniques-5x5/manifest.json`
2. Summary JSON: `experiments/recall/2026-06-23-techniques-5x5/summary.json`
3. Per-anchor JSONL: `experiments/recall/2026-06-23-techniques-5x5/details.jsonl`
