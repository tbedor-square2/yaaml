# Retrieval Miss Diagnosis

Date: 2026-07-06
Cases: `/Users/tbedor/Development/yaaml/experiments/recall/2026-07-06-wide-multi-query-candidate-generation-v2-pool-oracle/cases.jsonl`
Recall directory: `/Users/tbedor/Development/yaaml/target/recall-backtests/wide-multi-query-candidate-generation-v2`

## Summary

- Retrieval-miss anchors: 80
- Useful memory instances: 104
- Active oracle-useful anchors: 28
- Active known-useful in pool: 22
- Active known-useful selected: 3
- Active misses due to retrieval: 6
- Active misses due to selection: 19

## Cause Counts

- `memory_inactive`: 98
- `missing_identity_key_in_query`: 5
- `query_noise_dominates`: 1

## Memory Kinds

- `project_fact`: 57
- `workflow`: 18
- `lesson`: 17
- `task_state`: 9
- `task_checkpoint`: 2
- `preference`: 1

## Interpretation

Most raw retrieval misses are currently inactive memories, which production recall intentionally excludes. On active oracle-useful anchors, selection is the larger observed gap: useful memories are usually present in the widened pool but not selected.

## Artifacts

- `manifest.json` records inputs and summary metrics.
- `cases.jsonl` records per-memory miss features and assigned cause.
