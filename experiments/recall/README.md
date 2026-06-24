# Recall Experiments

Recall experiments are stored as timestamped directories so strategy results can
be compared over time.

Each experiment directory should contain:

- `manifest.json`: run metadata, input paths, strategy names, and primary metrics.
- `summary.json`: strategy-level metrics plus deltas against the baseline.
- `details.jsonl`: one row per strategy per anchor with selected memory IDs and
  oracle comparison metrics.
- `REPORT.md`: human-readable readout generated from the structured files.

Primary metrics for recall experiments:

- `average_known_score`
- `useful_known_selected`
- `low_known_selected`
- `useful_capture_runs`

Abstention metrics are tracked separately from score metrics:

- `empty_recall_rate`
- `empty_recall_runs`
- `clean_abstention_runs`
- `missed_useful_empty_runs`
- `missed_useful_empty_rate`

Use `scripts/recall-5x5-experiment.py` to replay the original five-strategy
exercise from a `scripts/backtest-recall-strategy.sh` output directory.

Use `scripts/recall-10x10-experiment.py` for the expanded exercise. It compares
ten strategy variants over ten deterministic cohorts, then reports aggregate
results plus cohort stability.

Use `scripts/recall-health-10x10-experiment.py` for the health-aware exercise.
It compares failure-mode-specific recall policies using the same frozen ranking
and oracle inputs plus memory health diagnostics from the local YAAML database.

Use `scripts/recall-candidate-5x5-experiment.py` to compare recall-improvement
candidate families. It runs five strategies across five cohorts for candidate
pool sizing, dynamic recall count, query-signal proxies, hybrid-generation
proxies, memory lifecycle suppression, and abstention gates. Query-signal and
hybrid-generation families replay over already retrieved candidates; they do
not measure candidates that a fresh query embedding or lexical retrieval pass
would newly retrieve.

Use `scripts/recall-techniques-5x5-experiment.py` for a broader technique
inventory. It runs five strategies across five cohorts for each recall
technique category: candidate generation, hard gating, reranking, selection
budgeting, LLM-filter proxies, memory corpus quality, and eval feedback. The
report is intended to compare useful recall against context volume, not just
average recall score.
