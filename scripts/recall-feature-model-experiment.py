#!/usr/bin/env python3
"""Train a small feature model for recall selection and compare heuristics."""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any


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
    "production_selected",
]
KINDS = ["lesson", "workflow", "preference", "project_fact", "task_state", "unknown"]
THRESHOLDS = [0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65]


def load_rows(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def feature_names() -> list[str]:
    return ["bias", *FEATURES, *[f"kind:{kind}" for kind in KINDS]]


def raw_features(row: Row) -> list[float]:
    values = [1.0]
    for name in FEATURES:
        value = row.get(name)
        if isinstance(value, bool):
            values.append(1.0 if value else 0.0)
        else:
            values.append(float(value or 0.0))
    kind = str(row.get("memory_kind") or "unknown")
    values.extend(1.0 if kind == known_kind else 0.0 for known_kind in KINDS)
    return values


def fit_scaler(rows: list[Row]) -> tuple[list[float], list[float]]:
    vectors = [raw_features(row) for row in rows]
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


def train_logistic(rows: list[Row], epochs: int, learning_rate: float) -> tuple[list[float], list[float], list[float]]:
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
                weights[index] -= learning_rate * error * value
    return weights, means, scales


def predict(row: Row, weights: list[float], means: list[float], scales: list[float]) -> float:
    vector = transform(row, means, scales)
    return sigmoid(sum(weight * value for weight, value in zip(weights, vector)))


def rows_by_run(rows: list[Row]) -> dict[int, list[Row]]:
    grouped = defaultdict(list)
    for row in rows:
        grouped[int(row["run_id"])].append(row)
    for candidates in grouped.values():
        candidates.sort(key=lambda row: int(row["rank_index"]))
    return dict(grouped)


def selected_production(candidates: list[Row]) -> list[Row]:
    return [row for row in candidates if row["production_selected"]]


def selected_top2(candidates: list[Row]) -> list[Row]:
    return selected_production(candidates)[:2]


def selected_weak_top_abstain(candidates: list[Row]) -> list[Row]:
    selected = selected_top2(candidates)
    if not selected or float(selected[0]["score"]) < 1.05:
        return []
    return selected


def selected_model(
    candidates: list[Row],
    weights: list[float],
    means: list[float],
    scales: list[float],
    threshold: float,
) -> list[Row]:
    scored = [
        (predict(row, weights, means, scales), int(row["rank_index"]), row)
        for row in candidates
    ]
    scored.sort(key=lambda item: (-item[0], item[1]))
    return [row for probability, _rank, row in scored if probability >= threshold][:2]


def summarize_selection(name: str, selections: list[list[Row]]) -> Row:
    selected_rows = [row for selection in selections for row in selection]
    known = [row for row in selected_rows if row["label_score"] is not None]
    scores = [int(row["label_score"]) for row in known]
    return {
        "strategy": name,
        "runs": len(selections),
        "selected_memories": len(selected_rows),
        "known_selected_memories": len(known),
        "unknown_selected_memories": len(selected_rows) - len(known),
        "average_selected_per_run": len(selected_rows) / len(selections) if selections else 0.0,
        "average_known_score": sum(scores) / len(scores) if scores else None,
        "useful_known_selected": sum(1 for score in scores if score >= 4),
        "low_known_selected": sum(1 for score in scores if score <= 2),
        "useful_capture_runs": sum(
            1
            for selection in selections
            if any((row["label_score"] is not None and int(row["label_score"]) >= 4) for row in selection)
        ),
        "low_selection_runs": sum(
            1
            for selection in selections
            if any((row["label_score"] is not None and int(row["label_score"]) <= 2) for row in selection)
        ),
        "empty_runs": sum(1 for selection in selections if not selection),
    }


def utility(summary: Row) -> float:
    return (
        summary["useful_capture_runs"] * 3.0
        + summary["useful_known_selected"]
        - summary["low_known_selected"] * 1.5
        - summary["unknown_selected_memories"] * 0.25
        - summary["empty_runs"] * 0.05
    )


def tune_threshold(
    grouped: dict[int, list[Row]],
    train_run_ids: list[int],
    weights: list[float],
    means: list[float],
    scales: list[float],
) -> float:
    best_threshold = THRESHOLDS[0]
    best_utility = float("-inf")
    for threshold in THRESHOLDS:
        selections = [
            selected_model(grouped[run_id], weights, means, scales, threshold)
            for run_id in train_run_ids
        ]
        score = utility(summarize_selection(f"model_threshold_{threshold}", selections))
        if score > best_utility:
            best_utility = score
            best_threshold = threshold
    return best_threshold


def aggregate_summaries(summaries: list[Row], strategy: str) -> Row:
    total_runs = sum(summary["runs"] for summary in summaries)
    total_selected = sum(summary["selected_memories"] for summary in summaries)
    known_scores = []
    for summary in summaries:
        if summary["average_known_score"] is not None and summary["known_selected_memories"]:
            known_scores.extend([summary["average_known_score"]] * summary["known_selected_memories"])
    return {
        "strategy": strategy,
        "runs": total_runs,
        "selected_memories": total_selected,
        "known_selected_memories": sum(summary["known_selected_memories"] for summary in summaries),
        "unknown_selected_memories": sum(summary["unknown_selected_memories"] for summary in summaries),
        "average_selected_per_run": total_selected / total_runs if total_runs else 0.0,
        "average_known_score": sum(known_scores) / len(known_scores) if known_scores else None,
        "useful_known_selected": sum(summary["useful_known_selected"] for summary in summaries),
        "low_known_selected": sum(summary["low_known_selected"] for summary in summaries),
        "useful_capture_runs": sum(summary["useful_capture_runs"] for summary in summaries),
        "low_selection_runs": sum(summary["low_selection_runs"] for summary in summaries),
        "empty_runs": sum(summary["empty_runs"] for summary in summaries),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", type=Path, default=Path("target/recall-training-dataset.jsonl"))
    parser.add_argument("--out", type=Path, default=Path("experiments/recall/2026-06-23-feature-model/summary.json"))
    parser.add_argument("--epochs", type=int, default=160)
    parser.add_argument("--learning-rate", type=float, default=0.03)
    args = parser.parse_args()

    rows = load_rows(args.dataset)
    grouped = rows_by_run(rows)
    folds = sorted({int(row["cohort"]) for row in rows})
    by_strategy = defaultdict(list)
    thresholds = []
    for fold in folds:
        train_rows = [row for row in rows if int(row["cohort"]) != fold and row["label_useful"] is not None]
        test_run_ids = sorted({int(row["run_id"]) for row in rows if int(row["cohort"]) == fold})
        train_run_ids = sorted({int(row["run_id"]) for row in train_rows})
        weights, means, scales = train_logistic(train_rows, args.epochs, args.learning_rate)
        threshold = tune_threshold(grouped, train_run_ids, weights, means, scales)
        thresholds.append(threshold)
        strategies = {
            "production": [selected_production(grouped[run_id]) for run_id in test_run_ids],
            "top_2": [selected_top2(grouped[run_id]) for run_id in test_run_ids],
            "weak_top_abstain": [selected_weak_top_abstain(grouped[run_id]) for run_id in test_run_ids],
            "feature_model": [
                selected_model(grouped[run_id], weights, means, scales, threshold)
                for run_id in test_run_ids
            ],
        }
        for name, selections in strategies.items():
            by_strategy[name].append(summarize_selection(name, selections))

    summary = {
        "dataset": str(args.dataset),
        "folds": len(folds),
        "rows": len(rows),
        "labeled_rows": sum(1 for row in rows if row["label_score"] is not None),
        "feature_names": feature_names(),
        "thresholds": thresholds,
        "strategies": {
            strategy: aggregate_summaries(summaries, strategy)
            for strategy, summaries in by_strategy.items()
        },
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
