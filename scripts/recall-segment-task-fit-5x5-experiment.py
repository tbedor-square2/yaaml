#!/usr/bin/env python3
"""Run a 5x5 segment/task-fit recall replay experiment.

This replays saved `yaaml recall --debug-ranking` candidates against saved eval
oracle labels. It does not call embedding or LLM providers. The strategies here
are proxies over already retrieved candidates, so they test selection/reranking
behavior rather than candidate-generation changes.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any, Callable


Case = dict[str, Any]
StrategyFn = Callable[[list[Case], dict[int, Any], int], list[int]]
DURABLE_KINDS = {"preference", "lesson", "workflow"}
SEGMENT_SPECIFIC_KINDS = {"task_state", "project_fact"}


def load_health_module() -> Any:
    path = Path(__file__).with_name("recall-health-10x10-experiment.py")
    spec = importlib.util.spec_from_file_location("recall_health_10x10_experiment", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed to load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


H = load_health_module()


def load_json(path: Path) -> Case:
    return json.loads(path.read_text())


def average(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def memory_id(candidate: Case) -> int:
    return H.memory_id(candidate)


def score(candidate: Case) -> float:
    return H.score(candidate)


def kind(candidate: Case) -> str:
    return H.kind(candidate)


def vector_score(candidate: Case) -> float:
    return H.vector_score(candidate)


def context_score(candidate: Case) -> float:
    return H.context_score(candidate)


def task_key_bonus(candidate: Case) -> float:
    return H.task_key_bonus(candidate)


def project_bonus(candidate: Case) -> float:
    return H.project_bonus(candidate)


def strong_task(candidate: Case) -> bool:
    return H.strong_task(candidate)


def weak_task(candidate: Case) -> bool:
    return H.weak_or_strong_task(candidate)


def matched_task_keys(candidate: Case) -> list[str]:
    return H.matched_task_keys(candidate)


def filter_reasons(candidate: Case) -> set[str]:
    rank = candidate.get("rank") or {}
    return {str(value) for value in candidate.get("filter_reasons") or rank.get("filter_reasons") or []}


def has_segment_evidence(candidate: Case) -> bool:
    return strong_task(candidate) or context_score(candidate) >= 0.52


def health_delta(candidate: Case, health: dict[int, Any]) -> tuple[float, bool]:
    memory_health = health.get(memory_id(candidate), H.MemoryHealth())
    mode_delta, clear_task_bonus = H.health_mode_adjustment(candidate, memory_health)
    return H.health_adjustment(memory_health) + mode_delta, clear_task_bonus


def adjusted_candidates(candidates: list[Case], health: dict[int, Any]) -> list[Case]:
    adjusted = []
    for candidate in candidates:
        delta, clear_task_bonus = health_delta(candidate, health)
        adjusted.append(H.clone_candidate(candidate, delta, clear_task_bonus=clear_task_bonus))
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return adjusted


def score_adjusted_candidates(
    candidates: list[Case],
    health: dict[int, Any],
    scorer: Callable[[Case], float],
) -> list[Case]:
    adjusted = []
    for candidate in candidates:
        delta, clear_task_bonus = health_delta(candidate, health)
        clone = H.clone_candidate(candidate, clear_task_bonus=clear_task_bonus)
        clone["score"] = scorer(clone) + delta
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return adjusted


def select_production(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    return H.strict_kind_select(adjusted_candidates(candidates, health), limit)


def select_segment_context_rerank(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    def scorer(candidate: Case) -> float:
        return (
            vector_score(candidate)
            + context_score(candidate) * 1.45
            + min(task_key_bonus(candidate) * 1.25, 0.45)
            + min(project_bonus(candidate), 0.08)
        )

    return H.strict_kind_select(score_adjusted_candidates(candidates, health, scorer), limit)


def select_task_fit_required(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = []
    for candidate in candidates:
        candidate_kind = kind(candidate)
        if candidate_kind in SEGMENT_SPECIFIC_KINDS and not (
            weak_task(candidate) or context_score(candidate) >= 0.56
        ):
            continue
        kept.append(candidate)
    return select_production(kept, health, limit)


def select_wrong_context_penalty(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), H.MemoryHealth())
        penalty = 0.0
        reasons = filter_reasons(candidate)
        if "drop:project_fact_wrong_context" in reasons:
            penalty -= 0.45
        if "same_project_no_task_key_overlap" in set((candidate.get("rank") or {}).get("penalties") or []):
            penalty -= 0.18
        if memory_health.failure_mode in {"wrong_context", "context_sensitive", "mixed_performance"} and not (
            weak_task(candidate) or context_score(candidate) >= 0.56
        ):
            penalty -= 0.34
        delta, clear_task_bonus = health_delta(candidate, health)
        adjusted.append(H.clone_candidate(candidate, delta + penalty, clear_task_bonus=clear_task_bonus))
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return H.strict_kind_select(adjusted, limit)


def select_segment_evidence_gate(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = []
    for candidate in adjusted_candidates(candidates, health):
        candidate_kind = kind(candidate)
        if candidate_kind in SEGMENT_SPECIFIC_KINDS:
            if strong_task(candidate) or context_score(candidate) >= 0.58:
                kept.append(candidate)
            continue
        if (
            score(candidate) >= 1.05
            or has_segment_evidence(candidate)
            or candidate_kind in DURABLE_KINDS and context_score(candidate) >= 0.38
        ):
            kept.append(candidate)
    return H.strict_kind_select(kept, limit)


def select_segment_context_top2(candidates: list[Case], health: dict[int, Any], _limit: int) -> list[int]:
    return select_segment_context_rerank(candidates, health, 2)


def strategy_definitions() -> list[tuple[str, StrategyFn]]:
    return [
        ("production_health_action", select_production),
        ("segment_context_rerank", select_segment_context_rerank),
        ("task_fit_required", select_task_fit_required),
        ("wrong_context_penalty", select_wrong_context_penalty),
        ("segment_evidence_gate", select_segment_evidence_gate),
        ("segment_context_top2", select_segment_context_top2),
    ]


def rationale_mentions_wrong_context(rationale: str) -> bool:
    return H.rationale_mentions_wrong_context(rationale.lower())


def rationale_mentions_stale_task(rationale: str) -> bool:
    text = rationale.lower()
    return any(
        needle in text
        for needle in [
            "stale",
            "obsolete",
            "old task",
            "old pr",
            "no longer",
            "already completed",
            "conversation moved",
            "moved on",
            "different stage",
            "current task",
        ]
    )


def load_oracle(path: Path) -> dict[int, Case]:
    oracle = load_json(path)
    results = {}
    for result in oracle.get("results") or []:
        judge_score = str(result.get("judge_score") or "")
        if judge_score in {"1", "2", "3", "4", "5"}:
            rationale = str(result.get("rationale") or "")
            results[int(result["memory_id"])] = {
                "score": int(judge_score),
                "rationale": rationale,
                "wrong_context": rationale_mentions_wrong_context(rationale),
                "stale_task": rationale_mentions_stale_task(rationale),
            }
    return results


def case_metrics(
    strategy: str,
    run_id: int,
    cohort: int,
    selected_ids: list[int],
    oracle: dict[int, Case],
) -> Case:
    selected_ids = H.dedupe(selected_ids, 3)
    known_results = [oracle[memory_id_value] for memory_id_value in selected_ids if memory_id_value in oracle]
    known_scores = [int(result["score"]) for result in known_results]
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
        "wrong_context_low_selected": sum(
            1 for result in known_results if result["score"] <= 2 and result["wrong_context"]
        ),
        "stale_task_low_selected": sum(
            1 for result in known_results if result["score"] <= 2 and result["stale_task"]
        ),
        "captured_any_known_useful": any(value >= 4 for value in known_scores),
        "selected_any_known_low": any(value <= 2 for value in known_scores),
        "oracle_has_useful": any(int(result["score"]) >= 4 for result in oracle.values()),
        "empty_recall": len(selected_ids) == 0,
        "empty_with_oracle_useful": len(selected_ids) == 0
        and any(int(result["score"]) >= 4 for result in oracle.values()),
        "empty_without_oracle_useful": len(selected_ids) == 0
        and not any(int(result["score"]) >= 4 for result in oracle.values()),
    }


def summarize(strategy: str, cases: list[Case], baseline: Case | None = None) -> Case:
    known_averages = [
        case["average_known_score"] for case in cases if case["average_known_score"] is not None
    ]
    anchors = len(cases)
    oracle_useful_runs = sum(1 for case in cases if case["oracle_has_useful"])
    empty_recall_runs = sum(1 for case in cases if case["empty_recall"])
    missed_useful_empty_runs = sum(1 for case in cases if case["empty_with_oracle_useful"])
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
        "wrong_context_low_selected": sum(case["wrong_context_low_selected"] for case in cases),
        "stale_task_low_selected": sum(case["stale_task_low_selected"] for case in cases),
        "useful_capture_runs": sum(1 for case in cases if case["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for case in cases if case["selected_any_known_low"]),
        "empty_recall_runs": empty_recall_runs,
        "empty_recall_rate": empty_recall_runs / anchors if anchors else None,
        "missed_useful_empty_runs": missed_useful_empty_runs,
        "missed_useful_empty_rate": (
            missed_useful_empty_runs / oracle_useful_runs if oracle_useful_runs else None
        ),
        "clean_abstention_runs": sum(1 for case in cases if case["empty_without_oracle_useful"]),
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
                "wrong_context_low_selected",
                "stale_task_low_selected",
                "useful_capture_runs",
                "low_selection_runs",
                "empty_recall_runs",
                "missed_useful_empty_runs",
                "average_selected_per_anchor",
            ]
        }
    return summary


def cohort_summaries(cases: list[Case], baseline_cases: list[Case] | None = None) -> list[Case]:
    summaries = []
    for cohort in range(5):
        subset = [case for case in cases if case["cohort"] == cohort]
        if baseline_cases is None:
            summaries.append(summarize(str(cohort), subset))
        else:
            baseline_subset = [case for case in baseline_cases if case["cohort"] == cohort]
            summaries.append(summarize(str(cohort), subset, summarize("baseline", baseline_subset)))
    return summaries


def stability_metrics(cohorts: list[Case], baseline: bool) -> Case:
    if baseline:
        return {
            "cohorts_with_useful_gain": None,
            "cohorts_with_low_reduction": None,
            "cohorts_with_wrong_context_reduction": None,
            "cohort_average_known_score_min": min((cohort["average_known_score"] or 0.0) for cohort in cohorts),
            "cohort_average_known_score_max": max((cohort["average_known_score"] or 0.0) for cohort in cohorts),
        }
    return {
        "cohorts_with_useful_gain": sum(
            1 for cohort in cohorts if (cohort.get("delta_vs_baseline") or {}).get("useful_known_selected", 0) > 0
        ),
        "cohorts_with_low_reduction": sum(
            1 for cohort in cohorts if (cohort.get("delta_vs_baseline") or {}).get("low_known_selected", 0) < 0
        ),
        "cohorts_with_wrong_context_reduction": sum(
            1
            for cohort in cohorts
            if (cohort.get("delta_vs_baseline") or {}).get("wrong_context_low_selected", 0) < 0
        ),
        "cohort_average_known_score_min": min((cohort["average_known_score"] or 0.0) for cohort in cohorts),
        "cohort_average_known_score_max": max((cohort["average_known_score"] or 0.0) for cohort in cohorts),
    }


def summarize_all(cases_by_strategy: dict[str, list[Case]]) -> list[Case]:
    baseline_name = "production_health_action"
    baseline = summarize(baseline_name, cases_by_strategy[baseline_name])
    baseline["cohorts"] = cohort_summaries(cases_by_strategy[baseline_name])
    baseline["stability"] = stability_metrics(baseline["cohorts"], baseline=True)
    summaries = [baseline]
    for strategy_name, _strategy in strategy_definitions()[1:]:
        summary = summarize(strategy_name, cases_by_strategy[strategy_name], baseline)
        summary["cohorts"] = cohort_summaries(
            cases_by_strategy[strategy_name],
            cases_by_strategy[baseline_name],
        )
        summary["stability"] = stability_metrics(summary["cohorts"], baseline=False)
        summaries.append(summary)
    return summaries


def format_float(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.2f}"


def format_rate(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.1%}"


def write_report(path: Path, manifest: Case, summaries: list[Case]) -> None:
    baseline = summaries[0]
    alternatives = summaries[1:]
    balanced = [
        summary
        for summary in alternatives
        if summary["useful_capture_runs"] >= baseline["useful_capture_runs"] - 1
        and summary["missed_useful_empty_runs"] <= baseline["missed_useful_empty_runs"] + 2
    ]
    if not balanced:
        balanced = [
            summary
            for summary in alternatives
            if summary["missed_useful_empty_runs"] <= baseline["missed_useful_empty_runs"] + 2
        ]
    best_balanced = min(
        balanced,
        key=lambda summary: (
            max(0, baseline["useful_capture_runs"] - summary["useful_capture_runs"]),
            summary["wrong_context_low_selected"],
            summary["low_known_selected"],
            summary["average_selected_per_anchor"] or 99.0,
        ),
    )
    best_precision = min(
        alternatives,
        key=lambda summary: (
            summary["wrong_context_low_selected"],
            summary["low_known_selected"],
            summary["average_selected_per_anchor"] or 99.0,
        ),
    )
    lines = [
        "# Segment/Task-Fit Recall 5x5",
        "",
        f"Date: {manifest['date']}",
        f"Input: `{manifest['input_dir']}`",
        f"Anchors: {manifest['anchors']}",
        "Cohorts: 5",
        "",
        "## Method",
        "",
        "This run compares five segment/task-fit strategy variants against current production health-action selection across five deterministic cohorts.",
        "It replays saved `yaaml recall --debug-ranking` outputs and saved eval oracle labels; no embedding or LLM provider calls are made.",
        "",
        "Limitations:",
        "",
        "1. These strategies operate over the saved candidate set, so they cannot measure memories a different segment query would newly retrieve.",
        "2. Segment fit is proxied by existing `context_score`, `task_key_bonus`, matched task keys, memory kind, filter reasons, and eval-health labels.",
        "3. Wrong-context and stale-task counts are derived from existing oracle rationales for low-scored selected memories.",
        "",
        "## Synthesis",
        "",
        f"1. Best balanced candidate: `{best_balanced['strategy']}` with score {format_float(best_balanced['average_known_score'])}, {best_balanced['useful_capture_runs']} useful runs, {best_balanced['low_known_selected']} low selections, {best_balanced['wrong_context_low_selected']} wrong-context lows, and {format_float(best_balanced['average_selected_per_anchor'])} avg memories.",
        f"2. Strongest precision candidate: `{best_precision['strategy']}` with {best_precision['low_known_selected']} low selections and {best_precision['wrong_context_low_selected']} wrong-context lows, but {best_precision['missed_useful_empty_runs']} missed-useful empty recalls.",
        "3. The main question for runtime work is whether segment evidence can reduce wrong-context lows without leaning on broad abstention.",
        "",
        "## Results",
        "",
        "| Strategy | Avg score | Useful selected | Low selected | Wrong-context lows | Stale-task lows | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for summary in summaries:
        lines.append(
            "| {strategy} | {avg} | {useful} | {low} | {wrong} | {stale} | {useful_runs} | {low_runs} | {avg_mem} | {empty} | {missed} |".format(
                strategy=summary["strategy"],
                avg=format_float(summary["average_known_score"]),
                useful=summary["useful_known_selected"],
                low=summary["low_known_selected"],
                wrong=summary["wrong_context_low_selected"],
                stale=summary["stale_task_low_selected"],
                useful_runs=summary["useful_capture_runs"],
                low_runs=summary["low_selection_runs"],
                avg_mem=format_float(summary["average_selected_per_anchor"]),
                empty=format_rate(summary["empty_recall_rate"]),
                missed=format_rate(summary["missed_useful_empty_rate"]),
            )
        )
    lines.extend([
        "",
        "## Deltas vs Production",
        "",
        "| Strategy | Avg score | Useful selected | Low selected | Wrong-context lows | Stale-task lows | Useful runs | Low runs | Avg memories | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ])
    for summary in alternatives:
        delta = summary["delta_vs_baseline"]
        lines.append(
            "| {strategy} | {avg:+.2f} | {useful:+} | {low:+} | {wrong:+} | {stale:+} | {useful_runs:+} | {low_runs:+} | {avg_mem:+.2f} | {missed:+} |".format(
                strategy=summary["strategy"],
                avg=delta["average_known_score"] or 0.0,
                useful=delta["useful_known_selected"] or 0,
                low=delta["low_known_selected"] or 0,
                wrong=delta["wrong_context_low_selected"] or 0,
                stale=delta["stale_task_low_selected"] or 0,
                useful_runs=delta["useful_capture_runs"] or 0,
                low_runs=delta["low_selection_runs"] or 0,
                avg_mem=delta["average_selected_per_anchor"] or 0.0,
                missed=delta["missed_useful_empty_runs"] or 0,
            )
        )
    lines.extend([
        "",
        "## Stability",
        "",
        "| Strategy | Useful-gain cohorts | Low-reduction cohorts | Wrong-context-reduction cohorts | Cohort avg score range |",
        "| --- | ---: | ---: | ---: | ---: |",
    ])
    for summary in summaries:
        stability = summary["stability"]
        lines.append(
            "| {strategy} | {useful} | {low} | {wrong} | {min_score:.2f}-{max_score:.2f} |".format(
                strategy=summary["strategy"],
                useful="n/a"
                if stability["cohorts_with_useful_gain"] is None
                else stability["cohorts_with_useful_gain"],
                low="n/a"
                if stability["cohorts_with_low_reduction"] is None
                else stability["cohorts_with_low_reduction"],
                wrong="n/a"
                if stability["cohorts_with_wrong_context_reduction"] is None
                else stability["cohorts_with_wrong_context_reduction"],
                min_score=stability["cohort_average_known_score_min"],
                max_score=stability["cohort_average_known_score_max"],
            )
        )
    lines.extend([
        "",
        "## Findings",
        "",
        "1. Segment/task-fit selection is mostly a precision lever over the existing candidate set; this replay does not test improved segment-aware candidate generation.",
        "2. Strategies that require strong task/context evidence should be judged against missed-useful empties, not average score alone.",
        "3. If no variant materially reduces wrong-context lows while preserving useful captures, the next experiment should change candidate generation with segment summaries rather than only rerank retrieved candidates.",
        "",
        "## Structured Artifacts",
        "",
        f"1. Manifest: `{manifest['manifest_path']}`",
        f"2. Summary JSON: `{manifest['summary_path']}`",
        f"3. Per-anchor JSONL: `{manifest['details_path']}`",
        "",
    ])
    path.write_text("\n".join(lines))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, default=Path("target/strict-kind-production"))
    parser.add_argument("--label", default="strict-kind-production")
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-06-30-segment-task-fit-5x5"))
    parser.add_argument("--date", default="2026-06-30")
    parser.add_argument("--limit", type=int, default=3)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    health = H.load_memory_health(args.db)
    recall_paths = sorted(args.input_dir.glob(f"{args.label}.recall-*.json"))
    if not recall_paths:
        raise SystemExit(f"no recall files found in {args.input_dir} for label {args.label}")

    strategies = strategy_definitions()
    cases_by_strategy: dict[str, list[Case]] = {name: [] for name, _strategy in strategies}
    details_path = args.out_dir / "details.jsonl"
    anchors = 0
    with details_path.open("w") as details:
        for index, recall_path in enumerate(recall_paths):
            run_id = int(recall_path.stem.split("-")[-1])
            oracle_path = args.input_dir / f"oracle-{run_id}.json"
            if not oracle_path.exists():
                continue
            anchors += 1
            cohort = index % 5
            recall = load_json(recall_path)
            candidates = recall.get("ranking") or []
            oracle = load_oracle(oracle_path)
            for strategy_name, strategy in strategies:
                selected_ids = strategy(candidates, health, args.limit)
                metrics = case_metrics(strategy_name, run_id, cohort, selected_ids, oracle)
                cases_by_strategy[strategy_name].append(metrics)
                details.write(json.dumps(metrics, sort_keys=True) + "\n")

    summaries = summarize_all(cases_by_strategy)
    manifest_path = args.out_dir / "manifest.json"
    summary_path = args.out_dir / "summary.json"
    report_path = args.out_dir / "REPORT.md"
    manifest = {
        "date": args.date,
        "kind": "segment_task_fit_5x5",
        "input_dir": str(args.input_dir),
        "label": args.label,
        "db_path": str(args.db),
        "anchors": anchors,
        "cohorts": 5,
        "strategies": [name for name, _strategy in strategies],
        "primary_metrics": [
            "average_known_score",
            "useful_known_selected",
            "low_known_selected",
            "wrong_context_low_selected",
            "stale_task_low_selected",
            "useful_capture_runs",
            "missed_useful_empty_runs",
            "average_selected_per_anchor",
        ],
        "manifest_path": str(manifest_path),
        "summary_path": str(summary_path),
        "details_path": str(details_path),
        "report_path": str(report_path),
    }
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    summary_path.write_text(
        json.dumps({"manifest": manifest, "summaries": summaries}, indent=2, sort_keys=True) + "\n"
    )
    write_report(report_path, manifest, summaries)
    print(json.dumps({"out_dir": str(args.out_dir), "anchors": anchors, "summaries": summaries}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
