# Recall 5x5 Experiment

Date: 2026-06-22
Input: `target/strict-kind-production`
Anchors: 200

## Method

This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.
No embedding or LLM provider calls are made during replay.

Five strategies are compared against five primary metrics: average known score, useful known selected, low known selected, useful capture runs, and empty recall runs.

## Results

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Empty runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.85 | 69 | 88 | 54 | 5 | 2.28 |
| context_fit_gate | 2.84 | 65 | 82 | 51 | 41 | 2.02 |
| eval_health_suppress | 2.98 | 69 | 69 | 54 | 34 | 1.89 |
| eval_health_rerank | 3.03 | 73 | 68 | 55 | 41 | 1.97 |
| context_score_rerank | 2.92 | 59 | 64 | 48 | 39 | 2.07 |

## Deltas vs Baseline

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Empty runs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| context_fit_gate | -0.01 | -4 | -6 | -3 | -1 | +36 |
| eval_health_suppress | +0.13 | +0 | -19 | +0 | -10 | +29 |
| eval_health_rerank | +0.18 | +4 | -20 | +1 | -9 | +36 |
| context_score_rerank | +0.08 | -10 | -24 | -6 | -11 | +34 |

## Readout

- `eval_health_suppress` tests whether clearly bad memories should be removed before recall without changing ranking.
- `context_fit_gate` and `context_score_rerank` test whether wrong-context recall is better handled by stricter context fit or by reranking.
- `eval_health_rerank` tests whether aggregate eval history is useful; it is intentionally not context-sensitive, so regressions indicate global memory health is too blunt.
- Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.
- Empty recall is counted as a first-class outcome because lower context cost is only useful when recall is not missing helpful context.

## Recommendation

`eval_health_rerank` is the best precision signal in this run, but it is too willing to abstain. The next production experiment should keep the eval-health score adjustment and tune the abstention threshold explicitly against recall-rate metrics, rather than silently filling empty results.

`eval_health_suppress` is the safer production candidate: it keeps useful recall count flat while removing 19 known low-scoring selections and 10 low-selection runs. It also increases empty runs, so it should be paired with recall-rate monitoring before enabling by default.

`context_fit_gate` and `context_score_rerank` reduced low selections by dropping too much useful context. This suggests the current context metadata is useful as a secondary feature, but too lossy as a hard gate.

## Structured Artifacts

- Manifest: `experiments/recall/2026-06-22-5x5/manifest.json`
- Summary JSON: `experiments/recall/2026-06-22-5x5/summary.json`
- Per-anchor JSONL: `experiments/recall/2026-06-22-5x5/details.jsonl`
