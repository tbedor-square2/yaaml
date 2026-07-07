# Learned Weight Calibration

Date: 2026-07-06
Dataset: `target/recall-training-datasets/wide-v2-candidates.jsonl`

## Experiment

Trained a logistic feature model on labeled saved candidates, calibrated training-fold predictions with isotonic regression, tuned a probability threshold on training-fold utility, and replayed selection on held-out folds.

## Results

| Metric | Production | Feature model | Delta | 95% CI | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `average_known_score` | 4.667 | 4.750 | 0.083 | [0.000, 0.500] | needs larger sample |
| `useful_known_selected` | 3.000 | 4.000 | 1.000 | [0.000, 3.000] | no detectable effect |
| `low_known_selected` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `useful_capture_runs` | 3.000 | 4.000 | 1.000 | [0.000, 3.000] | no detectable effect |
| `low_selection_runs` | 0.000 | 0.000 | 0.000 | [0.000, 0.000] | no detectable effect |
| `average_selected_per_anchor` | 0.626 | 2.000 | 1.374 | [1.292, 1.456] | confirmed |
| `empty_recall_runs` | 84.000 | 0.000 | -84.000 | [-98.000, -71.000] | confirmed |
| `missed_useful_empty_runs` | 11.000 | 0.000 | -11.000 | [-18.000, -5.000] | confirmed |
| `clean_abstention_runs` | 73.000 | 0.000 | -73.000 | [-87.000, -61.000] | confirmed |

## Model

- Rows: 12480
- Labeled rows: 46
- Folds: 5
- Thresholds by fold: 0.1, 0.1, 0.1, 0.1, 0.1
- Selection limit: 2

## Decision

Do not ship this learned selector. Useful-known selection did not improve with a CI excluding zero.

## Artifacts

- `manifest.json` records model configuration, summaries, and paired deltas.
- `production.details.jsonl` records the saved production selections.
- `feature_model.details.jsonl` records held-out feature-model selections.
