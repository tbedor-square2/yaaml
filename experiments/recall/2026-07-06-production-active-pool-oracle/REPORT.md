# Pool Recall Ceiling Oracle

Date: 2026-07-06
Anchors file: `/Users/tbedor/Development/yaaml/experiments/recall/anchor-libraries/2026-07-screening.tsv`
Recall source directory: `/Users/tbedor/Development/yaaml/target/anchor-refresh-2026-07`

## Question

For anchors with a known-useful oracle memory, does production retrieval put any known-useful memory into the 16-candidate ranked pool?

## Results

- Anchors: 200
- Oracle-useful anchors: 102
- Known-useful in pool: 18 (17.6%)
- Known-useful selected: 3 (2.9%)
- Missing due to retrieval: 84
- Missing due to selection: 15

## Active-Only Results

These exclude oracle-useful memories that are no longer active in the current memory corpus.

- Active oracle-useful anchors: 28
- Active known-useful in pool: 18 (64.3%)
- Active known-useful selected: 3 (10.7%)
- Active missing due to retrieval: 10
- Active missing due to selection: 15

## Interpretation

Selection is the larger observed gap on this screening library for active useful memories: more known-useful misses are in the candidate pool but not selected. Future recall-quality work should prioritize selection/reranking before widening candidate generation.

## Artifacts

- `manifest.json` records inputs and summary metrics.
- `cases.jsonl` records per-anchor useful IDs, pool membership, selected membership, and miss class.
