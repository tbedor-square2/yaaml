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

## Interpretation

Retrieval is the binding ceiling on this screening library: more known-useful misses are absent from the candidate pool than present-but-not-selected. Future recall-quality work should prioritize candidate generation before another selection/rerank variant.

## Artifacts

- `manifest.json` records inputs and summary metrics.
- `cases.jsonl` records per-anchor useful IDs, pool membership, selected membership, and miss class.

## Addendum (2026-07-07)

The interpretation above was overturned by the retrieval-miss diagnosis
(`experiments/recall/2026-07-06-retrieval-miss-diagnosis/REPORT.md`): 98 of
104 raw retrieval-miss instances refer to memories that are now inactive and
intentionally excluded from production recall. On currently active useful
memories, selection is the larger gap (see the active-only results in
`experiments/recall/2026-07-06-production-active-pool-oracle/REPORT.md`).
Do not use this report's raw-miss split to prioritize candidate-generation
work.
