#!/usr/bin/env python3
"""Measure whether known-useful memories appear in the recall candidate pool.

This is the Phase 0 pool-recall ceiling diagnostic from
experiments/recall/README.md. It reads a frozen anchor TSV, oracle JSON files
from a backtest run, and recall JSON files emitted with --debug-ranking. For
each anchor with at least one useful oracle-labeled memory, it checks whether
any useful memory is present in the ranked candidate pool and whether any was
selected.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sqlite3
from pathlib import Path
from typing import Any

Row = dict[str, Any]


def read_anchors(path: Path) -> list[Row]:
    anchors = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) != 4:
            raise SystemExit(f"malformed anchor line in {path}: {line!r}")
        run_id, session_id, turn_ordinal, case_label = fields
        anchors.append(
            {
                "run_id": int(run_id),
                "session_id": session_id,
                "turn_ordinal": int(turn_ordinal),
                "case": case_label,
            }
        )
    if not anchors:
        raise SystemExit(f"no anchors in {path}")
    return anchors


def load_json(path: Path) -> Row:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError as exc:
        raise SystemExit(f"missing required file: {path}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse {path}: {exc}") from exc


def useful_oracle_memory_ids(oracle: Row) -> set[int]:
    useful: set[int] = set()
    for result in oracle.get("results") or []:
        try:
            score = int(str(result.get("judge_score")))
        except ValueError:
            continue
        if score >= 4 and result.get("memory_id") is not None:
            useful.add(int(result["memory_id"]))
    return useful


def load_active_memory_ids(db_path: Path, memory_ids: set[int]) -> set[int]:
    if not memory_ids:
        return set()
    if not db_path.exists():
        return set()
    placeholders = ",".join("?" for _ in memory_ids)
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        f"""
        SELECT id
        FROM memories
        WHERE id IN ({placeholders})
          AND is_active = 1
          AND superseded_by_memory_id IS NULL
        """,
        sorted(memory_ids),
    ).fetchall()
    return {int(row[0]) for row in rows}


def memory_ids_from_ranking(recall: Row) -> list[int]:
    ids: list[int] = []
    for candidate in recall.get("ranking") or []:
        if candidate.get("memory_id") is not None:
            ids.append(int(candidate["memory_id"]))
    return ids


def selected_memory_ids(recall: Row) -> set[int]:
    return {int(memory_id) for memory_id in recall.get("selected_memory_ids") or []}


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def percent(numerator: int, denominator: int) -> str:
    if denominator == 0:
        return "n/a"
    return f"{numerator / denominator:.1%}"


def build_report(summary: Row, manifest: Row) -> str:
    return "\n".join(
        [
            "# Pool Recall Ceiling Oracle",
            "",
            f"Date: {manifest['date']}",
            f"Anchors file: `{manifest['anchors_file']}`",
            f"Recall source directory: `{manifest['input_dir']}`",
            "",
            "## Question",
            "",
            "For anchors with a known-useful oracle memory, does production retrieval put any known-useful memory into the 16-candidate ranked pool?",
            "",
            "## Results",
            "",
            f"- Anchors: {summary['anchors']}",
            f"- Oracle-useful anchors: {summary['oracle_useful_anchors']}",
            f"- Known-useful in pool: {summary['known_useful_in_pool']} ({percent(summary['known_useful_in_pool'], summary['oracle_useful_anchors'])})",
            f"- Known-useful selected: {summary['known_useful_selected']} ({percent(summary['known_useful_selected'], summary['oracle_useful_anchors'])})",
            f"- Missing due to retrieval: {summary['missing_due_to_retrieval']}",
            f"- Missing due to selection: {summary['missing_due_to_selection']}",
            "",
            "## Active-Only Results",
            "",
            "These exclude oracle-useful memories that are no longer active in the current memory corpus.",
            "",
            f"- Active oracle-useful anchors: {summary['active_oracle_useful_anchors']}",
            f"- Active known-useful in pool: {summary['active_known_useful_in_pool']} ({percent(summary['active_known_useful_in_pool'], summary['active_oracle_useful_anchors'])})",
            f"- Active known-useful selected: {summary['active_known_useful_selected']} ({percent(summary['active_known_useful_selected'], summary['active_oracle_useful_anchors'])})",
            f"- Active missing due to retrieval: {summary['active_missing_due_to_retrieval']}",
            f"- Active missing due to selection: {summary['active_missing_due_to_selection']}",
            "",
            "## Interpretation",
            "",
            summary["decision"],
            "",
            "## Artifacts",
            "",
            "- `manifest.json` records inputs and summary metrics.",
            "- `cases.jsonl` records per-anchor useful IDs, pool membership, selected membership, and miss class.",
            "",
        ]
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--input-dir", type=Path, required=True,
                        help="Backtest output directory containing oracle-*.json and recall JSON files")
    parser.add_argument("--label", default=dt.date.today().isoformat())
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db",
                        help="Current YAAML database used to compute active-only oracle metrics")
    parser.add_argument("--strategy-label", default="anchor-refresh",
                        help="Prefix used in recall files: <strategy-label>.recall-<run_id>.json")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(__file__).resolve().parent.parent
        / "experiments"
        / "recall"
        / f"{dt.date.today().isoformat()}-pool-recall-oracle",
    )
    args = parser.parse_args()

    anchors_file = args.anchors_file.expanduser().resolve()
    input_dir = args.input_dir.expanduser().resolve()
    out_dir = args.out_dir.expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    anchors = read_anchors(anchors_file)
    oracle_useful_ids_by_run: dict[int, set[int]] = {}
    all_useful_ids: set[int] = set()
    for anchor in anchors:
        run_id = anchor["run_id"]
        oracle = load_json(input_dir / f"oracle-{run_id}.json")
        useful_ids = useful_oracle_memory_ids(oracle)
        oracle_useful_ids_by_run[run_id] = useful_ids
        all_useful_ids.update(useful_ids)
    active_memory_ids = load_active_memory_ids(args.db.expanduser(), all_useful_ids)
    cases: list[Row] = []
    for anchor in anchors:
        run_id = anchor["run_id"]
        recall = load_json(input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        useful_ids = oracle_useful_ids_by_run[run_id]
        active_useful_ids = useful_ids & active_memory_ids
        ranking_ids = memory_ids_from_ranking(recall)
        ranking_id_set = set(ranking_ids)
        selected_ids = selected_memory_ids(recall)
        useful_in_pool = sorted(useful_ids & ranking_id_set)
        useful_selected = sorted(useful_ids & selected_ids)
        active_useful_in_pool = sorted(active_useful_ids & ranking_id_set)
        active_useful_selected = sorted(active_useful_ids & selected_ids)
        has_useful = bool(useful_ids)
        has_active_useful = bool(active_useful_ids)
        miss_class = None
        if has_useful and not useful_selected:
            miss_class = "missing_due_to_selection" if useful_in_pool else "missing_due_to_retrieval"
        active_miss_class = None
        if has_active_useful and not active_useful_selected:
            active_miss_class = (
                "missing_due_to_selection"
                if active_useful_in_pool
                else "missing_due_to_retrieval"
            )
        cases.append(
            {
                **anchor,
                "oracle_useful_memory_ids": sorted(useful_ids),
                "active_oracle_useful_memory_ids": sorted(active_useful_ids),
                "ranking_memory_ids": ranking_ids,
                "selected_memory_ids": sorted(selected_ids),
                "has_oracle_useful": has_useful,
                "has_active_oracle_useful": has_active_useful,
                "known_useful_in_pool": bool(useful_in_pool),
                "known_useful_selected": bool(useful_selected),
                "active_known_useful_in_pool": bool(active_useful_in_pool),
                "active_known_useful_selected": bool(active_useful_selected),
                "useful_memory_ids_in_pool": useful_in_pool,
                "useful_memory_ids_selected": useful_selected,
                "active_useful_memory_ids_in_pool": active_useful_in_pool,
                "active_useful_memory_ids_selected": active_useful_selected,
                "miss_class": miss_class,
                "active_miss_class": active_miss_class,
            }
        )

    useful_cases = [case for case in cases if case["has_oracle_useful"]]
    oracle_useful_anchors = len(useful_cases)
    known_useful_in_pool = sum(1 for case in useful_cases if case["known_useful_in_pool"])
    known_useful_selected = sum(1 for case in useful_cases if case["known_useful_selected"])
    missing_due_to_retrieval = sum(
        1 for case in useful_cases if case["miss_class"] == "missing_due_to_retrieval"
    )
    missing_due_to_selection = sum(
        1 for case in useful_cases if case["miss_class"] == "missing_due_to_selection"
    )
    active_useful_cases = [case for case in cases if case["has_active_oracle_useful"]]
    active_oracle_useful_anchors = len(active_useful_cases)
    active_known_useful_in_pool = sum(
        1 for case in active_useful_cases if case["active_known_useful_in_pool"]
    )
    active_known_useful_selected = sum(
        1 for case in active_useful_cases if case["active_known_useful_selected"]
    )
    active_missing_due_to_retrieval = sum(
        1
        for case in active_useful_cases
        if case["active_miss_class"] == "missing_due_to_retrieval"
    )
    active_missing_due_to_selection = sum(
        1
        for case in active_useful_cases
        if case["active_miss_class"] == "missing_due_to_selection"
    )
    if active_oracle_useful_anchors:
        decision_retrieval_misses = active_missing_due_to_retrieval
        decision_selection_misses = active_missing_due_to_selection
        decision_scope = "active useful memories"
    else:
        decision_retrieval_misses = missing_due_to_retrieval
        decision_selection_misses = missing_due_to_selection
        decision_scope = "all oracle-useful memories"
    if decision_retrieval_misses > decision_selection_misses:
        decision = (
            f"Retrieval is the binding ceiling on this screening library for {decision_scope}: more known-useful misses are absent from the candidate pool than present-but-not-selected. Future recall-quality work should prioritize candidate generation before another selection/rerank variant."
        )
    elif decision_selection_misses > decision_retrieval_misses:
        decision = (
            f"Selection is the larger observed gap on this screening library for {decision_scope}: more known-useful misses are in the candidate pool but not selected. Future recall-quality work should prioritize selection/reranking before widening candidate generation."
        )
    else:
        decision = (
            f"Retrieval and selection misses are tied on this screening library for {decision_scope}. Future work should compare one candidate-generation variant and one selection variant before committing to either track."
        )
    summary = {
        "anchors": len(cases),
        "oracle_useful_anchors": oracle_useful_anchors,
        "known_useful_in_pool": known_useful_in_pool,
        "pool_recall_rate": (
            known_useful_in_pool / oracle_useful_anchors if oracle_useful_anchors else None
        ),
        "known_useful_selected": known_useful_selected,
        "known_useful_selected_rate": (
            known_useful_selected / oracle_useful_anchors if oracle_useful_anchors else None
        ),
        "missing_due_to_retrieval": missing_due_to_retrieval,
        "missing_due_to_selection": missing_due_to_selection,
        "active_oracle_useful_anchors": active_oracle_useful_anchors,
        "active_known_useful_in_pool": active_known_useful_in_pool,
        "active_pool_recall_rate": (
            active_known_useful_in_pool / active_oracle_useful_anchors
            if active_oracle_useful_anchors
            else None
        ),
        "active_known_useful_selected": active_known_useful_selected,
        "active_known_useful_selected_rate": (
            active_known_useful_selected / active_oracle_useful_anchors
            if active_oracle_useful_anchors
            else None
        ),
        "active_missing_due_to_retrieval": active_missing_due_to_retrieval,
        "active_missing_due_to_selection": active_missing_due_to_selection,
        "decision": decision,
    }
    manifest = {
        "date": args.label,
        "generated_by": "scripts/pool-recall-oracle.py",
        "anchors_file": str(anchors_file),
        "input_dir": str(input_dir),
        "strategy_label": args.strategy_label,
        "db": str(args.db.expanduser().resolve()),
        "summary": summary,
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    write_jsonl(out_dir / "cases.jsonl", cases)
    (out_dir / "REPORT.md").write_text(build_report(summary, manifest))
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
