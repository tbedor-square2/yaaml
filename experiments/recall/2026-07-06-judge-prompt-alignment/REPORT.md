# Judge Prompt Alignment

Date: 2026-07-06
Labels file: `experiments/recall/judge-calibration/2026-07-labels.jsonl`
Details file: `experiments/recall/2026-07-06-judge-prompt-alignment/details.jsonl`
Provider/model: `anthropic:claude-haiku-4-5-20251001`

## Results

| Variant | Rows | Exact | Within 1 | Useful Binary | Low Binary | Kappa Useful | MAE | Bias |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| query_memory_rubric | 100 | 0.820 | 0.960 | 0.950 | 0.960 | 0.865 | 0.260 | 0.020 |
| strict_recall_decision | 100 | 0.600 | 0.920 | 0.920 | 0.920 | 0.769 | 0.510 | -0.390 |
| production_eval_candidate | 100 | 0.390 | 0.630 | 0.610 | 0.620 | 0.164 | 1.180 | 0.620 |
| production_existing | 100 | 0.340 | 0.580 | 0.580 | 0.580 | 0.206 | 1.420 | 0.940 |

## Prompt Variants

- `production_existing`: stored production judge scores from the calibration label file.
- `production_eval_candidate`: current offline eval prompt shape replayed against query and memory.
- `query_memory_rubric`: query-plus-memory prompt aligned to the calibration rubric.
- `strict_recall_decision`: stricter inclusion-value prompt that penalizes topical-but-not-actionable recall.

## Interpretation

Best variant by useful-binary, within-1, then exact agreement: `query_memory_rubric`.
These scores compare prompt variants against LLM adjudicator labels, not human ground truth.
