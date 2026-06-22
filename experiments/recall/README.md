# Recall Experiments

Recall experiments are stored as timestamped directories so strategy results can
be compared over time.

Each experiment directory should contain:

- `manifest.json`: run metadata, input paths, strategy names, and primary metrics.
- `summary.json`: strategy-level metrics plus deltas against the baseline.
- `details.jsonl`: one row per strategy per anchor with selected memory IDs and
  oracle comparison metrics.
- `REPORT.md`: human-readable readout generated from the structured files.

Primary metrics for 5x5 recall experiments:

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

Use `scripts/recall-5x5-experiment.py` to replay saved recall rankings from a
`scripts/backtest-recall-strategy.sh` output directory.
