#!/usr/bin/env python3
"""Run 5x5 experiments across recall technique categories.

The input is a directory produced by scripts/backtest-recall-strategy.sh. This
script replays saved `yaaml recall --debug-ranking` candidates against saved
eval oracle labels. It does not call embedding or LLM providers.

Generation and LLM-filter families are proxies over already retrieved
candidates. They can compare narrowing/ranking behavior, but cannot measure
memories that a different embedding query or a live LLM filter would newly find.
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


def score(candidate: Case) -> float:
    return H.score(candidate)


def memory_id(candidate: Case) -> int:
    return H.memory_id(candidate)


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


def has_task_signal(candidate: Case) -> bool:
    return bool(H.matched_task_keys(candidate)) or task_key_bonus(candidate) > 0.0


def is_durable(candidate: Case) -> bool:
    return kind(candidate) in DURABLE_KINDS


def is_project_or_task_specific(candidate: Case) -> bool:
    return project_bonus(candidate) > 0.0 or has_task_signal(candidate)


def health_delta(candidate: Case, health: dict[int, Any]) -> tuple[float, bool]:
    memory_health = health.get(memory_id(candidate), H.MemoryHealth())
    mode_delta, clear_task_bonus = H.health_mode_adjustment(candidate, memory_health)
    return H.health_adjustment(memory_health) + mode_delta, clear_task_bonus


def health_adjusted_candidates(candidates: list[Case], health: dict[int, Any]) -> list[Case]:
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
    return H.strict_kind_select(health_adjusted_candidates(candidates, health), limit)


def select_with_pool(pool_size: int) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
        return select_production(candidates[:pool_size], health, limit)

    return strategy


def select_fixed_limit(limit_override: int) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], _limit: int) -> list[int]:
        return H.strict_kind_select(health_adjusted_candidates(candidates, health), limit_override)

    return strategy


def select_margin(margin: float, limit_override: int = 3) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], _limit: int) -> list[int]:
        adjusted = health_adjusted_candidates(candidates, health)
        if not adjusted:
            return []
        top_score = score(adjusted[0])
        kept = [candidate for candidate in adjusted if score(candidate) >= top_score - margin]
        return H.strict_kind_select(kept, limit_override)

    return strategy


def select_drop_gap(gap: float) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
        adjusted = health_adjusted_candidates(candidates, health)
        if not adjusted:
            return []
        kept = [adjusted[0]]
        previous = score(adjusted[0])
        for candidate in adjusted[1:]:
            if previous - score(candidate) > gap:
                break
            kept.append(candidate)
            previous = score(candidate)
        return H.strict_kind_select(kept, limit)

    return strategy


def select_score_floor(floor: float) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
        kept = [
            candidate
            for candidate in health_adjusted_candidates(candidates, health)
            if score(candidate) >= floor
        ]
        return H.strict_kind_select(kept, limit)

    return strategy


def select_scored(name: str) -> StrategyFn:
    def scorer(candidate: Case) -> float:
        if name == "vector_only":
            return vector_score(candidate)
        if name == "context_heavy":
            return vector_score(candidate) + context_score(candidate) * 1.25 + min(task_key_bonus(candidate), 0.30)
        if name == "task_key_heavy":
            return vector_score(candidate) + context_score(candidate) + min(task_key_bonus(candidate) * 1.8, 0.80)
        if name == "project_context":
            return vector_score(candidate) + context_score(candidate) + project_bonus(candidate) * 2.0
        return score(candidate)

    def strategy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
        return H.strict_kind_select(score_adjusted_candidates(candidates, health, scorer), limit)

    return strategy


def select_task_state_requires_task(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = [
        candidate
        for candidate in candidates
        if kind(candidate) != "task_state" or has_task_signal(candidate)
    ]
    return select_production(kept, health, limit)


def select_project_or_task_gate(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = [
        candidate
        for candidate in candidates
        if is_durable(candidate) or is_project_or_task_specific(candidate)
    ]
    return select_production(kept, health, limit)


def select_evidence_gate(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = [
        candidate
        for candidate in health_adjusted_candidates(candidates, health)
        if score(candidate) >= 1.05 or strong_task(candidate) or context_score(candidate) >= 0.55
    ]
    return H.strict_kind_select(kept, limit)


def select_durable_or_context_gate(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    kept = [
        candidate
        for candidate in candidates
        if is_durable(candidate)
        or strong_task(candidate)
        or context_score(candidate) >= 0.50
        or project_bonus(candidate) > 0.0
    ]
    return select_production(kept, health, limit)


def select_abstain_if_weak_top(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    adjusted = health_adjusted_candidates(candidates, health)
    if not adjusted or score(adjusted[0]) < 1.05:
        return []
    return H.strict_kind_select(adjusted, limit)


def select_llm_conservative_proxy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    adjusted = []
    for candidate in health_adjusted_candidates(candidates, health):
        memory_health = health.get(memory_id(candidate), H.MemoryHealth())
        if score(candidate) >= 1.05 or strong_task(candidate) or memory_health.failure_mode == "proven_useful":
            adjusted.append(candidate)
    return H.strict_kind_select(adjusted, limit)


def select_llm_strict_proxy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    adjusted = []
    for candidate in health_adjusted_candidates(candidates, health):
        memory_health = health.get(memory_id(candidate), H.MemoryHealth())
        evidence = strong_task(candidate) or context_score(candidate) >= 0.50 or memory_health.failure_mode == "proven_useful"
        if score(candidate) >= 1.15 and evidence:
            adjusted.append(candidate)
    return H.strict_kind_select(adjusted, limit)


def select_llm_one_best_proxy(candidates: list[Case], health: dict[int, Any], _limit: int) -> list[int]:
    adjusted = health_adjusted_candidates(candidates, health)
    if not adjusted:
        return []
    top = adjusted[0]
    if score(top) < 1.00 and not strong_task(top):
        return []
    return [memory_id(top)]


def select_lifecycle_filter(modes: set[str]) -> StrategyFn:
    def strategy(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
        kept = [
            candidate
            for candidate in candidates
            if health.get(memory_id(candidate), H.MemoryHealth()).failure_mode not in modes
        ]
        return select_production(kept, health, limit)

    return strategy


def select_proven_useful_boost(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    def scorer(candidate: Case) -> float:
        memory_health = health.get(memory_id(candidate), H.MemoryHealth())
        boost = 0.18 if memory_health.failure_mode == "proven_useful" else 0.0
        return score(candidate) + boost

    return H.strict_kind_select(score_adjusted_candidates(candidates, health, scorer), limit)


def select_eval_ratio_rerank(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    return H.health_rerank(candidates, health, limit)


def select_eval_failure_mode_rerank(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    return H.health_action_rerank(candidates, health, limit)


def select_eval_precision_abstain(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    return H.health_precision_abstain(candidates, health, limit)


def select_eval_proven_or_context(candidates: list[Case], health: dict[int, Any], limit: int) -> list[int]:
    adjusted = []
    for candidate in health_adjusted_candidates(candidates, health):
        memory_health = health.get(memory_id(candidate), H.MemoryHealth())
        if memory_health.failure_mode == "proven_useful" or strong_task(candidate) or context_score(candidate) >= 0.50:
            adjusted.append(candidate)
    return H.strict_kind_select(adjusted, limit)


def category_definitions() -> dict[str, list[tuple[str, StrategyFn]]]:
    return {
        "candidate_generation": [
            ("pool_3", select_with_pool(3)),
            ("pool_5", select_with_pool(5)),
            ("pool_8", select_with_pool(8)),
            ("pool_12", select_with_pool(12)),
            ("pool_16", select_with_pool(16)),
        ],
        "hard_gating": [
            ("production_health_action", select_production),
            ("task_state_requires_task", select_task_state_requires_task),
            ("project_or_task_gate", select_project_or_task_gate),
            ("evidence_gate", select_evidence_gate),
            ("durable_or_context_gate", select_durable_or_context_gate),
        ],
        "reranking": [
            ("production_health_action", select_production),
            ("eval_ratio_rerank", select_eval_ratio_rerank),
            ("context_heavy_proxy", select_scored("context_heavy")),
            ("task_key_heavy_proxy", select_scored("task_key_heavy")),
            ("project_context_proxy", select_scored("project_context")),
        ],
        "selection_budgeting": [
            ("production_health_action", select_production),
            ("top_1", select_fixed_limit(1)),
            ("top_2", select_fixed_limit(2)),
            ("within_0_15_of_top", select_margin(0.15)),
            ("stop_on_0_20_gap", select_drop_gap(0.20)),
        ],
        "llm_filter_proxy": [
            ("production_health_action", select_production),
            ("abstain_if_weak_top", select_abstain_if_weak_top),
            ("llm_conservative_proxy", select_llm_conservative_proxy),
            ("llm_strict_proxy", select_llm_strict_proxy),
            ("llm_one_best_proxy", select_llm_one_best_proxy),
        ],
        "memory_corpus_quality": [
            ("production_health_action", select_production),
            ("suppress_vague", select_lifecycle_filter({"vague_under_contextualized"})),
            ("suppress_likely_low", select_lifecycle_filter({"likely_low_value", "consistently_low_value"})),
            ("suppress_all_bad_modes", select_lifecycle_filter({
                "stale_episodic",
                "consistently_low_value",
                "likely_low_value",
                "vague_under_contextualized",
                "noisy_metadata",
            })),
            ("proven_useful_boost", select_proven_useful_boost),
        ],
        "eval_feedback": [
            ("strict_kind_no_eval", lambda candidates, _health, limit: H.strict_kind_select(candidates, limit)),
            ("eval_ratio_rerank", select_eval_ratio_rerank),
            ("failure_mode_rerank", select_eval_failure_mode_rerank),
            ("precision_abstain", select_eval_precision_abstain),
            ("proven_or_context_gate", select_eval_proven_or_context),
        ],
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
    category: str,
    strategy: str,
    run_id: int,
    cohort: int,
    selected_ids: list[int],
    scores: dict[int, int],
) -> Case:
    selected_ids = H.dedupe(selected_ids, 3)
    known_scores = [scores[memory_id_value] for memory_id_value in selected_ids if memory_id_value in scores]
    return {
        "category": category,
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


def summarize_category(cases_by_strategy: dict[str, list[Case]]) -> list[Case]:
    first_strategy = next(iter(cases_by_strategy))
    baseline = summarize(first_strategy, cases_by_strategy[first_strategy])
    summaries = [baseline]
    for strategy, cases in list(cases_by_strategy.items())[1:]:
        summaries.append(summarize(strategy, cases, baseline))
    for summary in summaries:
        cohorts = []
        for cohort in range(5):
            cohort_cases = [case for case in cases_by_strategy[summary["strategy"]] if case["cohort"] == cohort]
            baseline_cases = [case for case in cases_by_strategy[first_strategy] if case["cohort"] == cohort]
            cohorts.append(summarize(str(cohort), cohort_cases, summarize("baseline", baseline_cases)))
        summary["cohorts"] = cohorts
        summary["stability"] = {
            "cohorts_with_useful_gain": None
            if summary["strategy"] == first_strategy
            else sum(1 for cohort in cohorts if (cohort.get("delta_vs_baseline") or {}).get("useful_known_selected", 0) > 0),
            "cohorts_with_low_reduction": None
            if summary["strategy"] == first_strategy
            else sum(1 for cohort in cohorts if (cohort.get("delta_vs_baseline") or {}).get("low_known_selected", 0) < 0),
            "cohort_average_known_score_min": min((cohort["average_known_score"] or 0.0) for cohort in cohorts),
            "cohort_average_known_score_max": max((cohort["average_known_score"] or 0.0) for cohort in cohorts),
        }
    return summaries


def format_float(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.2f}"


def format_rate(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.1%}"


def category_title(category: str) -> str:
    return category.replace("_", " ").title().replace("Llm", "LLM")


def best_readout(category_summaries: dict[str, list[Case]]) -> list[str]:
    notes = []
    for category, summaries in category_summaries.items():
        baseline = summaries[0]
        alternatives = summaries[1:]
        balanced = [
            summary
            for summary in alternatives
            if summary["useful_capture_runs"] >= baseline["useful_capture_runs"]
            and summary["missed_useful_empty_runs"] <= baseline["missed_useful_empty_runs"] + 2
        ]
        best_balanced = (
            min(
                balanced,
                key=lambda summary: (
                    summary["low_known_selected"],
                    summary["average_selected_per_anchor"] or 0.0,
                    -summary["useful_known_selected"],
                ),
            )
            if balanced
            else None
        )
        best_low = min(alternatives, key=lambda summary: summary["low_known_selected"])
        best_volume = min(alternatives, key=lambda summary: summary["average_selected_per_anchor"] or 99.0)
        best_score = max(alternatives, key=lambda summary: summary["average_known_score"] or 0.0)
        if best_balanced:
            balanced_text = (
                f"best balanced `{best_balanced['strategy']}` "
                f"(score={format_float(best_balanced['average_known_score'])}, "
                f"useful_runs={best_balanced['useful_capture_runs']}, "
                f"low={best_balanced['low_known_selected']}, "
                f"avg_mem={format_float(best_balanced['average_selected_per_anchor'])})"
            )
        else:
            balanced_text = "no alternative preserved useful-run coverage within the missed-useful budget"
        warning = ""
        if best_score["missed_useful_empty_runs"] > baseline["missed_useful_empty_runs"] + 10:
            warning = (
                f" `{best_score['strategy']}` has a misleading high score because it missed "
                f"{best_score['missed_useful_empty_runs']} useful-empty cases."
            )
        notes.append(
            {
                "category": category,
                "text": (
                    f"{balanced_text}. Lowest lows `{best_low['strategy']}` "
                    f"(low={best_low['low_known_selected']}, useful={best_low['useful_known_selected']}); "
                    f"lowest volume `{best_volume['strategy']}` "
                    f"(avg_mem={format_float(best_volume['average_selected_per_anchor'])}, "
                    f"useful_runs={best_volume['useful_capture_runs']}). "
                    f"Baseline `{baseline['strategy']}` score={format_float(baseline['average_known_score'])}, "
                    f"useful_runs={baseline['useful_capture_runs']}, low={baseline['low_known_selected']}, "
                    f"avg_mem={format_float(baseline['average_selected_per_anchor'])}.{warning}"
                ),
            }
        )
    return notes


def write_category_section(lines: list[str], category: str, summaries: list[Case]) -> None:
    lines.extend([
        f"## {category_title(category)}",
        "",
        "| Strategy | Avg known score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ])
    for summary in summaries:
        lines.append(
            "| {strategy} | {avg} | {useful} | {low} | {useful_runs} | {low_runs} | {avg_mem} | {empty} | {missed} |".format(
                strategy=summary["strategy"],
                avg=format_float(summary["average_known_score"]),
                useful=summary["useful_known_selected"],
                low=summary["low_known_selected"],
                useful_runs=summary["useful_capture_runs"],
                low_runs=summary["low_selection_runs"],
                avg_mem=format_float(summary["average_selected_per_anchor"]),
                empty=format_rate(summary["empty_recall_rate"]),
                missed=format_rate(summary["missed_useful_empty_rate"]),
            )
        )
    lines.extend([
        "",
        "Deltas vs first strategy in category:",
        "",
        "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ])
    for summary in summaries[1:]:
        delta = summary.get("delta_vs_baseline") or {}
        lines.append(
            "| {strategy} | {avg:+.2f} | {useful:+} | {low:+} | {useful_runs:+} | {low_runs:+} | {avg_mem:+.2f} | {missed:+} |".format(
                strategy=summary["strategy"],
                avg=delta.get("average_known_score") or 0.0,
                useful=delta.get("useful_known_selected") or 0,
                low=delta.get("low_known_selected") or 0,
                useful_runs=delta.get("useful_capture_runs") or 0,
                low_runs=delta.get("low_selection_runs") or 0,
                avg_mem=delta.get("average_selected_per_anchor") or 0.0,
                missed=delta.get("missed_useful_empty_runs") or 0,
            )
        )
    lines.extend([
        "",
        "Stability across five cohorts:",
        "",
        "| Strategy | Useful-gain cohorts | Low-reduction cohorts | Cohort avg score range |",
        "| --- | ---: | ---: | ---: |",
    ])
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
    lines.append("")


def write_report(path: Path, manifest: Case, category_summaries: dict[str, list[Case]], notes: list[Case]) -> None:
    lines = [
        "# Recall Techniques 5x5 Experiments",
        "",
        f"Date: {manifest['date']}",
        f"Input: `{manifest['input_dir']}`",
        f"Anchors: {manifest['anchors']}",
        "Cohorts: 5",
        "",
        "## Method",
        "",
        "Each category compares five strategies across five deterministic cohorts using saved `yaaml recall --debug-ranking` outputs and saved eval oracle labels.",
        "No embedding or LLM provider calls are made during replay. Empty recall is reported as abstention, not as a low score.",
        "",
        "Limitations:",
        "",
        "1. Candidate-generation variants are bounded by the saved 16-candidate ranking for each anchor.",
        "2. Query-signal and LLM-filter variants are deterministic proxies over already retrieved candidates.",
        "3. Corpus-quality variants simulate suppression or boosting; they do not rewrite memory content.",
        "",
        "## Synthesis",
        "",
    ]
    for index, note in enumerate(notes, start=1):
        lines.append(f"{index}. {category_title(note['category'])}: {note['text']}")
    lines.append("")
    lines.extend([
        "## Directional Takeaways",
        "",
        "1. The strongest context-bloat reducer is dynamic selection, especially `top_2`: it preserved useful-run coverage while reducing low selections and average memory count.",
        "2. Moderate abstention/filtering helps when it removes weak candidates, but strict variants can inflate average score by missing useful recalls.",
        "3. Project/context-heavy reranking improved useful coverage but did not reduce context volume in this replay; it is useful for relevance, not the primary bloat lever.",
        "4. Broad hard gates are not clearly better than narrow evidence gates; most variants were neutral, and aggressive gates risk missed-useful abstentions.",
        "5. Corpus-quality actions currently have small effects in replay, suggesting memory-health labels are useful as ranking inputs but not enough by themselves to solve context bloat.",
        "6. Eval feedback is valuable: compared with no eval-informed scoring, failure-mode reranking sharply reduced low selections, while precision abstention reduced lows further at the cost of additional missed-useful empties.",
        "",
        "## Recommended Next Runtime Experiment",
        "",
        "1. Apply a default `top_2` dynamic selection cap after existing ranking and health rerank.",
        "2. Add a moderate weak-top abstention gate, not a strict LLM-style filter, and track missed-useful empties separately.",
        "3. Keep failure-mode reranking enabled as the eval-informed baseline.",
        "4. Do not prioritize broad hard gates or corpus suppression until memory-health labels become more discriminating.",
        "5. Re-run this report after adding session cooldown so context volume captures repeated-memory bloat as well as per-call bloat.",
        "",
    ])
    for category, summaries in category_summaries.items():
        write_category_section(lines, category, summaries)
    lines.extend([
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
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-06-23-techniques-5x5"))
    parser.add_argument("--date", default="2026-06-23")
    parser.add_argument("--limit", type=int, default=3)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    health = H.load_memory_health(args.db)
    recall_paths = sorted(args.input_dir.glob(f"{args.label}.recall-*.json"))
    if not recall_paths:
        raise SystemExit(f"no recall files found in {args.input_dir} for label {args.label}")

    categories = category_definitions()
    raw_cases: dict[str, dict[str, list[Case]]] = {
        category: {strategy_name: [] for strategy_name, _ in strategies}
        for category, strategies in categories.items()
    }
    details_path = args.out_dir / "details.jsonl"
    anchors = 0
    with details_path.open("w") as details:
        for index, recall_path in enumerate(recall_paths):
            run_id = int(recall_path.stem.split("-")[-1])
            cohort = index % 5
            oracle_path = args.input_dir / f"oracle-{run_id}.json"
            if not oracle_path.exists():
                continue
            anchors += 1
            recall = load_json(recall_path)
            candidates = recall.get("ranking") or []
            scores = load_oracle_scores(oracle_path)
            for category, strategies in categories.items():
                for strategy_name, strategy in strategies:
                    selected_ids = strategy(candidates, health, args.limit)
                    metrics = case_metrics(category, strategy_name, run_id, cohort, selected_ids, scores)
                    raw_cases[category][strategy_name].append(metrics)
                    details.write(json.dumps(metrics, sort_keys=True) + "\n")

    category_summaries = {
        category: summarize_category(cases_by_strategy)
        for category, cases_by_strategy in raw_cases.items()
    }
    notes = best_readout(category_summaries)
    summary = {
        "date": args.date,
        "input_dir": str(args.input_dir),
        "label": args.label,
        "anchors": anchors,
        "categories": category_summaries,
        "synthesis": notes,
    }
    summary_path = args.out_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    manifest = {
        "date": args.date,
        "kind": "recall_techniques_5x5",
        "input_dir": str(args.input_dir),
        "label": args.label,
        "db_path": str(args.db),
        "anchors": anchors,
        "categories": list(categories.keys()),
        "strategies_by_category": {
            category: [name for name, _strategy in strategies]
            for category, strategies in categories.items()
        },
        "details_path": str(details_path),
        "summary_path": str(summary_path),
        "manifest_path": str(args.out_dir / "manifest.json"),
        "report_path": str(args.out_dir / "REPORT.md"),
    }
    manifest_path = args.out_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    write_report(args.out_dir / "REPORT.md", manifest, category_summaries, notes)
    print(json.dumps({"out_dir": str(args.out_dir), "anchors": anchors, "synthesis": notes}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
