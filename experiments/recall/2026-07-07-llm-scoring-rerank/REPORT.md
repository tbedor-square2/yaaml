# LLM Scoring Rerank

Date: 2026-07-07
Input directory: `target/recall-backtests/wide-multi-query-candidate-generation-v2`

## Experiment

Scored the top saved recall candidates with the aligned query-plus-memory rubric prompt, tuned a score threshold on training folds, and replayed held-out selection without rerunning retrieval.

## Results

| Metric | Production | LLM rerank | Delta | 95% CI | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `average_known_score` | 4.667 | 3.667 | -1.000 | [-4.000, 0.833] | needs larger sample |
| `useful_known_selected` | 3.000 | 2.000 | -1.000 | [-6.000, 3.000] | needs larger sample |
| `low_known_selected` | 0.000 | 1.000 | 1.000 | [0.000, 3.000] | no detectable effect |
| `useful_capture_runs` | 3.000 | 2.000 | -1.000 | [-6.000, 3.000] | needs larger sample |
| `low_selection_runs` | 0.000 | 1.000 | 1.000 | [0.000, 3.000] | no detectable effect |
| `average_selected_per_anchor` | 0.610 | 0.605 | -0.005 | [-0.130, 0.120] | no detectable effect |
| `empty_recall_runs` | 89.000 | 113.000 | 24.000 | [5.000, 42.000] | confirmed |
| `missed_useful_empty_runs` | 42.000 | 58.000 | 16.000 | [2.000, 31.000] | confirmed |
| `clean_abstention_runs` | 47.000 | 55.000 | 8.000 | [-3.000, 19.000] | needs larger sample |

## Threshold Diagnostics

| Threshold | Selected | Avg selected | Useful known | Low known | Useful runs | Empty | Missed-useful empty |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 390 | 1.950 | 3 | 1 | 3 | 5 | 2 |
| 2 | 386 | 1.930 | 3 | 1 | 3 | 6 | 2 |
| 3 | 265 | 1.325 | 3 | 1 | 3 | 48 | 22 |
| 4 | 259 | 1.295 | 3 | 1 | 3 | 49 | 23 |
| 5 | 121 | 0.605 | 2 | 1 | 2 | 113 | 58 |

## Scoring

- Model: `claude-haiku-4-5-20251001`
- Candidate pool: top 8 production-eligible candidates per anchor.
- Scored candidates: 1544
- Thresholds by fold: 5, 5, 5, 5, 5
- Selection limit: 2

## Decision

Do not ship this LLM rerank. Useful-known selection did not improve with a CI excluding zero.

## Artifacts

- `llm_scores.jsonl` records cached candidate scores.
- `production.details.jsonl` records saved production selections.
- `llm_rerank.details.jsonl` records held-out LLM-rerank selections.
- `manifest.json` records summaries and paired deltas.
