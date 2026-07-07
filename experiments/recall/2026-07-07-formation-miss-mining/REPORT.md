# Formation-Miss Mining

Date: 2026-07-07
Database: `/Users/tbedor/.yaaml/yaaml.db`

## Question

Where do repeated user corrections appear across sessions without an active memory that appears to cover the correction?

## Results

- Sessions scanned: 381
- Transcript files read: 380
- Correction-like user turns: 175
- Repeated correction clusters: 11
- Clusters with apparent active-memory coverage: 7
- Missed-formation clusters: 4
- Missed correction turns: 36

## Top Missed Clusters

- `java|path:github.com/squareup/java/pull/480115/changes`: 13 turns across 13 sessions; reasons=do_not
- `java|flag-rollout-wondering`: 13 turns across 13 sessions; reasons=instead, should_have
- `java|path:github.com/squareup/java/pull/481756`: 5 turns across 5 sessions; reasons=instead
- `java|partition-creates-right`: 5 turns across 5 sessions; reasons=instead

## Interpretation

Formation misses are observable with transcript-only mining. Use the missed clusters as seed cases for a formation-time activation condition prompt, but do not treat this heuristic diagnostic as a ship/no-ship evaluation of the write policy.

## Artifacts

- `manifest.json` records inputs and heuristic thresholds.
- `cases.jsonl` records repeated correction clusters, examples, and coverage classification.
