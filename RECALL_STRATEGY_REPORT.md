# Recall Strategy Backtest Report

Date: 2026-06-17
Base branch: `rust-impl` at `a2bd499`

## Method

I tested isolated strategy branches in git worktrees under `../yaaml-worktrees/*`.
Each branch used the same backtest harness:

```bash
scripts/backtest-recall-strategy.sh <repo> <strategy-label>
```

The harness replays exact `yaaml recall --session <id> --turn <ordinal> --json --debug-ranking`
for 16 historical eval anchors. It compares selected memory IDs to existing eval scores for those
anchors. Unknown selected memories are counted separately instead of guessed.

Dataset mix:

- Java/Risk Arbiter low and mixed recall cases.
- YAAML high and mixed recall cases.
- Forge/Signalsmith review cases.
- Tool/workflow mixed cases.

Important limitations:

- This is not a full judge rerun for newly selected unknown memories. It is a comparative backtest
  against known judged memories, so `unknown_selected_memories` should be treated as residual
  uncertainty.
- YAAML's live memory store continued to ingest during the experiment, so exact scores can shift
  slightly between runs. The relative strategy behavior was stable.

## Results

| Strategy | Selected | Avg per Anchor | Known Selected | Unknown | Avg Known Score | Useful Known | Low Known | Useful Runs Captured | Low Runs Selected |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deterministic-top5 | 80 | 5.00 | 56 | 24 | 2.68 | 18 | 37 | 12 / 14 | 12 |
| deterministic-top3 | 48 | 3.00 | 35 | 13 | 2.92 | 13 | 21 | 11 / 14 | 11 |
| project-task-filter | 78 | 4.88 | 53 | 25 | 2.63 | 17 | 35 | 11 / 14 | 12 |
| LLM top1 historical run | 16 | 1.00 | 13 | 3 | 3.00 | 6 | 7 | 6 / 14 | 7 |
| LLM top3 historical run | 48 | 3.00 | 36 | 12 | 2.81 | 13 | 22 | 11 / 14 | 12 |
| LLM score rerank | 16 | 1.00 | 13 | 3 | 3.00 | 6 | 7 | 6 / 14 | 7 |

## Findings

1. `deterministic-top3` is the best immediate tradeoff.
   It cuts context volume 40% versus top-5, improves average known score from 2.68 to 2.92, and
   preserves 11 of 14 useful-run opportunities.

2. The LLM filter is not currently reliable as a selector.
   Both binary filtering and score-reranking returned empty selections for every anchor. That means
   the remote model is not using the candidate list in a stable enough way for production recall
   gating.

3. The LLM top-1 historical run over-prunes.
   It gives the highest average known score but captures useful context in only 6 of 14 useful
   anchors. It often keeps the wrong deterministic top candidate while dropping a useful candidate
   lower in the top-5.

4. The stricter project/task filter did not improve fidelity.
   It removed little volume and slightly worsened average known score. The issue is not only
   cross-project leakage; same-project stale memories still outrank better task-fit memories.

5. Top-5 maximizes useful recall but bloats context with too much low-scoring material.
   It captures 12 of 14 useful opportunities but selects 37 known low-scoring memories. That matches
   the observed production problem: recall often contains something useful, but it also carries too
   much irrelevant context.

## Recommendation

Default to deterministic top-3 recall now:

- `recall_result_limit = 3`
- `recall_llm_filter_enabled = false`

Keep the LLM filter implementation available behind config, but do not enable it by default until
it is evaluated with a prompt or API shape that can consistently score candidates instead of falling
back.

After applying the recommendation to `rust-impl`, a final backtest selected 48 memories across 16
anchors, captured useful known memories in 11 of 14 useful anchors, selected no empty recalls, and
made zero LLM calls.

Next promising direction: improve deterministic ranking before selection by adding task-fit scoring
features, especially exact path/PR/branch/target matches and penalties for stale same-project
memories without task overlap. The backtest shows that simply filtering after ranking is too late
when the deterministic top candidate is already wrong.

## Artifacts

Generated summaries are under:

```text
target/recall-strategy-results/*.summary.json
target/recall-strategy-results/*.details.jsonl
```

Strategy worktrees:

```text
../yaaml-worktrees/deterministic-top5   e0fb0b2
../yaaml-worktrees/deterministic-top3   d6ed05c
../yaaml-worktrees/llm-fallback-top3    adfe952
../yaaml-worktrees/project-task-filter  e667193
../yaaml-worktrees/llm-score-rerank     c8ffe0d
```
