#!/usr/bin/env python3
"""Export candidate-level recall training rows from saved backtest artifacts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


Row = dict[str, Any]


def load_json(path: Path) -> Row:
    return json.loads(path.read_text())


def oracle_scores(path: Path) -> dict[int, int]:
    data = load_json(path)
    scores = {}
    for result in data.get("results") or []:
        score = str(result.get("judge_score") or "")
        if score in {"1", "2", "3", "4", "5"}:
            scores[int(result["memory_id"])] = int(score)
    return scores


def candidate_features(candidate: Row, rank_index: int, selected_ids: set[int], label: int | None) -> Row:
    rank = candidate.get("rank") or {}
    memory_id = int(candidate["memory_id"])
    return {
        "memory_id": memory_id,
        "rank_index": rank_index,
        "production_selected": memory_id in selected_ids,
        "score": float(candidate.get("score") or 0.0),
        "similarity": float(candidate.get("similarity") or 0.0),
        "vector_score": float(rank.get("vector_score") or candidate.get("similarity") or 0.0),
        "context_score": float(rank.get("context_score") or 0.0),
        "project_bonus": float(rank.get("project_bonus") or 0.0),
        "task_key_bonus": float(rank.get("task_key_bonus") or 0.0),
        "global_durable_bonus": float(rank.get("global_durable_bonus") or 0.0),
        "matched_task_key_count": len(rank.get("matched_task_keys") or []),
        "filter_reason_count": len(rank.get("filter_reasons") or candidate.get("filter_reasons") or []),
        "memory_kind": candidate.get("memory_kind") or "unknown",
        "label_score": label,
        "label_useful": None if label is None else label >= 4,
        "label_low": None if label is None else label <= 2,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, default=Path("target/strict-kind-production"))
    parser.add_argument("--label", default="strict-kind-production")
    parser.add_argument("--out", type=Path, default=Path("target/recall-training-dataset.jsonl"))
    parser.add_argument("--summary", type=Path, default=Path("target/recall-training-dataset.summary.json"))
    args = parser.parse_args()

    rows = []
    recall_paths = sorted(args.input_dir.glob(f"{args.label}.recall-*.json"))
    for index, recall_path in enumerate(recall_paths):
        run_id = int(recall_path.stem.split("-")[-1])
        oracle_path = args.input_dir / f"oracle-{run_id}.json"
        if not oracle_path.exists():
            continue
        recall = load_json(recall_path)
        scores = oracle_scores(oracle_path)
        selected_ids = {int(memory_id) for memory_id in recall.get("selected_memory_ids") or []}
        for rank_index, candidate in enumerate(recall.get("ranking") or []):
            memory_id = int(candidate["memory_id"])
            row = {
                "run_id": run_id,
                "cohort": index % 5,
                "session_id": recall.get("session_id"),
                "turn_ordinal": recall.get("turn_ordinal"),
                "query_source": recall.get("query_source"),
                "project_id": recall.get("project_id"),
                **candidate_features(candidate, rank_index, selected_ids, scores.get(memory_id)),
            }
            rows.append(row)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(json.dumps(row, sort_keys=True) for row in rows) + "\n")
    labeled = [row for row in rows if row["label_score"] is not None]
    summary = {
        "input_dir": str(args.input_dir),
        "label": args.label,
        "output": str(args.out),
        "runs": len({row["run_id"] for row in rows}),
        "rows": len(rows),
        "labeled_rows": len(labeled),
        "useful_labeled_rows": sum(1 for row in labeled if row["label_useful"]),
        "low_labeled_rows": sum(1 for row in labeled if row["label_low"]),
        "unlabeled_rows": len(rows) - len(labeled),
    }
    args.summary.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
