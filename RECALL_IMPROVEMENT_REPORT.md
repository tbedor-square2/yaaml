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

## Cleaned Query Follow-up

After adding recall-query noise stripping, the strict-kind baseline was rebuilt
against the same frozen 200-anchor library:

```bash
BACKTEST_OUT_DIR=target/strict-kind-cleaned \
BACKTEST_ANCHORS_FILE=target/strict-kind-production/anchors.tsv \
scripts/backtest-recall-strategy.sh . strict-kind-cleaned
```

| Strategy | Avg Memories | Avg Known Score | Useful Known | Low Known | Useful Runs | Low Runs | Empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| old strict-kind-production | 2.28 | 2.85 | 69 | 88 | 54 | 62 | 5 |
| cleaned strict-kind | 2.28 | 2.81 | 65 | 84 | 51 | 60 | 5 |

Query cleaning by itself is not an overall recall-quality win on the frozen
library. It removes some bad recall, but also removes more useful recall than
expected. This suggests the stripped internal/environment context was sometimes
acting as accidental retrieval signal for YAAML-heavy sessions.

The cluster reranker was then replayed against the cleaned strict-kind outputs:

```bash
source ~/.zshrc >/dev/null 2>&1
python3 scripts/recall-cluster-rerank.py \
  --input-dir target/strict-kind-cleaned \
  --label strict-kind-cleaned \
  --out-dir target/cluster-rerank-cleaned-tuned
```

| Strategy | Avg Memories | Avg Known Score | Useful Known | Low Known | Useful Runs | Low Runs | Empty |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| cleaned baseline | 2.28 | 2.81 | 65 | 84 | 51 | 60 | 5 |
| cluster_boost | 2.33 | 2.83 | 67 | 84 | 53 | 61 | 4 |
| cluster_gate | 2.27 | 2.80 | 66 | 80 | 52 | 62 | 7 |
| cluster_margin | 2.29 | 2.84 | 69 | 82 | 53 | 61 | 8 |
| cluster_margin_fallback | 2.31 | 2.84 | 69 | 82 | 53 | 61 | 4 |
| cluster_gate_mixed_demote | 2.31 | 2.82 | 66 | 83 | 52 | 60 | 4 |
| cluster_gate_min2 | 2.30 | 2.83 | 65 | 83 | 51 | 59 | 4 |

Best next candidate: `cluster_margin_fallback`.

- Restores useful known memory count from 65 back to 69.
- Reduces low known memories from old baseline 88 to 82.
- Improves empty recalls versus cleaned baseline, 5 to 4.
- Still trails old useful-run count, 53 vs. 54.
- Slightly increases low-run count versus cleaned baseline, 61 vs. 60.

The next production-oriented experiment should implement `cluster_margin_fallback`
behind a disabled config flag and replay it through the Rust path, then compare
against both old strict-kind and cleaned strict-kind before enabling it by
default.
