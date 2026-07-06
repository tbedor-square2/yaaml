"""Shared statistics helpers for YAAML recall experiments.

Implements the paired bootstrap confidence intervals required by
experiments/recall/README.md ("Significance"). Strategies are compared on the
same anchors: resample anchors with replacement, recompute each aggregate
metric for baseline and candidate on the resampled multiset, and take the
2.5/97.5 percentiles of the candidate-minus-baseline deltas.

Import from experiment runners:

    from recall_experiment_stats import paired_bootstrap_deltas

Rows are per-anchor dicts in the details.jsonl shape produced by
scripts/backtest-recall-strategy.sh (keys: run_id, average_known_score,
useful_known_selected, low_known_selected, captured_any_known_useful,
selected_any_known_low, oracle_has_useful, empty_recall, selected_count).
"""

from __future__ import annotations

import random
from typing import Any, Callable

Row = dict[str, Any]

BOOTSTRAP_RESAMPLES = 2000
BOOTSTRAP_SEED = 20260706

# Deltas smaller than these are not decision-relevant. Used only to separate
# "no detectable effect" (CI inside the relevant band) from "needs larger
# sample" (CI includes zero but extends past the band, so a relevant effect
# cannot be ruled out).
DECISION_RELEVANT_EFFECT = {
    "average_known_score": 0.15,
    "useful_known_selected": 5.0,
    "low_known_selected": 5.0,
    "useful_capture_runs": 4.0,
    "low_selection_runs": 4.0,
    "average_selected_per_anchor": 0.15,
    "empty_recall_runs": 5.0,
    "missed_useful_empty_runs": 2.0,
    "clean_abstention_runs": 5.0,
}


def _mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def metric_from_rows(metric: str, rows: list[Row]) -> float | None:
    """Recompute one aggregate metric from per-anchor detail rows."""
    if metric == "average_known_score":
        return _mean(
            [
                float(row["average_known_score"])
                for row in rows
                if row.get("average_known_score") is not None
            ]
        )
    if metric == "average_selected_per_anchor":
        return _mean([float(row["selected_count"]) for row in rows])
    if metric == "useful_known_selected":
        return float(sum(int(row["useful_known_selected"]) for row in rows))
    if metric == "low_known_selected":
        return float(sum(int(row["low_known_selected"]) for row in rows))
    if metric == "useful_capture_runs":
        return float(sum(1 for row in rows if row["captured_any_known_useful"]))
    if metric == "low_selection_runs":
        return float(sum(1 for row in rows if row["selected_any_known_low"]))
    if metric == "empty_recall_runs":
        return float(sum(1 for row in rows if row["empty_recall"]))
    if metric == "missed_useful_empty_runs":
        return float(
            sum(1 for row in rows if row["empty_recall"] and row["oracle_has_useful"])
        )
    if metric == "clean_abstention_runs":
        return float(
            sum(
                1
                for row in rows
                if row["empty_recall"] and not row["oracle_has_useful"]
            )
        )
    raise KeyError(f"unknown metric: {metric}")


def pair_rows_by_anchor(
    baseline_rows: list[Row], candidate_rows: list[Row]
) -> list[tuple[Row, Row]]:
    """Pair detail rows on run_id; both runs must cover the same anchors."""
    baseline_by_id = {row["run_id"]: row for row in baseline_rows}
    candidate_by_id = {row["run_id"]: row for row in candidate_rows}
    shared = sorted(baseline_by_id.keys() & candidate_by_id.keys())
    if not shared:
        raise ValueError("no shared anchors between baseline and candidate rows")
    return [(baseline_by_id[run_id], candidate_by_id[run_id]) for run_id in shared]


def classify_delta(
    metric: str, ci_low: float, ci_high: float
) -> str:
    """Label a delta per the methodology contract.

    - confirmed: CI excludes zero.
    - no detectable effect: CI includes zero and stays inside the
      decision-relevant band, so any real effect is too small to matter.
    - needs larger sample: CI includes zero but extends past the band, so a
      decision-relevant effect cannot be ruled out at this sample size.
    """
    if ci_low > 0 or ci_high < 0:
        return "confirmed"
    relevant = DECISION_RELEVANT_EFFECT.get(metric, 0.0)
    if relevant and (ci_low < -relevant or ci_high > relevant):
        return "needs larger sample"
    return "no detectable effect"


def paired_bootstrap_deltas(
    baseline_rows: list[Row],
    candidate_rows: list[Row],
    metrics: list[str],
    *,
    resamples: int = BOOTSTRAP_RESAMPLES,
    seed: int = BOOTSTRAP_SEED,
) -> dict[str, Row]:
    """Paired bootstrap CI for each metric delta (candidate minus baseline).

    Returns, per metric: point_delta, ci_95 [low, high], and verdict. Metrics
    where either side is undefined (for example no known-score anchors) get
    null CI fields and a "no data" verdict.
    """
    pairs = pair_rows_by_anchor(baseline_rows, candidate_rows)
    rng = random.Random(seed)
    n = len(pairs)
    deltas_per_metric: dict[str, list[float]] = {metric: [] for metric in metrics}
    for _ in range(resamples):
        indices = [rng.randrange(n) for _ in range(n)]
        base_sample = [pairs[i][0] for i in indices]
        cand_sample = [pairs[i][1] for i in indices]
        for metric in metrics:
            base_value = metric_from_rows(metric, base_sample)
            cand_value = metric_from_rows(metric, cand_sample)
            if base_value is None or cand_value is None:
                continue
            deltas_per_metric[metric].append(cand_value - base_value)

    results: dict[str, Row] = {}
    baseline_all = [pair[0] for pair in pairs]
    candidate_all = [pair[1] for pair in pairs]
    for metric in metrics:
        base_point = metric_from_rows(metric, baseline_all)
        cand_point = metric_from_rows(metric, candidate_all)
        deltas = sorted(deltas_per_metric[metric])
        if base_point is None or cand_point is None or len(deltas) < resamples // 2:
            results[metric] = {
                "point_delta": None,
                "ci_95": None,
                "verdict": "no data",
            }
            continue
        low = deltas[int(0.025 * (len(deltas) - 1))]
        high = deltas[int(0.975 * (len(deltas) - 1))]
        results[metric] = {
            "point_delta": cand_point - base_point,
            "ci_95": [low, high],
            "verdict": classify_delta(metric, low, high),
        }
    return results
