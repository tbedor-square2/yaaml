# Recall Improvement Report

Date: 2026-06-19
Branch: `rust-impl`

## Changes Made

This pass implemented the first concrete recall-quality improvements from the
post-5x5 plan:

- Strip noisy Codex/environment scaffolding from recall query text before
  embedding.
- Add memory-level eval diagnostics with `yaaml eval memories`.
- Add an offline eval-context cluster rerank backtest script.

## Query Cleaning

`build_recall_query` now removes:

- `<codex_internal_context ...>...</codex_internal_context>` blocks.
- `<environment_context>...</environment_context>` blocks.
- transient progress lines such as `Working (` and `Thinking (`.

This keeps repeated goal scaffolding, environment payloads, and transient agent
status out of recall embeddings while preserving actual user and assistant task
content.

## Memory Diagnostics

New command:

```bash
yaaml eval memories --eval-limit 200 --limit 25
```

The command groups recent eval results by memory and reports:

- selected count
- judged count
- useful, low, neutral, and n/a counts
- average numeric score
- mixed useful/low flag
- active/inactive state
- project id
- latest score and rationale snippet

On the current local database, the command quickly surfaces the memories that are
most context-sensitive rather than simply good or bad. Examples include YAAML
roadmap/rebuild memories and cross-project backfill patterns that are useful in
some contexts and noisy in others.

## Cluster Rerank Backtest

New script:

```bash
source ~/.zshrc >/dev/null 2>&1
python3 scripts/recall-cluster-rerank.py \
  --input-dir target/strict-kind-production \
  --label strict-kind-production \
  --out-dir target/cluster-rerank
```

The script builds positive and negative eval-context clusters per memory, embeds
each historical anchor query, and replays five cluster-informed rerank policies
against the frozen strict-kind-production 200-anchor library.

| Strategy | Avg Memories | Avg Known Score | Useful Known | Low Known | Useful Runs | Low Runs | Empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | 2.28 | 2.85 | 69 | 88 | 54 | 62 | 5 |
| cluster_boost | 2.33 | 2.87 | 71 | 88 | 56 | 63 | 4 |
| cluster_demote | 2.27 | 2.78 | 69 | 90 | 54 | 66 | 7 |
| cluster_margin | 2.29 | 2.85 | 71 | 88 | 56 | 64 | 8 |
| cluster_gate | 2.29 | 2.86 | 71 | 84 | 56 | 64 | 7 |
| cluster_count_weighted | 2.31 | 2.83 | 71 | 89 | 56 | 65 | 5 |

## Readout

The cluster signal is real and has full candidate-signal coverage on this
library, but it is still not production-ready as a default.

Best result: `cluster_gate`

- +2 useful known memories
- +2 useful capture runs
- -4 low known memories
- +2 low selection runs
- +2 empty recall runs

That is promising enough to keep exploring, but not enough to ship. The next
experiment should focus on reducing low-selection-run regressions from
`cluster_gate`, likely by applying cluster demotion only to mixed memories with
multiple prior evals and leaving one-sided positive histories as boosts only.

## Artifacts

```text
scripts/recall-cluster-rerank.py
target/cluster-rerank/cluster-rerank.summary.json
target/cluster-rerank/*.summary.json
target/cluster-rerank/*.details.jsonl
target/cluster-rerank/text-embedding-3-small.query-embeddings.json
```
