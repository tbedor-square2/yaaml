#!/usr/bin/env python3
"""Train a small calibrated feature model for saved-candidate recall selection.

This is a saved-candidate replay harness: it does not rerun retrieval. The
model trains on labeled candidates in the training folds, applies isotonic
calibration on training-fold predictions, tunes a probability threshold on
training-fold utility, and evaluates on held-out folds.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from recall_experiment_stats import metric_from_rows, paired_bootstrap_deltas

Row = dict[str, Any]

FEATURES = [
    "score",
    "similarity",
    "vector_score",
    "context_score",
    "project_bonus",
    "task_key_bonus",
    "global_durable_bonus",
    "matched_task_key_count",
    "filter_reason_count",
]
KINDS = ["lesson", "workflow", "preference", "project_fact", "task_state", "unknown"]
THRESHOLDS = [
    0.10,
    0.15,
    0.20,
    0.25,
    0.30,
    0.35,
    0.40,
    0.45,
    0.50,
    0.55,
    0.60,
    0.65,
    0.70,
    0.75,
    0.80,
    0.85,
    0.90,
    0.95,
]
METRICS = [
    "average_known_score",
    "useful_known_selected",
    "low_known_selected",
    "useful_capture_runs",
    "low_selection_runs",
    "average_selected_per_anchor",
    "empty_recall_runs",
    "missed_useful_empty_runs",
    "clean_abstention_runs",
]


def load_rows(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def feature_names() -> list[str]:
    return ["bias", *FEATURES, *[f"kind:{kind}" for kind in KINDS]]


def raw_features(row: Row) -> list[float]:
    values = [1.0]
    for name in FEATURES:
        values.append(float(row.get(name) or 0.0))
    kind = str(row.get("memory_kind") or "unknown")
    values.extend(1.0 if kind == known_kind else 0.0 for known_kind in KINDS)
    return values


def fit_scaler(rows: list[Row]) -> tuple[list[float], list[float]]:
    vectors = [raw_features(row) for row in rows]
    if not vectors:
        raise SystemExit("cannot train feature model without labeled rows")
    means = []
    scales = []
    for column in range(len(vectors[0])):
        if column == 0:
            means.append(0.0)
            scales.append(1.0)
            continue
        values = [vector[column] for vector in vectors]
        mean = sum(values) / len(values)
        variance = sum((value - mean) ** 2 for value in values) / len(values)
        means.append(mean)
        scales.append(math.sqrt(variance) or 1.0)
    return means, scales


def transform(row: Row, means: list[float], scales: list[float]) -> list[float]:
    return [
        (value - means[index]) / scales[index]
        for index, value in enumerate(raw_features(row))
    ]


def sigmoid(value: float) -> float:
    if value >= 0:
        z = math.exp(-value)
        return 1.0 / (1.0 + z)
    z = math.exp(value)
    return z / (1.0 + z)


def train_logistic(
    rows: list[Row],
    epochs: int,
    learning_rate: float,
    l2: float,
) -> tuple[list[float], list[float], list[float]]:
    labeled = [row for row in rows if row["label_useful"] is not None]
    means, scales = fit_scaler(labeled)
    weights = [0.0 for _ in feature_names()]
    for _epoch in range(epochs):
        for row in labeled:
            vector = transform(row, means, scales)
            target = 1.0 if row["label_useful"] else 0.0
            prediction = sigmoid(sum(weight * value for weight, value in zip(weights, vector)))
            error = prediction - target
            for index, value in enumerate(vector):
                regularization = 0.0 if index == 0 else l2 * weights[index]
                weights[index] -= learning_rate * (error * value + regularization)
    return weights, means, scales


def predict_raw(row: Row, weights: list[float], means: list[float], scales: list[float]) -> float:
    vector = transform(row, means, scales)
    return sigmoid(sum(weight * value for weight, value in zip(weights, vector)))


def fit_isotonic(points: list[tuple[float, float]]) -> list[tuple[float, float, float]]:
    """Fit nondecreasing isotonic calibration with PAVA.

    Returns blocks as (min_score, max_score, calibrated_probability).
    """
    if not points:
        return [(0.0, 1.0, 0.0)]
    sorted_points = sorted(points, key=lambda item: item[0])
    blocks: list[dict[str, float]] = []
    for score, target in sorted_points:
        blocks.append({
            "min": score,
            "max": score,
            "sum": target,
            "weight": 1.0,
            "value": target,
        })
        while len(blocks) >= 2 and blocks[-2]["value"] > blocks[-1]["value"]:
            right = blocks.pop()
            left = blocks.pop()
            weight = left["weight"] + right["weight"]
            total = left["sum"] + right["sum"]
            blocks.append({
                "min": left["min"],
                "max": right["max"],
                "sum": total,
                "weight": weight,
                "value": total / weight,
            })
    return [(block["min"], block["max"], block["value"]) for block in blocks]


def calibrate(score: float, blocks: list[tuple[float, float, float]]) -> float:
    for min_score, max_score, value in blocks:
        if min_score <= score <= max_score:
            return value
    if score < blocks[0][0]:
        return blocks[0][2]
    return blocks[-1][2]


def rows_by_run(rows: list[Row]) -> dict[int, list[Row]]:
    grouped: dict[int, list[Row]] = defaultdict(list)
    for row in rows:
        grouped[int(row["run_id"])].append(row)
    for candidates in grouped.values():
        candidates.sort(key=lambda row: int(row["rank_index"]))
    return dict(grouped)


def selected_production(candidates: list[Row]) -> list[Row]:
    return [row for row in candidates if row["production_selected"]]


def eligible_model_candidates(candidates: list[Row]) -> list[Row]:
    eligible = [row for row in candidates if row.get("eligible_filter_pool")]
    return eligible if eligible else candidates


def model_probability(
    row: Row,
    weights: list[float],
    means: list[float],
    scales: list[float],
    isotonic: list[tuple[float, float, float]],
) -> float:
    return calibrate(predict_raw(row, weights, means, scales), isotonic)


def selected_model(
    candidates: list[Row],
    weights: list[float],
    means: list[float],
    scales: list[float],
    isotonic: list[tuple[float, float, float]],
    threshold: float,
    limit: int,
) -> list[Row]:
    scored = [
        (model_probability(row, weights, means, scales, isotonic), int(row["rank_index"]), row)
        for row in eligible_model_candidates(candidates)
    ]
    scored.sort(key=lambda item: (-item[0], item[1], int(item[2]["memory_id"])))
    return [row for probability, _rank, row in scored if probability >= threshold][:limit]


def oracle_scores_for_run(candidates: list[Row]) -> dict[int, int]:
    scores = {}
    for row in candidates:
        if row["label_score"] is not None:
            scores[int(row["memory_id"])] = int(row["label_score"])
    return scores


def details_row(
    label: str,
    run_id: int,
    candidates: list[Row],
    selected: list[Row],
) -> Row:
    selected_ids = [int(row["memory_id"]) for row in selected]
    scores = oracle_scores_for_run(candidates)
    known_scores = [scores[memory_id] for memory_id in selected_ids if memory_id in scores]
    oracle_scores = list(scores.values())
    first = candidates[0] if candidates else {}
    return {
        "strategy": label,
        "case": f"eval-library-{run_id}",
        "run_id": run_id,
        "session_id": first.get("session_id"),
        "turn_ordinal": first.get("turn_ordinal"),
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": sum(known_scores) / len(known_scores) if known_scores else None,
        "useful_known_selected": sum(1 for score in known_scores if score >= 4),
        "low_known_selected": sum(1 for score in known_scores if score <= 2),
        "captured_any_known_useful": any(score >= 4 for score in known_scores),
        "selected_any_known_low": any(score <= 2 for score in known_scores),
        "oracle_has_useful": any(score >= 4 for score in oracle_scores),
        "oracle_best_score": max(oracle_scores) if oracle_scores else None,
        "empty_recall": len(selected_ids) == 0,
    }


def utility(rows: list[Row]) -> float:
    return (
        sum(row["useful_known_selected"] for row in rows) * 3.0
        + sum(1 for row in rows if row["captured_any_known_useful"]) * 2.0
        - sum(row["low_known_selected"] for row in rows) * 2.0
        - sum(row["selected_count"] for row in rows) * 0.50
        - sum(1 for row in rows if row["empty_recall"] and row["oracle_has_useful"]) * 0.75
    )


def tune_threshold(
    grouped: dict[int, list[Row]],
    train_run_ids: list[int],
    weights: list[float],
    means: list[float],
    scales: list[float],
    isotonic: list[tuple[float, float, float]],
    limit: int,
) -> float:
    best_threshold = THRESHOLDS[0]
    best_utility = float("-inf")
    for threshold in THRESHOLDS:
        rows = [
            details_row(
                f"feature_model_threshold_{threshold}",
                run_id,
                grouped[run_id],
                selected_model(grouped[run_id], weights, means, scales, isotonic, threshold, limit),
            )
            for run_id in train_run_ids
        ]
        score = utility(rows)
        if score > best_utility:
            best_utility = score
            best_threshold = threshold
    return best_threshold


def summary_from_details(label: str, rows: list[Row]) -> Row:
    return {
        "strategy": label,
        "anchors": len(rows),
        "selected_memories": sum(row["selected_count"] for row in rows),
        "average_selected_per_anchor": metric_from_rows("average_selected_per_anchor", rows),
        "known_selected_memories": sum(row["known_selected_count"] for row in rows),
        "unknown_selected_memories": sum(row["unknown_selected_count"] for row in rows),
        "average_known_score": metric_from_rows("average_known_score", rows),
        "useful_known_selected": metric_from_rows("useful_known_selected", rows),
        "low_known_selected": metric_from_rows("low_known_selected", rows),
        "useful_capture_runs": metric_from_rows("useful_capture_runs", rows),
        "low_selection_runs": metric_from_rows("low_selection_runs", rows),
        "oracle_useful_runs": sum(1 for row in rows if row["oracle_has_useful"]),
        "empty_recall_runs": metric_from_rows("empty_recall_runs", rows),
        "missed_useful_empty_runs": metric_from_rows("missed_useful_empty_runs", rows),
        "clean_abstention_runs": metric_from_rows("clean_abstention_runs", rows),
    }


def build_report(manifest: Row) -> str:
    baseline = manifest["baseline_summary"]
    candidate = manifest["candidate_summary"]
    lines = [
        "# Learned Weight Calibration",
        "",
        f"Date: {manifest['date']}",
        f"Dataset: `{manifest['dataset']}`",
        "",
        "## Experiment",
        "",
        "Trained a logistic feature model on labeled saved candidates, calibrated training-fold predictions with isotonic regression, tuned a probability threshold on training-fold utility, and replayed selection on held-out folds.",
        "",
        "## Results",
        "",
        "| Metric | Production | Feature model | Delta | 95% CI | Verdict |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for metric in METRICS:
        delta = manifest["deltas"][metric]
        ci = delta["ci_95"]
        ci_text = "n/a" if ci is None else f"[{ci[0]:.3f}, {ci[1]:.3f}]"
        point = delta["point_delta"]
        point_text = "n/a" if point is None else f"{point:.3f}"
        base = baseline[metric]
        cand = candidate[metric]
        lines.append(
            f"| `{metric}` | {base:.3f} | {cand:.3f} | {point_text} | {ci_text} | {delta['verdict']} |"
        )
    lines.extend(
        [
            "",
            "## Model",
            "",
            f"- Rows: {manifest['rows']}",
            f"- Labeled rows: {manifest['labeled_rows']}",
            f"- Folds: {manifest['folds']}",
            f"- Thresholds by fold: {', '.join(str(value) for value in manifest['thresholds'])}",
            f"- Selection limit: {manifest['selection_limit']}",
            "",
            "## Decision",
            "",
            manifest["decision"],
            "",
            "## Artifacts",
            "",
            "- `manifest.json` records model configuration, summaries, and paired deltas.",
            "- `production.details.jsonl` records the saved production selections.",
            "- `feature_model.details.jsonl` records held-out feature-model selections.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", type=Path, default=Path("target/recall-training-dataset.jsonl"))
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-07-06-learned-weight-calibration"))
    parser.add_argument("--label", default="learned-weight-calibration")
    parser.add_argument("--date", default="2026-07-06")
    parser.add_argument("--epochs", type=int, default=180)
    parser.add_argument("--learning-rate", type=float, default=0.03)
    parser.add_argument("--l2", type=float, default=0.001)
    parser.add_argument("--selection-limit", type=int, default=2)
    args = parser.parse_args()

    rows = load_rows(args.dataset)
    grouped = rows_by_run(rows)
    folds = sorted({int(row["cohort"]) for row in rows})
    if not folds:
        raise SystemExit("dataset has no folds")
    production_details: list[Row] = []
    feature_details: list[Row] = []
    thresholds = []
    fold_summaries = []
    for fold in folds:
        train_rows = [
            row for row in rows if int(row["cohort"]) != fold and row["label_useful"] is not None
        ]
        train_run_ids = sorted({int(row["run_id"]) for row in rows if int(row["cohort"]) != fold})
        test_run_ids = sorted({int(row["run_id"]) for row in rows if int(row["cohort"]) == fold})
        weights, means, scales = train_logistic(train_rows, args.epochs, args.learning_rate, args.l2)
        calibration_points = [
            (
                predict_raw(row, weights, means, scales),
                1.0 if row["label_useful"] else 0.0,
            )
            for row in train_rows
        ]
        isotonic = fit_isotonic(calibration_points)
        threshold = tune_threshold(
            grouped,
            train_run_ids,
            weights,
            means,
            scales,
            isotonic,
            args.selection_limit,
        )
        thresholds.append(threshold)
        fold_feature_rows = []
        fold_production_rows = []
        for run_id in test_run_ids:
            candidates = grouped[run_id]
            production = selected_production(candidates)
            modeled = selected_model(
                candidates,
                weights,
                means,
                scales,
                isotonic,
                threshold,
                args.selection_limit,
            )
            fold_production_rows.append(details_row("production", run_id, candidates, production))
            fold_feature_rows.append(details_row(args.label, run_id, candidates, modeled))
        production_details.extend(fold_production_rows)
        feature_details.extend(fold_feature_rows)
        fold_summaries.append(
            {
                "fold": fold,
                "train_labeled_rows": len(train_rows),
                "test_runs": len(test_run_ids),
                "threshold": threshold,
                "production": summary_from_details("production", fold_production_rows),
                "feature_model": summary_from_details(args.label, fold_feature_rows),
            }
        )

    production_details.sort(key=lambda row: row["run_id"])
    feature_details.sort(key=lambda row: row["run_id"])
    out_dir = args.out_dir.expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    production_path = out_dir / "production.details.jsonl"
    feature_path = out_dir / "feature_model.details.jsonl"
    write_jsonl(production_path, production_details)
    write_jsonl(feature_path, feature_details)
    deltas = paired_bootstrap_deltas(production_details, feature_details, METRICS)
    useful_delta = deltas["useful_known_selected"]
    low_delta = deltas["low_known_selected"]
    decision = (
        "Do not ship this learned selector. Useful-known selection did not improve with a CI excluding zero."
    )
    if (
        useful_delta["ci_95"] is not None
        and useful_delta["ci_95"][0] > 0
        and (low_delta["ci_95"] is None or low_delta["ci_95"][1] <= 0)
    ):
        decision = (
            "Candidate is promising on screening: useful-known selection improved without a confirmed low-selection increase. Confirm on holdout before shipping."
        )
    manifest = {
        "date": args.date,
        "dataset": str(args.dataset),
        "label": args.label,
        "rows": len(rows),
        "labeled_rows": sum(1 for row in rows if row["label_score"] is not None),
        "folds": len(folds),
        "feature_names": feature_names(),
        "thresholds": thresholds,
        "selection_limit": args.selection_limit,
        "fold_summaries": fold_summaries,
        "baseline_details": str(production_path),
        "candidate_details": str(feature_path),
        "baseline_summary": summary_from_details("production", production_details),
        "candidate_summary": summary_from_details(args.label, feature_details),
        "deltas": deltas,
        "decision": decision,
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (out_dir / "REPORT.md").write_text(build_report(manifest))
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
