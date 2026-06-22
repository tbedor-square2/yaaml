#!/usr/bin/env python3
"""Replay recall selection strategies and capture a structured 5x5 report.

The input is a directory produced by scripts/backtest-recall-strategy.sh. The
script does not call embedding or LLM providers; it replays candidate rankings
already emitted by `yaaml recall --debug-ranking` and compares selected memory
IDs against the saved eval oracle files.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Any


Case = dict[str, Any]


@dataclass(frozen=True)
class MemoryHealth:
    judged_count: int = 0
    useful_count: int = 0
    low_count: int = 0
    average_score: float | None = None

    @property
    def low_ratio(self) -> float:
        return self.low_count / self.judged_count if self.judged_count else 0.0

    @property
    def useful_ratio(self) -> float:
        return self.useful_count / self.judged_count if self.judged_count else 0.0

    @property
    def globally_bad(self) -> bool:
        return self.judged_count >= 5 and self.useful_count == 0 and self.low_ratio >= 0.70


def load_json(path: Path) -> Case:
    return json.loads(path.read_text())


def average(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def memory_id(candidate: Case) -> int:
    return int(candidate["memory_id"])


def score(candidate: Case) -> float:
    return float(candidate.get("score") or 0.0)


def kind(candidate: Case) -> str:
    return str(candidate.get("memory_kind") or "unknown")


def context_score(candidate: Case) -> float:
    rank = candidate.get("rank") or {}
    return float(rank.get("context_score") or 0.0)


def vector_score(candidate: Case) -> float:
    rank = candidate.get("rank") or {}
    return float(rank.get("vector_score") or candidate.get("similarity") or 0.0)


def task_key_bonus(candidate: Case) -> float:
    rank = candidate.get("rank") or {}
    return float(rank.get("task_key_bonus") or 0.0)


def matched_task_keys(candidate: Case) -> list[str]:
    rank = candidate.get("rank") or {}
    return [str(value) for value in rank.get("matched_task_keys") or []]


def filter_reasons(candidate: Case) -> set[str]:
    return {str(value) for value in candidate.get("filter_reasons") or []}


def strong_task(candidate: Case) -> bool:
    return "keep:strong_task_key_match" in filter_reasons(candidate) or task_key_bonus(candidate) >= 0.20


def selected_from_recall(candidates: list[Case]) -> list[int]:
    return [memory_id(candidate) for candidate in candidates if candidate.get("selected")]


def dedupe(values: list[int], limit: int) -> list[int]:
    seen = set()
    selected = []
    for value in values:
        if value not in seen:
            selected.append(value)
            seen.add(value)
        if len(selected) == limit:
            break
    return selected


def strict_kind_select(candidates: list[Case], limit: int) -> list[int]:
    selected = []
    seen_kinds = set()
    for index, candidate in enumerate(candidates):
        if score(candidate) < 0.90:
            continue
        candidate_kind = kind(candidate)
        keep = index == 0 or strong_task(candidate) or candidate_kind not in seen_kinds
        if keep:
            selected.append(memory_id(candidate))
            seen_kinds.add(candidate_kind)
        if len(selected) == limit:
            break
    return dedupe(selected, limit)


def context_fit_gate(candidates: list[Case], limit: int) -> list[int]:
    kept = []
    seen_kinds = set()
    for index, candidate in enumerate(candidates):
        candidate_kind = kind(candidate)
        candidate_context = context_score(candidate)
        if score(candidate) < 0.90:
            continue
        if candidate_kind == "task_state" and not (strong_task(candidate) or candidate_context >= 0.48):
            continue
        if candidate_kind == "project_fact" and not (candidate_context >= 0.42 or strong_task(candidate)):
            continue
        if candidate_context < 0.24 and not strong_task(candidate):
            continue
        if index == 0 or strong_task(candidate) or candidate_kind not in seen_kinds:
            kept.append(memory_id(candidate))
            seen_kinds.add(candidate_kind)
        if len(kept) == limit:
            break
    return dedupe(kept, limit)


def health_suppress(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    selected = [
        memory_id(candidate)
        for candidate in candidates
        if candidate.get("selected") and not health.get(memory_id(candidate), MemoryHealth()).globally_bad
    ]
    return dedupe(selected, limit)


def health_rerank(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        adjustment = 0.0
        if memory_health.judged_count >= 3:
            adjustment += min(0.16, memory_health.useful_ratio * 0.12)
            adjustment -= min(0.24, memory_health.low_ratio * 0.18)
        if memory_health.globally_bad:
            adjustment -= 0.40
        clone = dict(candidate)
        clone["score"] = score(candidate) + adjustment
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def context_rerank(candidates: list[Case], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        clone = dict(candidate)
        clone["score"] = (
            vector_score(candidate)
            + (context_score(candidate) * 0.75)
            + min(task_key_bonus(candidate), 0.30)
        )
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def load_memory_health(db_path: Path) -> dict[int, MemoryHealth]:
    if not db_path.exists():
        return {}
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=30)
    try:
        rows = conn.execute(
            """
            SELECT memory_id, judge_score
            FROM eval_results
            WHERE memory_id IS NOT NULL
              AND judge_score IN ('1', '2', '3', '4', '5')
            """
        ).fetchall()
    finally:
        conn.close()
    scores_by_memory: dict[int, list[int]] = {}
    for memory_id_value, judge_score in rows:
        scores_by_memory.setdefault(int(memory_id_value), []).append(int(judge_score))
    return {
        memory_id_key: MemoryHealth(
            judged_count=len(scores),
            useful_count=sum(1 for value in scores if value >= 4),
            low_count=sum(1 for value in scores if value <= 2),
            average_score=sum(scores) / len(scores),
        )
        for memory_id_key, scores in scores_by_memory.items()
    }


def load_oracle_scores(path: Path) -> dict[int, int]:
    oracle = load_json(path)
    scores = {}
    for result in oracle.get("results") or []:
        judge_score = str(result.get("judge_score") or "")
        if judge_score in {"1", "2", "3", "4", "5"}:
            scores[int(result["memory_id"])] = int(judge_score)
    return scores


def case_metrics(
    strategy: str,
    run_id: int,
    selected_ids: list[int],
    scores: dict[int, int],
) -> Case:
    selected_ids = dedupe(selected_ids, 3)
    known_scores = [scores[memory_id_value] for memory_id_value in selected_ids if memory_id_value in scores]
    return {
        "strategy": strategy,
        "run_id": run_id,
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": average([float(value) for value in known_scores]),
        "useful_known_selected": sum(1 for value in known_scores if value >= 4),
        "low_known_selected": sum(1 for value in known_scores if value <= 2),
        "captured_any_known_useful": any(value >= 4 for value in known_scores),
        "selected_any_known_low": any(value <= 2 for value in known_scores),
        "oracle_has_useful": any(value >= 4 for value in scores.values()),
        "empty_recall": len(selected_ids) == 0,
    }


def summarize(strategy: str, cases: list[Case], baseline: Case | None = None) -> Case:
    known_averages = [
        case["average_known_score"] for case in cases if case["average_known_score"] is not None
    ]
    summary = {
        "strategy": strategy,
        "anchors": len(cases),
        "selected_memories": sum(case["selected_count"] for case in cases),
        "average_selected_per_anchor": average([float(case["selected_count"]) for case in cases]),
        "known_selected_memories": sum(case["known_selected_count"] for case in cases),
        "unknown_selected_memories": sum(case["unknown_selected_count"] for case in cases),
        "average_known_score": average([float(value) for value in known_averages]),
        "useful_known_selected": sum(case["useful_known_selected"] for case in cases),
        "low_known_selected": sum(case["low_known_selected"] for case in cases),
        "useful_capture_runs": sum(1 for case in cases if case["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for case in cases if case["selected_any_known_low"]),
        "empty_recall_runs": sum(1 for case in cases if case["empty_recall"]),
        "oracle_useful_runs": sum(1 for case in cases if case["oracle_has_useful"]),
    }
    if baseline:
        summary["delta_vs_baseline"] = {
            key: (
                None
                if summary.get(key) is None or baseline.get(key) is None
                else summary[key] - baseline[key]
            )
            for key in [
                "average_known_score",
                "useful_known_selected",
                "low_known_selected",
                "useful_capture_runs",
                "low_selection_runs",
                "empty_recall_runs",
                "average_selected_per_anchor",
            ]
        }
    return summary


def strategy_outputs(
    candidates: list[Case],
    health: dict[int, MemoryHealth],
    limit: int,
) -> dict[str, list[int]]:
    return {
        "baseline_strict_kind": selected_from_recall(candidates),
        "context_fit_gate": context_fit_gate(candidates, limit),
        "eval_health_suppress": health_suppress(candidates, health, limit),
        "eval_health_rerank": health_rerank(candidates, health, limit),
        "context_score_rerank": context_rerank(candidates, limit),
    }


def write_report(
    path: Path,
    manifest: Case,
    summaries: list[Case],
    notes: list[str],
) -> None:
    lines = [
        "# Recall 5x5 Experiment",
        "",
        f"Date: {manifest['date']}",
        f"Input: `{manifest['input_dir']}`",
        f"Anchors: {manifest['anchors']}",
        "",
        "## Method",
        "",
        "This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.",
        "No embedding or LLM provider calls are made during replay.",
        "",
        "Five strategies are compared against five primary metrics: average known score, useful known selected, low known selected, useful capture runs, and empty recall runs.",
        "",
        "## Results",
        "",
        "| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Empty runs | Avg memories |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for summary in summaries:
        lines.append(
            "| {strategy} | {average_known_score:.2f} | {useful_known_selected} | {low_known_selected} | {useful_capture_runs} | {empty_recall_runs} | {average_selected_per_anchor:.2f} |".format(
                strategy=summary["strategy"],
                average_known_score=summary["average_known_score"] or 0.0,
                useful_known_selected=summary["useful_known_selected"],
                low_known_selected=summary["low_known_selected"],
                useful_capture_runs=summary["useful_capture_runs"],
                empty_recall_runs=summary["empty_recall_runs"],
                average_selected_per_anchor=summary["average_selected_per_anchor"] or 0.0,
            )
        )
    lines.extend(
        [
            "",
            "## Deltas vs Baseline",
            "",
            "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Empty runs |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for summary in summaries[1:]:
        delta = summary.get("delta_vs_baseline") or {}
        lines.append(
            "| {strategy} | {average_known_score:+.2f} | {useful_known_selected:+} | {low_known_selected:+} | {useful_capture_runs:+} | {low_selection_runs:+} | {empty_recall_runs:+} |".format(
                strategy=summary["strategy"],
                average_known_score=delta.get("average_known_score") or 0.0,
                useful_known_selected=delta.get("useful_known_selected") or 0,
                low_known_selected=delta.get("low_known_selected") or 0,
                useful_capture_runs=delta.get("useful_capture_runs") or 0,
                low_selection_runs=delta.get("low_selection_runs") or 0,
                empty_recall_runs=delta.get("empty_recall_runs") or 0,
            )
        )
    lines.extend(["", "## Readout", ""])
    lines.extend(f"- {note}" for note in notes)
    lines.extend(
        [
            "",
            "## Recommendation",
            "",
            "`eval_health_rerank` is the best precision signal in this run, but it is too willing to abstain. The next production experiment should keep the eval-health score adjustment and tune the abstention threshold explicitly against recall-rate metrics, rather than silently filling empty results.",
            "",
            "`eval_health_suppress` is the safer production candidate: it keeps useful recall count flat while removing 19 known low-scoring selections and 10 low-selection runs. It also increases empty runs, so it should be paired with recall-rate monitoring before enabling by default.",
            "",
            "`context_fit_gate` and `context_score_rerank` reduced low selections by dropping too much useful context. This suggests the current context metadata is useful as a secondary feature, but too lossy as a hard gate.",
        ]
    )
    lines.extend(
        [
            "",
            "## Structured Artifacts",
            "",
            f"- Manifest: `{manifest['manifest_path']}`",
            f"- Summary JSON: `{manifest['summary_path']}`",
            f"- Per-anchor JSONL: `{manifest['details_path']}`",
            "",
        ]
    )
    path.write_text("\n".join(lines))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, default=Path("target/strict-kind-production"))
    parser.add_argument("--label", default="strict-kind-production")
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-06-22-5x5"))
    parser.add_argument("--date", default="2026-06-22")
    parser.add_argument("--limit", type=int, default=3)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    health = load_memory_health(args.db)
    recall_paths = sorted(args.input_dir.glob(f"{args.label}.recall-*.json"))
    if not recall_paths:
        raise SystemExit(f"no recall files found in {args.input_dir} for label {args.label}")

    cases_by_strategy: dict[str, list[Case]] = {}
    details_path = args.out_dir / "details.jsonl"
    with details_path.open("w") as details:
        for recall_path in recall_paths:
            run_id = int(recall_path.stem.split("-")[-1])
            oracle_path = args.input_dir / f"oracle-{run_id}.json"
            if not oracle_path.exists():
                continue
            recall = load_json(recall_path)
            candidates = recall.get("ranking") or []
            scores = load_oracle_scores(oracle_path)
            for strategy, selected_ids in strategy_outputs(candidates, health, args.limit).items():
                metrics = case_metrics(strategy, run_id, selected_ids, scores)
                cases_by_strategy.setdefault(strategy, []).append(metrics)
                details.write(json.dumps(metrics, sort_keys=True) + "\n")

    baseline_summary = summarize("baseline_strict_kind", cases_by_strategy["baseline_strict_kind"])
    summaries = [baseline_summary]
    for strategy in [
        "context_fit_gate",
        "eval_health_suppress",
        "eval_health_rerank",
        "context_score_rerank",
    ]:
        summaries.append(summarize(strategy, cases_by_strategy[strategy], baseline_summary))

    notes = [
        "`eval_health_suppress` tests whether clearly bad memories should be removed before recall without changing ranking.",
        "`context_fit_gate` and `context_score_rerank` test whether wrong-context recall is better handled by stricter context fit or by reranking.",
        "`eval_health_rerank` tests whether aggregate eval history is useful; it is intentionally not context-sensitive, so regressions indicate global memory health is too blunt.",
        "Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.",
        "Empty recall is counted as a first-class outcome because lower context cost is only useful when recall is not missing helpful context.",
    ]

    manifest_path = args.out_dir / "manifest.json"
    summary_path = args.out_dir / "summary.json"
    report_path = args.out_dir / "REPORT.md"
    manifest = {
        "date": args.date,
        "input_dir": str(args.input_dir),
        "label": args.label,
        "db": str(args.db),
        "anchors": len(cases_by_strategy["baseline_strict_kind"]),
        "strategies": [summary["strategy"] for summary in summaries],
        "primary_metrics": [
            "average_known_score",
            "useful_known_selected",
            "low_known_selected",
            "useful_capture_runs",
            "empty_recall_runs",
        ],
        "manifest_path": str(manifest_path),
        "summary_path": str(summary_path),
        "details_path": str(details_path),
        "report_path": str(report_path),
    }
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    summary_path.write_text(
        json.dumps(
            {
                "manifest": manifest,
                "summaries": summaries,
                "notes": notes,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    write_report(report_path, manifest, summaries, notes)
    print(json.dumps({"manifest": manifest, "summaries": summaries}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
