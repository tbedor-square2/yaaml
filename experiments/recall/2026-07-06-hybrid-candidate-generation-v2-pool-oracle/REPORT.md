# Pool Recall Ceiling Oracle

Date: 2026-07-06
Anchors file: `/Users/tbedor/Development/yaaml/experiments/recall/anchor-libraries/2026-07-screening.tsv`
Recall source directory: `/Users/tbedor/Development/yaaml/target/recall-backtests/hybrid-candidate-generation-v2`

## Question

For anchors with a known-useful oracle memory, does production retrieval put any known-useful memory into the 16-candidate ranked pool?

## Results

- Anchors: 200
- Oracle-useful anchors: 102
- Known-useful in pool: 21 (20.6%)
- Known-useful selected: 4 (3.9%)
- Missing due to retrieval: 81
- Missing due to selection: 17

## Interpretation

Retrieval is the binding ceiling on this screening library: more known-useful misses are absent from the candidate pool than present-but-not-selected. Future recall-quality work should prioritize candidate generation before another selection/rerank variant.

## Artifacts

- `manifest.json` records inputs and summary metrics.
- `cases.jsonl` records per-anchor useful IDs, pool membership, selected membership, and miss class.
