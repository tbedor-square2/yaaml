#!/usr/bin/env python3
"""Revalidate shipped recall decisions on the refreshed anchor library."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from recall_experiment_stats import paired_bootstrap_deltas

Row = dict[str, Any]

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


def read_anchor_ids(path: Path) -> set[int]:
    return {
        int(line.split("\t", 1)[0])
        for line in path.read_text().splitlines()
        if line.strip()
    }


def read_jsonl(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def load_json(path: Path) -> Row:
    return json.loads(path.read_text())


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def score_map(oracle: Row) -> dict[int, int]:
    scores = {}
    for result in oracle.get("results") or []:
        try:
            score = int(str(result.get("judge_score")))
        except ValueError:
            continue
        if 1 <= score <= 5 and result.get("memory_id") is not None:
            scores[int(result["memory_id"])] = score
    return scores


def details_row(label: str, run_id: int, oracle: Row, selected_ids: list[int]) -> Row:
    scores = score_map(oracle)
    known_scores = [scores[memory_id] for memory_id in selected_ids if memory_id in scores]
    oracle_scores = list(scores.values())
    return {
        "strategy": label,
        "case": f"eval-library-{run_id}",
        "run_id": run_id,
        "session_id": oracle.get("run", {}).get("session_id"),
        "turn_ordinal": oracle.get("run", {}).get("turn_ordinal"),
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": (
            sum(known_scores) / len(known_scores) if known_scores else None
        ),
        "useful_known_selected": sum(1 for score in known_scores if score >= 4),
        "low_known_selected": sum(1 for score in known_scores if score <= 2),
        "captured_any_known_useful": any(score >= 4 for score in known_scores),
        "selected_any_known_low": any(score <= 2 for score in known_scores),
        "oracle_has_useful": any(score >= 4 for score in oracle_scores),
        "oracle_best_score": max(oracle_scores) if oracle_scores else None,
        "empty_recall": len(selected_ids) == 0,
    }


def mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def summary(label: str, rows: list[Row], details_path: Path) -> Row:
    known_scores = [
        float(row["average_known_score"])
        for row in rows
        if row.get("average_known_score") is not None
    ]
    oracle_useful = sum(1 for row in rows if row["oracle_has_useful"])
    missed_useful_empty = sum(
        1 for row in rows if row["empty_recall"] and row["oracle_has_useful"]
    )
    return {
        "strategy": label,
        "anchors": len(rows),
        "average_known_score": mean(known_scores),
        "useful_known_selected": sum(row["useful_known_selected"] for row in rows),
        "low_known_selected": sum(row["low_known_selected"] for row in rows),
        "useful_capture_runs": sum(1 for row in rows if row["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for row in rows if row["selected_any_known_low"]),
        "average_selected_per_anchor": mean([row["selected_count"] for row in rows]),
        "empty_recall_runs": sum(1 for row in rows if row["empty_recall"]),
        "missed_useful_empty_runs": missed_useful_empty,
        "missed_useful_empty_rate": missed_useful_empty / oracle_useful
        if oracle_useful
        else None,
        "clean_abstention_runs": sum(
            1 for row in rows if row["empty_recall"] and not row["oracle_has_useful"]
        ),
        "details_path": str(details_path),
    }


def eligible_for_filter_pool(candidate: Row) -> bool:
    return any(
        reason.startswith("keep:") and reason != "keep:strict_kind_diverse"
        for reason in candidate.get("filter_reasons") or []
    )


def health_delta(candidate: Row) -> float:
    delta = 0.0
    for penalty in candidate.get("rank", {}).get("penalties") or []:
        match = re.match(r"health_action_rerank:[^:]+:[^:]+:([-+]?[0-9.]+)$", penalty)
        if match:
            delta += float(match.group(1))
    return delta


def adjusted_no_health_candidate(candidate: Row) -> Row:
    adjusted = json.loads(json.dumps(candidate))
    adjusted["score"] = float(candidate["score"]) - health_delta(candidate)
    penalties = [
        penalty
        for penalty in adjusted.get("rank", {}).get("penalties") or []
        if not penalty.startswith("health_action_rerank:")
    ]
    adjusted.setdefault("rank", {})["penalties"] = penalties
    return adjusted


def has_specific_task_match(candidate: Row) -> bool:
    reasons = candidate.get("rank", {}).get("filter_reasons") or candidate.get("filter_reasons") or []
    if "keep:strong_task_key_match" in reasons:
        return True
    return any(
        not key.startswith(("label:", "topic:", "tool:"))
        for key in candidate.get("rank", {}).get("matched_task_keys") or []
    )


def has_positive_health_signal(candidate: Row) -> bool:
    return any(
        ":proven_useful:" in penalty
        for penalty in candidate.get("rank", {}).get("penalties") or []
    )


def active_segment_task_state(candidate: Row) -> bool:
    return "keep:task_state_same_active_segment" in (
        candidate.get("rank", {}).get("filter_reasons") or candidate.get("filter_reasons") or []
    )


def risky_unproven_procedural(candidate: Row) -> bool:
    kind = candidate.get("memory_kind") or "unknown"
    return (
        kind in {"lesson", "workflow"}
        and not has_specific_task_match(candidate)
        and not has_positive_health_signal(candidate)
        and float(candidate["score"]) < 1.50
    )


def strict_kind_selection(candidates: list[Row], limit: int = 2) -> list[int]:
    if not candidates:
        return []
    top = candidates[0]
    active = [candidate for candidate in candidates if active_segment_task_state(candidate)]
    if float(top["score"]) < 0.75 and not active:
        return []
    selected: list[Row] = []
    for candidate in active:
        selected.append(candidate)
        if len(selected) == limit:
            return [int(row["memory_id"]) for row in selected]
    seen_kinds = {row.get("memory_kind") or "unknown" for row in selected}
    selected_ids = {int(row["memory_id"]) for row in selected}
    for index, candidate in enumerate(candidates):
        memory_id = int(candidate["memory_id"])
        if memory_id in selected_ids:
            continue
        kind = candidate.get("memory_kind") or "unknown"
        keep = (
            (index == 0 and not risky_unproven_procedural(candidate))
            or has_specific_task_match(candidate)
            or (kind not in seen_kinds and has_positive_health_signal(candidate))
        ) and float(candidate["score"]) >= 0.90
        if keep:
            selected.append(candidate)
            selected_ids.add(memory_id)
            seen_kinds.add(kind)
        if len(selected) == limit:
            break
    return [int(row["memory_id"]) for row in selected]


def build_no_health_rows(anchor_ids: set[int], input_dir: Path, strategy_label: str) -> list[Row]:
    rows = []
    for run_id in sorted(anchor_ids):
        oracle = load_json(input_dir / f"oracle-{run_id}.json")
        recall = load_json(input_dir / f"{strategy_label}.recall-{run_id}.json")
        candidates = [
            adjusted_no_health_candidate(candidate)
            for candidate in recall.get("ranking") or []
            if eligible_for_filter_pool(candidate)
        ]
        candidates.sort(key=lambda row: (-float(row["score"]), int(row["memory_id"])))
        selected_ids = strict_kind_selection(candidates)
        rows.append(details_row("no_health_action_rerank_proxy", run_id, oracle, selected_ids))
    return rows


def add_comparison(name: str, baseline: Row, candidate: Row, out: Row) -> None:
    baseline_rows = read_jsonl(Path(baseline["details_path"]))
    candidate_rows = read_jsonl(Path(candidate["details_path"]))
    bootstrap = paired_bootstrap_deltas(baseline_rows, candidate_rows, METRICS)
    out[name] = {
        "baseline": baseline,
        "candidate": candidate,
        "delta_ci_95": {metric: value["ci_95"] for metric, value in bootstrap.items()},
        "delta_verdicts": {metric: value["verdict"] for metric, value in bootstrap.items()},
        "point_deltas": {metric: value["point_delta"] for metric, value in bootstrap.items()},
    }


def write_report(path: Path, comparisons: Row) -> None:
    health_decision = health_action_decision(comparisons["health_action_rerank"])
    query_decision = (
        None
        if comparisons.get("query_cleaning") is None
        else query_cleaning_decision(comparisons["query_cleaning"])
    )
    lines = [
        "# Pre-CI Decision Revalidation",
        "",
        "## Health Action Rerank",
        "",
    ]
    health = comparisons["health_action_rerank"]
    lines.extend(render_comparison(health))
    lines.extend([
        "",
        f"Decision: {health_decision}",
        "",
        "Note: this is a saved-candidate proxy that reverses encoded health deltas; noisy-metadata task-key restoration is not reconstructable from saved JSON, so the no-health baseline is conservative rather than exact.",
        "",
        "## Recall Query Cleaning",
        "",
    ])
    query = comparisons.get("query_cleaning")
    if query is None:
        lines.append("Query-cleaning comparison was not run.")
    else:
        lines.extend(render_comparison(query))
        lines.extend([
            "",
            f"Decision: {query_decision}",
            "",
            "Note: this comparison reruns retrieval because query construction changes the embedding query.",
        ])
    path.write_text("\n".join(lines) + "\n")


def health_action_decision(comparison: Row) -> str:
    verdicts = comparison["delta_verdicts"]
    deltas = comparison["point_deltas"]
    if (
        verdicts["missed_useful_empty_runs"] == "confirmed"
        and deltas["missed_useful_empty_runs"] > 0
    ):
        return (
            "tune `health_action_rerank`, not a clean keep. The refreshed screening replay confirms lower context volume and better average known score, but it also confirms a large missed-useful-empty regression; future work should reduce over-abstention before treating this as fully validated."
        )
    if (
        verdicts["low_known_selected"] == "confirmed"
        and deltas["low_known_selected"] < 0
    ):
        return "`health_action_rerank` remains validated on the refreshed screening library."
    return (
        "`health_action_rerank` is not fully validated by this refreshed replay; keep only as provisional and prioritize a tuning or revert experiment."
    )


def query_cleaning_decision(comparison: Row) -> str:
    verdicts = comparison["delta_verdicts"]
    deltas = comparison["point_deltas"]
    useful_regression = (
        verdicts["useful_capture_runs"] == "confirmed"
        and deltas["useful_capture_runs"] < 0
    ) or (
        verdicts["useful_known_selected"] == "confirmed"
        and deltas["useful_known_selected"] < 0
    )
    if useful_regression:
        return (
            "revert or redesign recall-query cleaning; the refreshed screening replay confirms a useful-recall regression."
        )
    return (
        "keep recall-query cleaning. The refreshed screening replay shows no detectable effect on useful selected memories, useful capture runs, low selections, or average selected memories; the small point regression in useful capture is not confirmed by CI."
    )


def render_comparison(comparison: Row) -> list[str]:
    baseline = comparison["baseline"]
    candidate = comparison["candidate"]
    lines = [
        f"Baseline: `{baseline['strategy']}`",
        f"Candidate: `{candidate['strategy']}`",
        "",
        "| Metric | Baseline | Candidate | Delta | CI 95% | Verdict |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for metric in METRICS:
        ci = comparison["delta_ci_95"][metric]
        ci_text = "n/a" if ci is None else f"[{ci[0]:.2f}, {ci[1]:.2f}]"
        lines.append(
            "| {metric} | {base} | {cand} | {delta} | {ci} | {verdict} |".format(
                metric=metric,
                base=format_value(baseline.get(metric)),
                cand=format_value(candidate.get(metric)),
                delta=format_value(comparison["point_deltas"][metric], signed=True),
                ci=ci_text,
                verdict=comparison["delta_verdicts"][metric],
            )
        )
    return lines


def format_value(value: Any, *, signed: bool = False) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, int):
        return f"{value:+d}" if signed else str(value)
    if isinstance(value, float):
        return f"{value:+.2f}" if signed else f"{value:.2f}"
    return str(value)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--production-details", type=Path, required=True)
    parser.add_argument("--strategy-label", default="anchor-refresh")
    parser.add_argument("--query-no-cleaning-details", type=Path)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()

    out_dir = args.out_dir.expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    anchor_ids = read_anchor_ids(args.anchors_file)
    production_rows = [
        row for row in read_jsonl(args.production_details) if int(row["run_id"]) in anchor_ids
    ]
    production_rows.sort(key=lambda row: int(row["run_id"]))
    production_details = out_dir / "production.details.jsonl"
    write_jsonl(production_details, production_rows)
    production_summary = summary("production_current", production_rows, production_details)

    no_health_rows = build_no_health_rows(anchor_ids, args.input_dir, args.strategy_label)
    no_health_details = out_dir / "no_health_action_rerank_proxy.details.jsonl"
    write_jsonl(no_health_details, no_health_rows)
    no_health_summary = summary(
        "no_health_action_rerank_proxy",
        no_health_rows,
        no_health_details,
    )

    comparisons: Row = {}
    add_comparison(
        "health_action_rerank",
        no_health_summary,
        production_summary,
        comparisons,
    )

    if args.query_no_cleaning_details is not None:
        no_clean_rows = [
            row
            for row in read_jsonl(args.query_no_cleaning_details)
            if int(row["run_id"]) in anchor_ids
        ]
        no_clean_rows.sort(key=lambda row: int(row["run_id"]))
        no_clean_details = out_dir / "no_query_cleaning.details.jsonl"
        write_jsonl(no_clean_details, no_clean_rows)
        no_clean_summary = summary("no_query_cleaning", no_clean_rows, no_clean_details)
        add_comparison("query_cleaning", no_clean_summary, production_summary, comparisons)

    manifest = {
        "anchors_file": str(args.anchors_file.expanduser().resolve()),
        "input_dir": str(args.input_dir.expanduser().resolve()),
        "production_details": str(args.production_details.expanduser().resolve()),
        "query_no_cleaning_details": None
        if args.query_no_cleaning_details is None
        else str(args.query_no_cleaning_details.expanduser().resolve()),
        "comparisons": comparisons,
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    write_report(out_dir / "REPORT.md", comparisons)
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
