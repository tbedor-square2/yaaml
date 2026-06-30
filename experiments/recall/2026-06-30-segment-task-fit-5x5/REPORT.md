# Segment/Task-Fit Recall 5x5

Date: 2026-06-30
Input: `target/strict-kind-production`
Anchors: 200
Cohorts: 5

## Method

This run compares five segment/task-fit strategy variants against current production health-action selection across five deterministic cohorts.
It replays saved `yaaml recall --debug-ranking` outputs and saved eval oracle labels; no embedding or LLM provider calls are made.

Limitations:

1. These strategies operate over the saved candidate set, so they cannot measure memories a different segment query would newly retrieve.
2. Segment fit is proxied by existing `context_score`, `task_key_bonus`, matched task keys, memory kind, filter reasons, and eval-health labels.
3. Wrong-context and stale-task counts are derived from existing oracle rationales for low-scored selected memories.

## Synthesis

1. Best balanced candidate: `task_fit_required` with score 2.95, 54 useful runs, 75 low selections, 29 wrong-context lows, and 1.95 avg memories.
2. Strongest precision candidate: `segment_context_top2` with 50 low selections and 21 wrong-context lows, but 2 missed-useful empty recalls.
3. The main question for runtime work is whether segment evidence can reduce wrong-context lows without leaning on broad abstention.

## Results

| Strategy | Avg score | Useful selected | Low selected | Wrong-context lows | Stale-task lows | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production_health_action | 2.94 | 74 | 77 | 32 | 43 | 56 | 58 | 2.06 | 15.5% | 3.2% |
| segment_context_rerank | 2.80 | 63 | 75 | 37 | 36 | 52 | 62 | 2.46 | 7.5% | 2.1% |
| task_fit_required | 2.95 | 72 | 75 | 29 | 42 | 54 | 57 | 1.95 | 16.5% | 3.2% |
| wrong_context_penalty | 2.90 | 67 | 70 | 29 | 40 | 52 | 59 | 1.80 | 19.0% | 5.3% |
| segment_evidence_gate | 2.98 | 72 | 73 | 28 | 40 | 54 | 55 | 1.79 | 23.0% | 6.4% |
| segment_context_top2 | 2.96 | 52 | 50 | 21 | 26 | 45 | 47 | 1.78 | 7.5% | 2.1% |

## Deltas vs Production

| Strategy | Avg score | Useful selected | Low selected | Wrong-context lows | Stale-task lows | Useful runs | Low runs | Avg memories | Missed useful empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| segment_context_rerank | -0.14 | -11 | -2 | +5 | -7 | -4 | +4 | +0.39 | -1 |
| task_fit_required | +0.01 | -2 | -2 | -3 | -1 | -2 | -1 | -0.12 | +0 |
| wrong_context_penalty | -0.04 | -7 | -7 | -3 | -3 | -4 | +1 | -0.26 | +2 |
| segment_evidence_gate | +0.04 | -2 | -4 | -4 | -3 | -2 | -3 | -0.27 | +3 |
| segment_context_top2 | +0.02 | -22 | -27 | -11 | -17 | -11 | -11 | -0.28 | -1 |

## Stability

| Strategy | Useful-gain cohorts | Low-reduction cohorts | Wrong-context-reduction cohorts | Cohort avg score range |
| --- | ---: | ---: | ---: | ---: |
| production_health_action | n/a | n/a | n/a | 2.79-3.34 |
| segment_context_rerank | 0 | 2 | 0 | 2.50-3.18 |
| task_fit_required | 0 | 2 | 2 | 2.76-3.22 |
| wrong_context_penalty | 0 | 3 | 2 | 2.66-3.31 |
| segment_evidence_gate | 0 | 3 | 3 | 2.84-3.22 |
| segment_context_top2 | 0 | 5 | 5 | 2.75-3.26 |

## Findings

1. Segment/task-fit selection is mostly a precision lever over the existing candidate set; this replay does not test improved segment-aware candidate generation.
2. Strategies that require strong task/context evidence should be judged against missed-useful empties, not average score alone.
3. If no variant materially reduces wrong-context lows while preserving useful captures, the next experiment should change candidate generation with segment summaries rather than only rerank retrieved candidates.

## Structured Artifacts

1. Manifest: `experiments/recall/2026-06-30-segment-task-fit-5x5/manifest.json`
2. Summary JSON: `experiments/recall/2026-06-30-segment-task-fit-5x5/summary.json`
3. Per-anchor JSONL: `experiments/recall/2026-06-30-segment-task-fit-5x5/details.jsonl`
