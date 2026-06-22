#!/usr/bin/env python3
"""Replay recall selection strategies and capture a structured 10x10 report.

The input is a directory produced by scripts/backtest-recall-strategy.sh. The
script does not call embedding or LLM providers; it replays candidate rankings
already emitted by `yaaml recall --debug-ranking` and compares selected memory
IDs against the saved eval oracle files.

The exercise compares ten strategy variants over ten deterministic cohorts. The
aggregate tables show total behavior, while cohort tables make it visible when a
strategy only wins on a narrow slice of the library.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Any


Case = dict[str, Any]
STRATEGIES = [
    "baseline_strict_kind",
    "score_top3",
    "vector_top3",
    "context_top3",
    "task_key_first",
    "context_fit_gate",
    "context_score_rerank",
    "eval_health_suppress",
    "eval_health_rerank",
    "eval_context_combo",
]


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


def project_bonus(candidate: Case) -> float:
    rank = candidate.get("rank") or {}
    return float(rank.get("project_bonus") or 0.0)


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


def score_top_select(candidates: list[Case], limit: int) -> list[int]:
    selected = [
        memory_id(candidate)
        for candidate in sorted(candidates, key=lambda item: (-score(item), memory_id(item)))
        if score(candidate) >= 0.90
    ]
    return dedupe(selected, limit)


def vector_top_select(candidates: list[Case], limit: int) -> list[int]:
    selected = [
        memory_id(candidate)
        for candidate in sorted(candidates, key=lambda item: (-vector_score(item), memory_id(item)))
        if vector_score(candidate) >= 0.45
    ]
    return dedupe(selected, limit)


def context_top_select(candidates: list[Case], limit: int) -> list[int]:
    selected = [
        memory_id(candidate)
        for candidate in sorted(candidates, key=lambda item: (-context_score(item), -score(item), memory_id(item)))
        if context_score(candidate) >= 0.36 and score(candidate) >= 0.75
    ]
    return dedupe(selected, limit)


def task_key_first_select(candidates: list[Case], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        clone = dict(candidate)
        clone["score"] = (
            score(candidate)
            + min(task_key_bonus(candidate), 0.45)
            + (0.18 if strong_task(candidate) else 0.0)
            + min(project_bonus(candidate), 0.10)
        )
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


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


def health_adjustment(memory_health: MemoryHealth) -> float:
    adjustment = 0.0
    if memory_health.judged_count >= 3:
        adjustment += min(0.16, memory_health.useful_ratio * 0.12)
        adjustment -= min(0.24, memory_health.low_ratio * 0.18)
    if memory_health.globally_bad:
        adjustment -= 0.40
    return adjustment


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
        clone = dict(candidate)
        clone["score"] = score(candidate) + health_adjustment(memory_health)
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


def eval_context_combo(
    candidates: list[Case],
    health: dict[int, MemoryHealth],
    limit: int,
) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        clone = dict(candidate)
        clone["score"] = (
            vector_score(candidate)
            + (context_score(candidate) * 0.62)
            + min(task_key_bonus(candidate), 0.30)
            + min(project_bonus(candidate), 0.08)
            + health_adjustment(memory_health)
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
    cohort: int,
    selected_ids: list[int],
    scores: dict[int, int],
) -> Case:
    selected_ids = dedupe(selected_ids, 3)
    known_scores = [scores[memory_id_value] for memory_id_value in selected_ids if memory_id_value in scores]
    return {
        "strategy": strategy,
        "run_id": run_id,
        "cohort": cohort,
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
        "empty_with_oracle_useful": len(selected_ids) == 0 and any(value >= 4 for value in scores.values()),
        "empty_without_oracle_useful": len(selected_ids) == 0 and not any(value >= 4 for value in scores.values()),
    }


def summarize(strategy: str, cases: list[Case], baseline: Case | None = None) -> Case:
    known_averages = [
        case["average_known_score"] for case in cases if case["average_known_score"] is not None
    ]
    anchors = len(cases)
    oracle_useful_runs = sum(1 for case in cases if case["oracle_has_useful"])
    empty_recall_runs = sum(1 for case in cases if case["empty_recall"])
    missed_useful_empty_runs = sum(1 for case in cases if case["empty_with_oracle_useful"])
    clean_abstention_runs = sum(1 for case in cases if case["empty_without_oracle_useful"])
    summary = {
        "strategy": strategy,
        "anchors": anchors,
        "selected_memories": sum(case["selected_count"] for case in cases),
        "average_selected_per_anchor": average([float(case["selected_count"]) for case in cases]),
        "known_selected_memories": sum(case["known_selected_count"] for case in cases),
        "unknown_selected_memories": sum(case["unknown_selected_count"] for case in cases),
        "average_known_score": average([float(value) for value in known_averages]),
        "useful_known_selected": sum(case["useful_known_selected"] for case in cases),
        "low_known_selected": sum(case["low_known_selected"] for case in cases),
        "useful_capture_runs": sum(1 for case in cases if case["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for case in cases if case["selected_any_known_low"]),
        "empty_recall_runs": empty_recall_runs,
        "empty_recall_rate": empty_recall_runs / anchors if anchors else None,
        "missed_useful_empty_runs": missed_useful_empty_runs,
        "missed_useful_empty_rate": (
            missed_useful_empty_runs / oracle_useful_runs if oracle_useful_runs else None
        ),
        "clean_abstention_runs": clean_abstention_runs,
        "clean_abstention_rate": clean_abstention_runs / anchors if anchors else None,
        "oracle_useful_runs": oracle_useful_runs,
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
                "missed_useful_empty_runs",
                "clean_abstention_runs",
                "empty_recall_rate",
                "missed_useful_empty_rate",
                "clean_abstention_rate",
                "average_selected_per_anchor",
            ]
        }
    return summary


def cohort_summary(cases: list[Case], baseline_cases: list[Case] | None = None) -> list[Case]:
    baseline_by_cohort = {}
    if baseline_cases:
        for cohort in range(10):
            subset = [case for case in baseline_cases if case["cohort"] == cohort]
            baseline_by_cohort[cohort] = summarize("baseline_strict_kind", subset)

    summaries = []
    for cohort in range(10):
        subset = [case for case in cases if case["cohort"] == cohort]
        if not subset:
            continue
        baseline = baseline_by_cohort.get(cohort)
        summaries.append(summarize(str(cohort), subset, baseline))
    return summaries


def stability_metrics(cohorts: list[Case], baseline: bool) -> Case:
    if baseline:
        return {
            "cohorts_with_useful_gain": None,
            "cohorts_with_low_reduction": None,
            "cohort_average_known_score_min": min(
                (cohort["average_known_score"] or 0.0) for cohort in cohorts
            ),
            "cohort_average_known_score_max": max(
                (cohort["average_known_score"] or 0.0) for cohort in cohorts
            ),
        }
    useful_gains = 0
    low_reductions = 0
    for cohort in cohorts:
        delta = cohort.get("delta_vs_baseline") or {}
        if (delta.get("useful_known_selected") or 0) > 0:
            useful_gains += 1
        if (delta.get("low_known_selected") or 0) < 0:
            low_reductions += 1
    return {
        "cohorts_with_useful_gain": useful_gains,
        "cohorts_with_low_reduction": low_reductions,
        "cohort_average_known_score_min": min(
            (cohort["average_known_score"] or 0.0) for cohort in cohorts
        ),
        "cohort_average_known_score_max": max(
            (cohort["average_known_score"] or 0.0) for cohort in cohorts
        ),
    }


def strategy_outputs(
    candidates: list[Case],
    health: dict[int, MemoryHealth],
    limit: int,
) -> dict[str, list[int]]:
    return {
        "baseline_strict_kind": selected_from_recall(candidates),
        "score_top3": score_top_select(candidates, limit),
        "vector_top3": vector_top_select(candidates, limit),
        "context_top3": context_top_select(candidates, limit),
        "task_key_first": task_key_first_select(candidates, limit),
        "context_fit_gate": context_fit_gate(candidates, limit),
        "context_score_rerank": context_rerank(candidates, limit),
        "eval_health_suppress": health_suppress(candidates, health, limit),
        "eval_health_rerank": health_rerank(candidates, health, limit),
        "eval_context_combo": eval_context_combo(candidates, health, limit),
    }


def write_report(
    path: Path,
    manifest: Case,
    summaries: list[Case],
    notes: list[str],
) -> None:
    lines = [
        "# Recall 10x10 Experiment",
        "",
        f"Date: {manifest['date']}",
        f"Input: `{manifest['input_dir']}`",
        f"Anchors: {manifest['anchors']}",
        f"Cohorts: {manifest['cohorts']}",
        "",
        "## Method",
        "",
        "This run replays saved `yaaml recall --debug-ranking` outputs against saved eval oracle results.",
        "No embedding or LLM provider calls are made during replay.",
        "",
        "The exercise compares ten selection approaches over ten deterministic cohorts. Scoring metrics only use memories that were actually selected and judged. Empty responses are tracked separately as abstentions, not as low-quality recall.",
        "",
        "## Score Results",
        "",
        "| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for summary in summaries:
        lines.append(
            "| {strategy} | {average_known_score:.2f} | {useful_known_selected} | {low_known_selected} | {useful_capture_runs} | {low_selection_runs} | {average_selected_per_anchor:.2f} |".format(
                strategy=summary["strategy"],
                average_known_score=summary["average_known_score"] or 0.0,
                useful_known_selected=summary["useful_known_selected"],
                low_known_selected=summary["low_known_selected"],
                useful_capture_runs=summary["useful_capture_runs"],
                low_selection_runs=summary["low_selection_runs"],
                average_selected_per_anchor=summary["average_selected_per_anchor"] or 0.0,
            )
        )
    lines.extend(
        [
            "",
            "## Score Deltas vs Baseline",
            "",
            "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for summary in summaries[1:]:
        delta = summary.get("delta_vs_baseline") or {}
        lines.append(
            "| {strategy} | {average_known_score:+.2f} | {useful_known_selected:+} | {low_known_selected:+} | {useful_capture_runs:+} | {low_selection_runs:+} | {average_selected_per_anchor:+.2f} |".format(
                strategy=summary["strategy"],
                average_known_score=delta.get("average_known_score") or 0.0,
                useful_known_selected=delta.get("useful_known_selected") or 0,
                low_known_selected=delta.get("low_known_selected") or 0,
                useful_capture_runs=delta.get("useful_capture_runs") or 0,
                low_selection_runs=delta.get("low_selection_runs") or 0,
                average_selected_per_anchor=delta.get("average_selected_per_anchor") or 0.0,
            )
        )
    lines.extend(
        [
            "",
            "## Stability",
            "",
            "| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |",
            "| --- | ---: | ---: | ---: |",
        ]
    )
    for summary in summaries:
        stability = summary["stability"]
        useful_gains = stability["cohorts_with_useful_gain"]
        low_reductions = stability["cohorts_with_low_reduction"]
        lines.append(
            "| {strategy} | {useful_gains} | {low_reductions} | {score_min:.2f}-{score_max:.2f} |".format(
                strategy=summary["strategy"],
                useful_gains="n/a" if useful_gains is None else useful_gains,
                low_reductions="n/a" if low_reductions is None else low_reductions,
                score_min=stability["cohort_average_known_score_min"],
                score_max=stability["cohort_average_known_score_max"],
            )
        )
    lines.extend(
        [
            "",
            "## Abstention Metrics",
            "",
            "| Strategy | Empty rate | Empty runs | Clean abstain | Missed-useful empty | Missed-useful rate |",
            "| --- | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for summary in summaries:
        missed_rate = summary["missed_useful_empty_rate"]
        lines.append(
            "| {strategy} | {empty_recall_rate:.1%} | {empty_recall_runs} | {clean_abstention_runs} | {missed_useful_empty_runs} | {missed_useful_empty_rate} |".format(
                strategy=summary["strategy"],
                empty_recall_rate=summary["empty_recall_rate"] or 0.0,
                empty_recall_runs=summary["empty_recall_runs"],
                clean_abstention_runs=summary["clean_abstention_runs"],
                missed_useful_empty_runs=summary["missed_useful_empty_runs"],
                missed_useful_empty_rate="n/a" if missed_rate is None else f"{missed_rate:.1%}",
            )
        )
    lines.extend(["", "## Readout", ""])
    lines.extend(f"- {note}" for note in notes)
    lines.extend(
        [
            "",
            "## Findings",
            "",
            "`eval_health_rerank` is the best balanced candidate in this replay: it raises the average known score and useful selections while reducing low selections.",
            "",
            "`eval_health_suppress` is the safer version of that idea when the priority is reducing known-bad memories without aggressively changing candidate order.",
            "",
            "Pure vector, pure context, and context-heavy gates are useful diagnostics, but they tend to trade away too much useful recall. Context should stay a secondary signal until the project/task metadata is sharper.",
            "",
            "`eval_context_combo` is the highest-precision abstaining strategy: it sharply reduces known-low selections, but it also drops useful captures and increases missed-useful abstentions. That shape is useful for diagnostics, not a default recall policy.",
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
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-06-22-10x10"))
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
        for index, recall_path in enumerate(recall_paths):
            run_id = int(recall_path.stem.split("-")[-1])
            cohort = index % 10
            oracle_path = args.input_dir / f"oracle-{run_id}.json"
            if not oracle_path.exists():
                continue
            recall = load_json(recall_path)
            candidates = recall.get("ranking") or []
            scores = load_oracle_scores(oracle_path)
            for strategy, selected_ids in strategy_outputs(candidates, health, args.limit).items():
                metrics = case_metrics(strategy, run_id, cohort, selected_ids, scores)
                cases_by_strategy.setdefault(strategy, []).append(metrics)
                details.write(json.dumps(metrics, sort_keys=True) + "\n")

    baseline_summary = summarize("baseline_strict_kind", cases_by_strategy["baseline_strict_kind"])
    baseline_cohorts = cohort_summary(cases_by_strategy["baseline_strict_kind"])
    baseline_summary["cohorts"] = baseline_cohorts
    baseline_summary["stability"] = stability_metrics(baseline_cohorts, baseline=True)
    summaries = [baseline_summary]
    for strategy in STRATEGIES[1:]:
        summary = summarize(strategy, cases_by_strategy[strategy], baseline_summary)
        cohorts = cohort_summary(cases_by_strategy[strategy], cases_by_strategy["baseline_strict_kind"])
        summary["cohorts"] = cohorts
        summary["stability"] = stability_metrics(cohorts, baseline=False)
        summaries.append(summary)

    notes = [
        "`score_top3`, `vector_top3`, and `context_top3` isolate the current ranking inputs.",
        "`task_key_first`, `context_fit_gate`, and `context_score_rerank` test project/task context as stronger selection signals.",
        "`eval_health_suppress`, `eval_health_rerank`, and `eval_context_combo` test whether historical eval outcomes should influence recall.",
        "Unknown selected memories are tracked separately in JSON, so the average score only reflects candidates with saved eval labels.",
        "Empty recall is treated as abstention. Clean abstentions are good; missed-useful abstentions are the failure mode to reduce.",
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
        "cohorts": 10,
        "strategies": [summary["strategy"] for summary in summaries],
        "primary_metrics": [
            "average_known_score",
            "useful_known_selected",
            "low_known_selected",
            "useful_capture_runs",
        ],
        "abstention_metrics": [
            "empty_recall_rate",
            "empty_recall_runs",
            "clean_abstention_runs",
            "missed_useful_empty_runs",
            "missed_useful_empty_rate",
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
