# YAAML Memory Quality Evidence

Date: 2026-07-02

## Summary

YAAML has strong evidence that recall selection can be improved with small
budgets, health-aware reranking, abstention, cooldown, and segment/task-fit
signals. The memory-write side has less isolated ablation evidence, but the
local corpus and eval store now provide enough signal to make memory quality a
measurable engineering target.

The important point is not "memory quality is solved." It is the opposite:
YAAML now has the instrumentation to show that active memories still produce
many low-scored recalls, while also showing which metadata and lifecycle
mechanisms are available to fix that.

## Corpus Evidence

Local YAAML database snapshot:

| Metric | Count |
| --- | ---: |
| Total memories | 3,369 |
| Active memories | 884 |
| Memories with source-turn refs | 3,363 |
| Memories with task keys | 1,727 |
| Memories with origin segment metadata | 1,436 |
| Segment-scoped memories | 106 |
| Memories with consolidation lineage refs | 362 |
| Completed memory formulation tasks | 1,409 |
| Completed memory consolidation tasks | 574 |
| Conversation segments | 2,176 |
| Segment labels | 60 |

Segment status distribution:

| Status | Segments | Avg task keys |
| --- | ---: | ---: |
| superseded | 1,893 | 3.5 |
| abandoned | 281 | 4.2 |
| active | 2 | 2.0 |

This supports two claims:

1. YAAML is not just storing text. It has provenance, task-key, segment,
   validity, and lineage data for a large local corpus.
2. Conversation segmentation is doing real lifecycle work: the corpus has many
   superseded/abandoned historical segments and only two active segments.

## Eval-Joined Corpus Quality

The local eval store has 5,989 judged memory results that join back to memories.

By memory kind:

| Kind | Judged | Avg score | Good | Low | Good rate | Low rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| workflow | 2,135 | 3.03 | 958 | 1,169 | 44.9% | 54.8% |
| lesson | 1,856 | 2.56 | 593 | 1,244 | 32.0% | 67.0% |
| project_fact | 1,648 | 2.88 | 692 | 944 | 42.0% | 57.3% |
| task_state | 178 | 2.87 | 71 | 105 | 39.9% | 59.0% |
| preference | 140 | 2.48 | 40 | 91 | 28.6% | 65.0% |
| task_checkpoint | 32 | 3.56 | 22 | 9 | 68.8% | 28.1% |

By active state:

| State | Judged | Memories | Avg score | Good | Low | Good rate | Low rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| active | 1,929 | 307 | 2.57 | 625 | 1,282 | 32.4% | 66.5% |
| inactive | 4,060 | 675 | 2.95 | 1,751 | 2,280 | 43.1% | 56.2% |

By active-memory metadata slice:

| Slice | Memories | Avg score | Good | Low |
| --- | ---: | ---: | ---: | ---: |
| active with lineage refs | 28 | 2.72 | 88 | 140 |
| active without lineage refs | 279 | 2.36 | 537 | 1,142 |
| active with origin segment | 305 | 2.40 | 625 | 1,277 |
| active without origin segment | 2 | 1.00 | 0 | 5 |
| active with task keys | 299 | 2.40 | 592 | 1,266 |
| active without task keys | 8 | 2.09 | 33 | 16 |

This evidence is directional, not causal. Memories were not randomly assigned
to metadata treatments. But it is still useful:

- Task checkpoints currently look much better than other kinds.
- Lessons, preferences, project facts, and workflows are the main low-score
  target areas.
- Source refs are nearly universal, which means provenance is available for
  debugging, rewrite, and suppression.
- Lineage-bearing active memories have a higher average per-memory score than
  active memories without lineage, suggesting consolidation may be useful enough
  to evaluate more directly.
- Active memories still have many low-scored historical recalls, so memory
  quality needs more than activation/deactivation.

## Existing Mechanisms

YAAML already has the core mechanisms needed for memory-quality work:

- typed memory kinds,
- source-turn refs,
- task keys,
- origin segment metadata,
- `valid_while_segment_active` validity,
- active/superseded/abandoned segment lifecycle,
- duplicate deactivation,
- consolidation clusters,
- consolidation lineage refs,
- eval-derived memory health,
- memory-level eval summaries.

Relevant tests cover:

- segment splitting on task-key and context shifts,
- segment task-key priority and bounds,
- idle segment abandonment,
- task-state segment scoping and expiry,
- formulation refinement of existing candidate memories,
- duplicate deactivation while preserving related distinct memories,
- consolidation scheduling and lineage preservation,
- recall eval scoring per memory,
- empty-recall abstention classification,
- memory-level mixed-score reporting.

## What The Evidence Does Not Yet Prove

The current evidence does not prove that any individual memory-write technique
caused better recall. The recall experiments are stronger because they compare
selectors on the same replay anchors. Memory-write quality needs the same rigor.

Missing evals:

- formulation with vs. without refinement candidates,
- segment-scoped task state vs. unscoped task state,
- consolidation dry-run vs. unconsolidated corpus,
- memory kind classification quality,
- source-window size effects,
- rewrite vs. suppress for consistently low active memories,
- segment labels used during candidate generation.

## Recommended Next Work

1. Add a memory-formulation eval harness.

   Replay historical formulation windows and judge the memories generated from
   those windows. Score kind, durability, specificity, source faithfulness, and
   future recall usefulness.

2. Add a consolidation dry-run eval.

   For each consolidation cluster, generate the consolidated candidate, replay
   recall with the cluster replaced by the candidate, and compare useful runs,
   low selections, and missed-useful abstentions before writing.

3. Add segment-aware candidate generation.

   The existing segment/task-fit recall experiment showed that segment evidence
   is a precision lever, but it only reranked the existing candidate set. The
   next step is using segment summaries and labels earlier in retrieval.

4. Turn memory health into actions.

   Use eval-derived failure modes to decide whether to rewrite, consolidate,
   expire, demote, or preserve memories. The current active corpus has enough
   low-scored memories to justify this as targeted cleanup rather than
   speculative tuning.

5. Track memory-write experiments like recall experiments.

   Every memory-quality change should report useful coverage, low recall,
   missed-useful abstention, and corpus effects. This keeps memory work in the
   same empirical loop that already improved recall.

## Bottom Line

YAAML has enough evidence to justify more memory-quality work. The corpus has
rich provenance and lifecycle metadata, the eval store can identify low-value
active memories, and the implementation already has hooks for segmentation,
consolidation, lineage, and memory health. The next step is to add isolated
write-quality evals so formulation and consolidation can improve with the same
discipline as recall selection.
