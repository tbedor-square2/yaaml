# Recall 10x10 Experiment

Date: 2026-06-22
Input: `target/strict-kind-production`
Anchors: 200
Cohorts: 10

## Method

This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.
No embedding or LLM provider calls are made during replay.

The exercise compares ten selection approaches over ten deterministic cohorts. Scoring metrics only use memories that were actually selected and judged. Empty responses are tracked separately as abstentions, not as low-quality recall.

## Score Results

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.85 | 69 | 88 | 54 | 62 | 2.28 |
| score_top3 | 2.75 | 65 | 96 | 51 | 66 | 2.25 |
| vector_top3 | 2.56 | 38 | 66 | 33 | 52 | 3.00 |
| context_top3 | 2.63 | 53 | 99 | 47 | 71 | 2.37 |
| task_key_first | 2.80 | 67 | 86 | 53 | 64 | 2.25 |
| context_fit_gate | 2.84 | 65 | 82 | 51 | 61 | 2.02 |
| context_score_rerank | 2.92 | 59 | 64 | 48 | 51 | 2.07 |
| eval_health_suppress | 2.98 | 69 | 69 | 54 | 52 | 1.89 |
| eval_health_rerank | 3.03 | 73 | 68 | 55 | 53 | 1.98 |
| eval_context_combo | 3.38 | 67 | 41 | 49 | 30 | 1.90 |

## Score Deltas vs Baseline

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| score_top3 | -0.09 | -4 | +8 | -3 | +4 | -0.03 |
| vector_top3 | -0.29 | -31 | -22 | -21 | -10 | +0.72 |
| context_top3 | -0.22 | -16 | +11 | -7 | +9 | +0.09 |
| task_key_first | -0.05 | -2 | -2 | -1 | +2 | -0.03 |
| context_fit_gate | -0.01 | -4 | -6 | -3 | -1 | -0.26 |
| context_score_rerank | +0.08 | -10 | -24 | -6 | -11 | -0.21 |
| eval_health_suppress | +0.13 | +0 | -19 | +0 | -10 | -0.39 |
| eval_health_rerank | +0.18 | +4 | -20 | +1 | -9 | -0.30 |
| eval_context_combo | +0.54 | -2 | -47 | -5 | -32 | -0.38 |

## Stability

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| baseline_strict_kind | n/a | n/a | 2.50-3.56 |
| score_top3 | 0 | 1 | 2.20-3.48 |
| vector_top3 | 1 | 8 | 1.69-3.48 |
| context_top3 | 1 | 1 | 2.02-3.60 |
| task_key_first | 1 | 4 | 2.17-3.47 |
| context_fit_gate | 0 | 5 | 2.17-3.38 |
| context_score_rerank | 2 | 8 | 2.39-3.86 |
| eval_health_suppress | 0 | 8 | 2.63-3.56 |
| eval_health_rerank | 4 | 6 | 2.42-3.62 |
| eval_context_combo | 3 | 10 | 2.43-4.00 |

## Abstention Metrics

| Strategy | Empty rate | Empty runs | Clean abstain | Missed-useful empty | Missed-useful rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.5% | 5 | 4 | 1 | 1.1% |
| score_top3 | 15.0% | 30 | 27 | 3 | 3.2% |
| vector_top3 | 0.0% | 0 | 0 | 0 | 0.0% |
| context_top3 | 15.0% | 30 | 27 | 3 | 3.2% |
| task_key_first | 6.0% | 12 | 11 | 1 | 1.1% |
| context_fit_gate | 20.5% | 41 | 37 | 4 | 4.3% |
| context_score_rerank | 19.5% | 39 | 35 | 4 | 4.3% |
| eval_health_suppress | 17.0% | 34 | 31 | 3 | 3.2% |
| eval_health_rerank | 20.5% | 41 | 37 | 4 | 4.3% |
| eval_context_combo | 26.0% | 52 | 46 | 6 | 6.4% |

## Readout

- `score_top3`, `vector_top3`, and `context_top3` isolate the current ranking inputs.
- `task_key_first`, `context_fit_gate`, and `context_score_rerank` test project/task context as stronger selection signals.
- `eval_health_suppress`, `eval_health_rerank`, and `eval_context_combo` test whether historical eval outcomes should influence recall.
- Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.
- Empty recall is treated as abstention. Clean abstentions are good; missed-useful abstentions are the failure mode to reduce.

## Findings

`eval_health_rerank` is the best balanced candidate in this replay: it raises the average known score and useful selections while reducing low selections.

`eval_health_suppress` is the safer version of that idea when the priority is reducing known-bad memories without aggressively changing candidate order.

Pure vector, pure context, and context-heavy gates are useful diagnostics, but they tend to trade away too much useful recall. Context should stay a secondary signal until the project/task metadata is sharper.

`eval_context_combo` is the highest-precision abstaining strategy: it sharply reduces known-low selections, but it also drops useful captures and increases missed-useful abstentions. That shape is useful for diagnostics, not a default recall policy.

## Structured Artifacts

- Manifest: `experiments/recall/2026-06-22-10x10/manifest.json`
- Summary JSON: `experiments/recall/2026-06-22-10x10/summary.json`
- Per-anchor JSONL: `experiments/recall/2026-06-22-10x10/details.jsonl`
