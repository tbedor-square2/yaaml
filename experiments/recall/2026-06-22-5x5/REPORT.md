# Recall 5x5 Experiment

Date: 2026-06-22
Input: `target/strict-kind-production`
Anchors: 200

## Method

This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.
No embedding or LLM provider calls are made during replay.

Scoring metrics only use memories that were actually selected and judged. Empty responses are tracked separately as abstentions, not as low-quality recall.

## Score Results

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.85 | 69 | 88 | 54 | 62 | 2.28 |
| context_fit_gate | 2.84 | 65 | 82 | 51 | 61 | 2.02 |
| eval_health_suppress | 2.98 | 69 | 69 | 54 | 52 | 1.89 |
| eval_health_rerank | 3.03 | 73 | 68 | 55 | 53 | 1.97 |
| context_score_rerank | 2.92 | 59 | 64 | 48 | 51 | 2.07 |

## Score Deltas vs Baseline

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| context_fit_gate | -0.01 | -4 | -6 | -3 | -1 | -0.26 |
| eval_health_suppress | +0.13 | +0 | -19 | +0 | -10 | -0.39 |
| eval_health_rerank | +0.18 | +4 | -20 | +1 | -9 | -0.31 |
| context_score_rerank | +0.08 | -10 | -24 | -6 | -11 | -0.21 |

## Abstention Metrics

| Strategy | Empty rate | Empty runs | Clean abstain | Missed-useful empty | Missed-useful rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.5% | 5 | 4 | 1 | 1.1% |
| context_fit_gate | 20.5% | 41 | 37 | 4 | 4.3% |
| eval_health_suppress | 17.0% | 34 | 31 | 3 | 3.2% |
| eval_health_rerank | 20.5% | 41 | 37 | 4 | 4.3% |
| context_score_rerank | 19.5% | 39 | 35 | 4 | 4.3% |

## Readout

- `eval_health_suppress` tests whether clearly bad memories should be removed before recall without changing ranking.
- `context_fit_gate` and `context_score_rerank` test whether wrong-context recall is better handled by stricter context fit or by reranking.
- `eval_health_rerank` tests whether aggregate eval history is useful; it is intentionally not context-sensitive, so regressions indicate global memory health is too blunt.
- Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.
- Empty recall is treated as abstention. Clean abstentions are good; missed-useful abstentions are the failure mode to reduce.

## Recommendation

`eval_health_rerank` is the best precision signal in this run. Its extra empty responses are tracked as abstentions, not score penalties; the relevant coverage regression is missed-useful abstention, where the oracle had a known useful memory and recall returned nothing.

`eval_health_suppress` is the safer production candidate: it keeps useful recall count flat while removing 19 known low-scoring selections and 10 low-selection runs. It should be paired with missed-useful-abstention monitoring before enabling by default.

`context_fit_gate` and `context_score_rerank` reduced low selections by dropping too much useful context. This suggests the current context metadata is useful as a secondary feature, but too lossy as a hard gate.

## Structured Artifacts

- Manifest: `experiments/recall/2026-06-22-5x5/manifest.json`
- Summary JSON: `experiments/recall/2026-06-22-5x5/summary.json`
- Per-anchor JSONL: `experiments/recall/2026-06-22-5x5/details.jsonl`
