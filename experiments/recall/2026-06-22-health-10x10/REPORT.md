# Health-Aware Recall 10x10 Experiment

Date: 2026-06-22
Input: `target/strict-kind-production`
Anchors: 200
Cohorts: 10

## Method

This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.
No embedding or LLM provider calls are made during replay.

The exercise compares ten health-aware selection approaches over ten deterministic cohorts. Scoring metrics only use memories that were actually selected and judged. Empty responses are tracked separately as abstentions, not as low-quality recall.

## Score Results

| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.85 | 69 | 88 | 54 | 62 | 2.28 |
| eval_health_rerank | 3.03 | 74 | 69 | 56 | 54 | 1.97 |
| wrong_context_gate | 2.86 | 67 | 81 | 53 | 60 | 2.10 |
| wrong_context_rerank | 2.86 | 67 | 81 | 53 | 60 | 2.10 |
| stale_low_filter | 2.80 | 68 | 85 | 54 | 66 | 2.10 |
| noisy_metadata_rekey | 2.87 | 68 | 81 | 53 | 60 | 2.11 |
| context_sensitive_gate | 2.91 | 61 | 73 | 49 | 56 | 1.94 |
| health_action_filter | 2.81 | 61 | 77 | 49 | 62 | 2.00 |
| health_action_rerank | 3.20 | 75 | 57 | 54 | 44 | 1.88 |
| health_precision_abstain | 3.35 | 75 | 45 | 54 | 37 | 1.60 |

## Score Deltas vs Baseline

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| eval_health_rerank | +0.18 | +5 | -19 | +2 | -8 | -0.31 |
| wrong_context_gate | +0.02 | -2 | -7 | -1 | -2 | -0.18 |
| wrong_context_rerank | +0.02 | -2 | -7 | -1 | -2 | -0.18 |
| stale_low_filter | -0.04 | -1 | -3 | +0 | +4 | -0.18 |
| noisy_metadata_rekey | +0.03 | -1 | -7 | -1 | -2 | -0.17 |
| context_sensitive_gate | +0.06 | -8 | -15 | -5 | -6 | -0.34 |
| health_action_filter | -0.04 | -8 | -11 | -5 | +0 | -0.28 |
| health_action_rerank | +0.36 | +6 | -31 | +0 | -18 | -0.40 |
| health_precision_abstain | +0.50 | +6 | -43 | +0 | -25 | -0.67 |

## Deltas vs Eval-Health Rerank

| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Missed-useful empty | Avg memories |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wrong_context_gate | -0.16 | -7 | +12 | -3 | +6 | -1 | +0.13 |
| wrong_context_rerank | -0.16 | -7 | +12 | -3 | +6 | -1 | +0.13 |
| stale_low_filter | -0.22 | -6 | +16 | -2 | +12 | -1 | +0.13 |
| noisy_metadata_rekey | -0.15 | -6 | +12 | -3 | +6 | -1 | +0.14 |
| context_sensitive_gate | -0.12 | -13 | +4 | -7 | +2 | +2 | -0.03 |
| health_action_filter | -0.22 | -13 | +8 | -7 | +8 | +0 | +0.03 |
| health_action_rerank | +0.18 | +1 | -12 | -2 | -10 | +2 | -0.09 |
| health_precision_abstain | +0.32 | +1 | -24 | -2 | -17 | +3 | -0.36 |

## Stability

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: |
| baseline_strict_kind | n/a | n/a | 2.50-3.56 |
| eval_health_rerank | 4 | 6 | 2.42-3.62 |
| wrong_context_gate | 1 | 5 | 2.17-3.47 |
| wrong_context_rerank | 1 | 5 | 2.17-3.47 |
| stale_low_filter | 1 | 4 | 2.17-3.38 |
| noisy_metadata_rekey | 2 | 6 | 2.17-3.72 |
| context_sensitive_gate | 0 | 6 | 2.17-3.56 |
| health_action_filter | 0 | 6 | 2.17-3.38 |
| health_action_rerank | 7 | 8 | 2.80-3.94 |
| health_precision_abstain | 7 | 10 | 2.94-3.94 |

## Abstention Metrics

| Strategy | Empty rate | Empty runs | Clean abstain | Missed-useful empty | Missed-useful rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline_strict_kind | 2.5% | 5 | 4 | 1 | 1.1% |
| eval_health_rerank | 21.0% | 42 | 38 | 4 | 4.3% |
| wrong_context_gate | 15.5% | 31 | 28 | 3 | 3.2% |
| wrong_context_rerank | 15.5% | 31 | 28 | 3 | 3.2% |
| stale_low_filter | 15.0% | 30 | 27 | 3 | 3.2% |
| noisy_metadata_rekey | 15.0% | 30 | 27 | 3 | 3.2% |
| context_sensitive_gate | 22.0% | 44 | 38 | 6 | 6.4% |
| health_action_filter | 16.5% | 33 | 29 | 4 | 4.3% |
| health_action_rerank | 22.5% | 45 | 39 | 6 | 6.4% |
| health_precision_abstain | 25.5% | 51 | 44 | 7 | 7.4% |

## Readout

- `eval_health_rerank` is the generic eval-history reranker from the prior 10x10.
- `wrong_context_gate` and `wrong_context_rerank` restrict memories diagnosed as valid-but-wrong-context unless context/task fit is strong.
- `stale_low_filter`, `noisy_metadata_rekey`, and `context_sensitive_gate` test targeted handling for specific failure modes.
- `health_action_filter`, `health_action_rerank`, and `health_precision_abstain` combine the diagnosed recommended actions.
- Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.
- Empty recall is treated as abstention. Clean abstentions are good; missed-useful abstentions are the failure mode to reduce.

## Findings

`eval_health_rerank` is retained as the generic eval-history baseline from the previous experiment.

`health_action_rerank` is the best balanced failure-mode-aware strategy in this run: compared with `eval_health_rerank`, it keeps useful selected memories roughly flat, cuts low selected memories materially, and does not collapse into pure abstention.

`health_precision_abstain` is the strongest precision mode, but it increases missed-useful abstentions. It is useful evidence for an abstaining mode, not a default recall policy.

Single-mode policies like `wrong_context_gate`, `stale_low_filter`, and `context_sensitive_gate` are weaker than the combined reranker. The failure modes interact; handling only one class of bad recall leaves too much noise or drops too much useful recall.

## Structured Artifacts

- Manifest: `experiments/recall/2026-06-22-health-10x10/manifest.json`
- Summary JSON: `experiments/recall/2026-06-22-health-10x10/summary.json`
- Per-anchor JSONL: `experiments/recall/2026-06-22-health-10x10/details.jsonl`
