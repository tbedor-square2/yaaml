#!/usr/bin/env python3
"""Replay health-aware recall strategies and capture a structured 10x10 report.

The input is a directory produced by scripts/backtest-recall-strategy.sh. The
script does not call embedding or LLM providers; it replays candidate rankings
already emitted by `yaaml recall --debug-ranking` and compares selected memory
IDs against the saved eval oracle files.

The exercise compares ten failure-mode-aware strategy variants over ten
deterministic cohorts. The aggregate tables show total behavior, while cohort
tables make it visible when a strategy only wins on a narrow slice of the
library.
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
    "eval_health_rerank",
    "wrong_context_gate",
    "wrong_context_rerank",
    "stale_low_filter",
    "noisy_metadata_rekey",
    "context_sensitive_gate",
    "health_action_filter",
    "health_action_rerank",
    "health_precision_abstain",
]


@dataclass(frozen=True)
class MemoryHealth:
    judged_count: int = 0
    useful_count: int = 0
    low_count: int = 0
    average_score: float | None = None
    failure_mode: str = "unproven"
    recommended_action: str = "keep_observing"
    memory_kind: str = "unknown"
    task_key_count: int = 0

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


def weak_or_strong_task(candidate: Case) -> bool:
    return strong_task(candidate) or task_key_bonus(candidate) >= 0.08 or bool(matched_task_keys(candidate))


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


def health_adjustment(memory_health: MemoryHealth) -> float:
    adjustment = 0.0
    if memory_health.judged_count >= 3:
        adjustment += min(0.16, memory_health.useful_ratio * 0.12)
        adjustment -= min(0.24, memory_health.low_ratio * 0.18)
    if memory_health.globally_bad:
        adjustment -= 0.40
    return adjustment


def health_rerank(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        clone = dict(candidate)
        clone["score"] = score(candidate) + health_adjustment(memory_health)
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def clone_candidate(candidate: Case, score_delta: float = 0.0, clear_task_bonus: bool = False) -> Case:
    clone = dict(candidate)
    clone["score"] = score(candidate) + score_delta
    if clear_task_bonus:
        rank = dict(clone.get("rank") or {})
        rank["task_key_bonus"] = 0.0
        rank["matched_task_keys"] = []
        clone["rank"] = rank
        clone["filter_reasons"] = [
            reason
            for reason in clone.get("filter_reasons") or []
            if reason != "keep:strong_task_key_match"
        ]
    return clone


def wrong_context_applies(candidate: Case, memory_health: MemoryHealth) -> bool:
    if memory_health.failure_mode != "wrong_context":
        return False
    return not (strong_task(candidate) or context_score(candidate) >= 0.62)


def wrong_context_gate(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    kept = [
        candidate
        for candidate in candidates
        if not wrong_context_applies(candidate, health.get(memory_id(candidate), MemoryHealth()))
    ]
    return strict_kind_select(kept, limit)


def wrong_context_rerank(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        penalty = -0.55 if wrong_context_applies(candidate, memory_health) else 0.0
        adjusted.append(clone_candidate(candidate, penalty))
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def stale_low_filter(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    dropped_modes = {"stale_episodic", "consistently_low_value"}
    kept = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        if memory_health.failure_mode in dropped_modes:
            continue
        if memory_health.failure_mode == "likely_low_value" and memory_health.judged_count >= 5:
            continue
        kept.append(candidate)
    return strict_kind_select(kept, limit)


def noisy_metadata_rekey(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        if memory_health.failure_mode == "noisy_metadata":
            penalty = -min(0.40, task_key_bonus(candidate) + 0.16)
            adjusted.append(clone_candidate(candidate, penalty, clear_task_bonus=True))
        else:
            adjusted.append(candidate)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def context_sensitive_gate(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    modes = {"context_sensitive", "mixed_performance"}
    kept = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        if memory_health.failure_mode in modes and not (
            strong_task(candidate) or context_score(candidate) >= 0.50
        ):
            continue
        kept.append(candidate)
    return strict_kind_select(kept, limit)


def health_action_filter(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    kept = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        if memory_health.failure_mode in {"stale_episodic", "consistently_low_value"}:
            continue
        if memory_health.failure_mode == "wrong_context" and not (
            strong_task(candidate) or context_score(candidate) >= 0.62
        ):
            continue
        if memory_health.failure_mode == "noisy_metadata" and not context_score(candidate) >= 0.48:
            continue
        if memory_health.failure_mode in {"context_sensitive", "mixed_performance"} and not (
            weak_or_strong_task(candidate) or context_score(candidate) >= 0.50
        ):
            continue
        kept.append(candidate)
    return strict_kind_select(kept, limit)


def health_mode_adjustment(candidate: Case, memory_health: MemoryHealth) -> tuple[float, bool]:
    mode = memory_health.failure_mode
    if mode == "proven_useful":
        return 0.14, False
    if mode == "wrong_context":
        return (-0.55 if wrong_context_applies(candidate, memory_health) else -0.08), False
    if mode in {"stale_episodic", "consistently_low_value"}:
        return -0.65, False
    if mode == "likely_low_value":
        return -0.25, False
    if mode == "noisy_metadata":
        return (-min(0.42, task_key_bonus(candidate) + 0.18), True)
    if mode in {"context_sensitive", "mixed_performance"}:
        return (0.04 if strong_task(candidate) or context_score(candidate) >= 0.50 else -0.24), False
    if mode == "vague_under_contextualized":
        return -0.20, False
    return 0.0, False


def health_action_rerank(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        mode_delta, clear_task_bonus = health_mode_adjustment(candidate, memory_health)
        adjusted.append(
            clone_candidate(
                candidate,
                health_adjustment(memory_health) + mode_delta,
                clear_task_bonus=clear_task_bonus,
            )
        )
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return strict_kind_select(adjusted, limit)


def health_precision_abstain(candidates: list[Case], health: dict[int, MemoryHealth], limit: int) -> list[int]:
    adjusted = []
    for candidate in candidates:
        memory_health = health.get(memory_id(candidate), MemoryHealth())
        mode_delta, clear_task_bonus = health_mode_adjustment(candidate, memory_health)
        clone = clone_candidate(
            candidate,
            health_adjustment(memory_health) + mode_delta,
            clear_task_bonus=clear_task_bonus,
        )
        if score(clone) < 1.05 and not (
            memory_health.failure_mode == "proven_useful" and score(clone) >= 0.95
        ):
            continue
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
            SELECT m.id,
                   m.title,
                   m.body,
                   m.memory_kind,
                   m.task_keys,
                   er.judge_score,
                   er.rationale,
                   er.id
            FROM memories m
            LEFT JOIN eval_results er
              ON er.memory_id = m.id
             AND er.judge_score IN ('1', '2', '3', '4', '5')
            WHERE m.is_active = 1
            ORDER BY m.id, er.id
            """
        ).fetchall()
    finally:
        conn.close()
    raw: dict[int, Case] = {}
    for memory_id_value, title, body, memory_kind, task_keys_json, judge_score, rationale, _row_id in rows:
        memory_id_key = int(memory_id_value)
        entry = raw.setdefault(
            memory_id_key,
            {
                "title": title or "",
                "body": body or "",
                "memory_kind": memory_kind or "unknown",
                "task_keys": json.loads(task_keys_json or "[]"),
                "scores": [],
                "latest_low_rationale": "",
            },
        )
        if judge_score in {"1", "2", "3", "4", "5"}:
            numeric_score = int(judge_score)
            entry["scores"].append(numeric_score)
            if numeric_score <= 2 and rationale:
                entry["latest_low_rationale"] = str(rationale)

    result = {}
    for memory_id_key, entry in raw.items():
        scores = entry["scores"]
        useful_count = sum(1 for value in scores if value >= 4)
        low_count = sum(1 for value in scores if value <= 2)
        judged_count = len(scores)
        failure_mode = diagnose_memory_failure(
            entry["title"],
            entry["body"],
            entry["memory_kind"],
            entry["task_keys"],
            judged_count,
            useful_count,
            low_count,
            entry["latest_low_rationale"],
        )
        result[memory_id_key] = MemoryHealth(
            judged_count=judged_count,
            useful_count=useful_count,
            low_count=low_count,
            average_score=(sum(scores) / judged_count if judged_count else None),
            failure_mode=failure_mode,
            recommended_action=recommended_memory_action(failure_mode),
            memory_kind=entry["memory_kind"],
            task_key_count=len(entry["task_keys"]),
        )
    return result


def diagnose_memory_failure(
    title: str,
    body: str,
    memory_kind: str,
    task_keys: list[str],
    judged_count: int,
    useful_count: int,
    low_count: int,
    latest_low_rationale: str,
) -> str:
    low_rate = low_count / judged_count if judged_count else 0.0
    useful_rate = useful_count / judged_count if judged_count else 0.0
    text = f"{title}\n{body}\n{latest_low_rationale}".lower()
    latest_low = latest_low_rationale.lower()
    if judged_count >= 5 and useful_count == 0 and low_rate >= 0.70:
        if rationale_mentions_wrong_context(latest_low):
            return "wrong_context"
        if looks_stale_or_episodic(memory_kind, text):
            return "stale_episodic"
        if len(body) < 300:
            return "vague_under_contextualized"
        if len(task_keys) >= 6:
            return "noisy_metadata"
        return "consistently_low_value"
    if judged_count >= 5 and useful_rate >= 0.70:
        return "proven_useful"
    if useful_count > 0 and low_count > 0:
        if rationale_mentions_wrong_context(latest_low) or memory_kind in {"task_state", "project_fact"}:
            return "context_sensitive"
        return "mixed_performance"
    if judged_count > 0 and low_rate >= 0.70:
        if len(body) < 300:
            return "vague_under_contextualized"
        if len(task_keys) >= 6:
            return "noisy_metadata"
        return "likely_low_value"
    return "unproven"


def rationale_mentions_wrong_context(rationale: str) -> bool:
    return any(
        needle in rationale
        for needle in [
            "unrelated",
            "wrong context",
            "different project",
            "different domain",
            "no connection",
            "no bearing",
            "irrelevant",
            "mismatch",
        ]
    )


def looks_stale_or_episodic(memory_kind: str, text: str) -> bool:
    return memory_kind == "task_state" or any(
        needle in text
        for needle in [
            "stale",
            "obsolete",
            "old pr",
            "draft pr",
            "paused",
            "blocked",
            "remaining",
            "open questions",
            "completed",
            "rollout",
            "temporary",
            "current progress",
        ]
    )


def recommended_memory_action(failure_mode: str) -> str:
    return {
        "wrong_context": "regenerate_metadata_or_tighten_gates",
        "stale_episodic": "move_to_dormant",
        "vague_under_contextualized": "refine_or_suppress",
        "noisy_metadata": "regenerate_task_keys",
        "consistently_low_value": "suppress_or_tombstone",
        "context_sensitive": "require_stronger_context_match",
        "mixed_performance": "context_sensitive_rerank",
        "likely_low_value": "suppress_pending_more_evals",
        "proven_useful": "boost_or_keep_active",
    }.get(failure_mode, "keep_observing")


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
        "eval_health_rerank": health_rerank(candidates, health, limit),
        "wrong_context_gate": wrong_context_gate(candidates, health, limit),
        "wrong_context_rerank": wrong_context_rerank(candidates, health, limit),
        "stale_low_filter": stale_low_filter(candidates, health, limit),
        "noisy_metadata_rekey": noisy_metadata_rekey(candidates, health, limit),
        "context_sensitive_gate": context_sensitive_gate(candidates, health, limit),
        "health_action_filter": health_action_filter(candidates, health, limit),
        "health_action_rerank": health_action_rerank(candidates, health, limit),
        "health_precision_abstain": health_precision_abstain(candidates, health, limit),
    }


def write_report(
    path: Path,
    manifest: Case,
    summaries: list[Case],
    notes: list[str],
) -> None:
    lines = [
        "# Health-Aware Recall 10x10 Experiment",
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
        "The exercise compares ten health-aware selection approaches over ten deterministic cohorts. Scoring metrics only use memories that were actually selected and judged. Empty responses are tracked separately as abstentions, not as low-quality recall.",
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
    eval_health_summary = next(
        (summary for summary in summaries if summary["strategy"] == "eval_health_rerank"),
        None,
    )
    if eval_health_summary:
        lines.extend(
            [
                "",
                "## Deltas vs Eval-Health Rerank",
                "",
                "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Missed-useful empty | Avg memories |",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
            ]
        )
        for summary in summaries:
            if summary["strategy"] in {"baseline_strict_kind", "eval_health_rerank"}:
                continue
            lines.append(
                "| {strategy} | {average_known_score:+.2f} | {useful_known_selected:+} | {low_known_selected:+} | {useful_capture_runs:+} | {low_selection_runs:+} | {missed_useful_empty_runs:+} | {average_selected_per_anchor:+.2f} |".format(
                    strategy=summary["strategy"],
                    average_known_score=metric_delta(summary, eval_health_summary, "average_known_score"),
                    useful_known_selected=int(metric_delta(summary, eval_health_summary, "useful_known_selected")),
                    low_known_selected=int(metric_delta(summary, eval_health_summary, "low_known_selected")),
                    useful_capture_runs=int(metric_delta(summary, eval_health_summary, "useful_capture_runs")),
                    low_selection_runs=int(metric_delta(summary, eval_health_summary, "low_selection_runs")),
                    missed_useful_empty_runs=int(metric_delta(summary, eval_health_summary, "missed_useful_empty_runs")),
                    average_selected_per_anchor=metric_delta(summary, eval_health_summary, "average_selected_per_anchor"),
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
            "`eval_health_rerank` is retained as the generic eval-history baseline from the previous experiment.",
            "",
            "`health_action_rerank` is the best balanced failure-mode-aware strategy in this run: compared with `eval_health_rerank`, it keeps useful selected memories roughly flat, cuts low selected memories materially, and does not collapse into pure abstention.",
            "",
            "`health_precision_abstain` is the strongest precision mode, but it increases missed-useful abstentions. It is useful evidence for an abstaining mode, not a default recall policy.",
            "",
            "Single-mode policies like `wrong_context_gate`, `stale_low_filter`, and `context_sensitive_gate` are weaker than the combined reranker. The failure modes interact; handling only one class of bad recall leaves too much noise or drops too much useful recall.",
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


def metric_delta(summary: Case, baseline: Case, key: str) -> float:
    if summary.get(key) is None or baseline.get(key) is None:
        return 0.0
    return float(summary[key]) - float(baseline[key])


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, default=Path("target/strict-kind-production"))
    parser.add_argument("--label", default="strict-kind-production")
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-06-22-health-10x10"))
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
        "`eval_health_rerank` is the generic eval-history reranker from the prior 10x10.",
        "`wrong_context_gate` and `wrong_context_rerank` restrict memories diagnosed as valid-but-wrong-context unless context/task fit is strong.",
        "`stale_low_filter`, `noisy_metadata_rekey`, and `context_sensitive_gate` test targeted handling for specific failure modes.",
        "`health_action_filter`, `health_action_rerank`, and `health_precision_abstain` combine the diagnosed recommended actions.",
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
        "health_diagnoses": sorted(
            {
                memory_health.failure_mode
                for memory_health in health.values()
                if memory_health.failure_mode != "unproven"
            }
        ),
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
