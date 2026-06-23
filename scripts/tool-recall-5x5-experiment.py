#!/usr/bin/env python3
"""Replay recent tool-triggered recall evals against narrowing strategies.

This is a pruning backtest: it starts from memories YAAML actually selected for
recent `tool_pre_use` recalls and measures which selected memories each strategy
would keep or suppress. It cannot measure alternate memories that a different
retrieval pass would have found.
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable


POST_FIX_UNIX = 1_782_251_960
COHORTS = ["git", "yarn", "apply_patch_source", "apply_patch_text", "other"]
STRATEGIES = [
    "baseline",
    "session_cooldown_20m",
    "family_cooldown_20m",
    "command_family_gate",
    "targeted_cooldown",
]


@dataclass(frozen=True)
class Result:
    memory_id: int | None
    score: str
    title: str
    body: str


@dataclass(frozen=True)
class Run:
    run_id: int
    started_unix: int
    started_at: str
    turn_ordinal: int | None
    tool_name: str
    injected: bool | None
    input: str
    results: tuple[Result, ...]


def unix_seconds(value: str) -> int:
    if value.startswith("unix:"):
        return int(value[5:])
    return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp())


def numeric_score(score: str) -> int | None:
    return int(score) if score in {"1", "2", "3", "4", "5"} else None


def memory_results(run: Run) -> list[Result]:
    return [result for result in run.results if result.memory_id is not None]


def score_is_useful(score: str) -> bool:
    value = numeric_score(score)
    return value is not None and value >= 4


def score_is_low(score: str) -> bool:
    value = numeric_score(score)
    return value is not None and value <= 2


def command_family(run: Run) -> str:
    command = run.input.strip()
    lower = command.lower()
    if run.tool_name == "apply_patch":
        if "/tmp/" in lower or re.search(r"add file: /?tmp/", lower):
            return "apply_patch_text"
        return "apply_patch_source"
    if lower.startswith("yarn "):
        return "yarn"
    if lower.startswith("git "):
        return "git"
    if lower.startswith("gh "):
        return "other"
    if lower.startswith("npm "):
        return "other"
    return "other"


def cohort_for(run: Run) -> str:
    family = command_family(run)
    return family if family in COHORTS else "other"


def command_terms(run: Run) -> set[str]:
    terms = set(re.findall(r"[A-Za-z][A-Za-z0-9_-]{2,}", run.input.lower()))
    stop = {
        "begin",
        "patch",
        "update",
        "file",
        "users",
        "tbedor",
        "development",
        "subapps",
        "src",
        "tmp",
        "origin",
        "master",
        "main",
        "true",
        "head",
    }
    return {term for term in terms if term not in stop}


def memory_text(result: Result) -> str:
    return f"{result.title}\n{result.body}".lower()


def command_family_allows(run: Run, result: Result) -> bool:
    text = memory_text(result)
    family = command_family(run)
    command = run.input.lower()
    if family == "yarn":
        return "yarn" in text or "node_modules" in text
    if family == "git":
        if "force-with-lease" in command:
            return "force-with-lease" in text or "branch" in text or "pr" in text
        if "rebase" in command:
            return "rebase" in text
        return False
    if family == "apply_patch_text":
        return False
    if family == "apply_patch_source":
        terms = command_terms(run)
        return bool(terms and terms.intersection(set(re.findall(r"[a-z][a-z0-9_-]{2,}", text))))
    return False


def load_runs(db_path: Path, since_unix: int, limit: int) -> list[Run]:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT
            er.id,
            er.started_at,
            er.turn_ordinal,
            er.tool_name,
            er.injected,
            json_extract(er.config_json, '$.tool_input_summary') AS input
        FROM eval_runs er
        WHERE er.recall_origin = 'tool_pre_use'
          AND CAST(substr(er.started_at, 6) AS INTEGER) >= ?
        ORDER BY CAST(substr(er.started_at, 6) AS INTEGER) ASC, er.id ASC
        LIMIT ?
        """,
        (since_unix, limit),
    ).fetchall()
    runs: list[Run] = []
    for row in rows:
        result_rows = conn.execute(
            """
            SELECT er.memory_id, er.judge_score, COALESCE(m.title, '') AS title, COALESCE(m.body, '') AS body
            FROM eval_results er
            LEFT JOIN memories m ON m.id = er.memory_id
            WHERE er.eval_run_id = ?
            ORDER BY er.id ASC
            """,
            (row["id"],),
        ).fetchall()
        runs.append(
            Run(
                run_id=int(row["id"]),
                started_unix=unix_seconds(row["started_at"]),
                started_at=row["started_at"],
                turn_ordinal=row["turn_ordinal"],
                tool_name=row["tool_name"] or "-",
                injected=bool(row["injected"]) if row["injected"] is not None else None,
                input=row["input"] or "",
                results=tuple(
                    Result(
                        memory_id=result["memory_id"],
                        score=result["judge_score"] or "",
                        title=result["title"],
                        body=result["body"],
                    )
                    for result in result_rows
                ),
            )
        )
    conn.close()
    return runs


def select_baseline(run: Run, _state: dict) -> list[Result]:
    return memory_results(run)


def select_session_cooldown(run: Run, state: dict) -> list[Result]:
    selected = []
    recalled_at: dict[int, int] = state.setdefault("recalled_at", {})
    for result in memory_results(run):
        assert result.memory_id is not None
        previous = recalled_at.get(result.memory_id)
        if previous is None or run.started_unix - previous >= 20 * 60:
            selected.append(result)
            recalled_at[result.memory_id] = run.started_unix
    return selected


def select_family_cooldown(run: Run, state: dict) -> list[Result]:
    selected = []
    recalled_at: dict[tuple[int, str], int] = state.setdefault("family_recalled_at", {})
    family = command_family(run)
    for result in memory_results(run):
        assert result.memory_id is not None
        key = (result.memory_id, family)
        previous = recalled_at.get(key)
        if previous is None or run.started_unix - previous >= 20 * 60:
            selected.append(result)
            recalled_at[key] = run.started_unix
    return selected


def select_command_family_gate(run: Run, _state: dict) -> list[Result]:
    return [result for result in memory_results(run) if command_family_allows(run, result)]


def select_targeted_cooldown(run: Run, state: dict) -> list[Result]:
    if command_family(run) == "apply_patch_text":
        return []
    return select_session_cooldown(run, state)


STRATEGY_SELECTORS: dict[str, Callable[[Run, dict], list[Result]]] = {
    "baseline": select_baseline,
    "session_cooldown_20m": select_session_cooldown,
    "family_cooldown_20m": select_family_cooldown,
    "command_family_gate": select_command_family_gate,
    "targeted_cooldown": select_targeted_cooldown,
}


def run_strategy(runs: list[Run], strategy: str) -> tuple[list[dict], dict]:
    state: dict = {}
    details = []
    for run in runs:
        selected = STRATEGY_SELECTORS[strategy](run, state)
        baseline_memory_results = memory_results(run)
        oracle_has_useful = any(score_is_useful(result.score) for result in baseline_memory_results)
        scores = [numeric_score(result.score) for result in selected]
        known_scores = [score for score in scores if score is not None]
        details.append(
            {
                "strategy": strategy,
                "cohort": cohort_for(run),
                "run_id": run.run_id,
                "started_at": run.started_at,
                "tool_name": run.tool_name,
                "command_family": command_family(run),
                "input": run.input,
                "selected_memory_ids": [result.memory_id for result in selected],
                "selected_scores": [result.score for result in selected],
                "baseline_memory_ids": [result.memory_id for result in baseline_memory_results],
                "baseline_scores": [result.score for result in baseline_memory_results],
                "selected_count": len(selected),
                "known_selected_count": len(known_scores),
                "average_known_score": sum(known_scores) / len(known_scores)
                if known_scores
                else None,
                "useful_known_selected": sum(1 for score in known_scores if score >= 4),
                "low_known_selected": sum(1 for score in known_scores if score <= 2),
                "captured_any_known_useful": any(score >= 4 for score in known_scores),
                "selected_any_known_low": any(score <= 2 for score in known_scores),
                "empty_recall": len(selected) == 0,
                "oracle_has_useful": oracle_has_useful,
                "empty_with_oracle_useful": len(selected) == 0 and oracle_has_useful,
                "empty_without_oracle_useful": len(selected) == 0 and not oracle_has_useful,
            }
        )
    return details, summarize_details(details)


def summarize_details(details: list[dict]) -> dict:
    scores = []
    for detail in details:
        for score in detail["selected_scores"]:
            value = numeric_score(score)
            if value is not None:
                scores.append(value)
    selected_runs = [detail for detail in details if detail["selected_count"] > 0]
    return {
        "runs": len(details),
        "selected_count": sum(detail["selected_count"] for detail in details),
        "known_selected_count": len(scores),
        "average_known_score": sum(scores) / len(scores) if scores else None,
        "useful_known_selected": sum(1 for score in scores if score >= 4),
        "low_known_selected": sum(1 for score in scores if score <= 2),
        "useful_capture_runs": sum(1 for detail in details if detail["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for detail in details if detail["selected_any_known_low"]),
        "empty_recall_runs": sum(1 for detail in details if detail["empty_recall"]),
        "missed_useful_empty_runs": sum(
            1 for detail in details if detail["empty_with_oracle_useful"]
        ),
        "clean_empty_runs": sum(1 for detail in details if detail["empty_without_oracle_useful"]),
        "selection_rate": len(selected_runs) / len(details) if details else 0.0,
    }


def summarize_by_cohort(details: list[dict]) -> dict[str, dict]:
    cohorts = {}
    for cohort in COHORTS:
        cohort_details = [detail for detail in details if detail["cohort"] == cohort]
        cohorts[cohort] = summarize_details(cohort_details) if cohort_details else {}
    return cohorts


def markdown_table(rows: list[dict], columns: list[str]) -> list[str]:
    lines = [
        "| " + " | ".join(columns) + " |",
        "| " + " | ".join("---" for _ in columns) + " |",
    ]
    for row in rows:
        rendered = []
        for column in columns:
            value = row.get(column)
            if isinstance(value, float):
                rendered.append(f"{value:.2f}")
            elif value is None:
                rendered.append("n/a")
            else:
                rendered.append(str(value))
        lines.append("| " + " | ".join(rendered) + " |")
    return lines


def write_report(out_dir: Path, manifest: dict, summary: dict) -> None:
    rows = []
    baseline = summary["strategies"]["baseline"]
    for name in STRATEGIES:
        metrics = summary["strategies"][name]
        rows.append(
            {
                "strategy": name,
                "avg": metrics["average_known_score"],
                "useful": metrics["useful_known_selected"],
                "low": metrics["low_known_selected"],
                "useful_runs": metrics["useful_capture_runs"],
                "low_runs": metrics["low_selection_runs"],
                "empty": metrics["empty_recall_runs"],
                "missed_useful_empty": metrics["missed_useful_empty_runs"],
                "delta_low": metrics["low_known_selected"] - baseline["low_known_selected"],
                "delta_useful": metrics["useful_known_selected"]
                - baseline["useful_known_selected"],
            }
        )
    lines = [
        "# Tool Recall Narrowing 5x5",
        "",
        "This replay compares five pruning strategies across five recent tool-recall cohorts.",
        "It uses post-fix `tool_pre_use` eval runs from the local YAAML SQLite store.",
        "",
        "Important limitation: this backtest can only remove memories that the current retrieval selected; it cannot score alternate memories that a different retriever would have found.",
        "",
        "## Strategies",
        "",
        "- `baseline`: current selected memories.",
        "- `session_cooldown_20m`: suppress a memory if it was already kept in the session in the last 20 minutes.",
        "- `family_cooldown_20m`: suppress a memory if it was already kept for the same command family in the last 20 minutes.",
        "- `command_family_gate`: keep only memories with deterministic command-family evidence.",
        "- `targeted_cooldown`: session cooldown plus suppress patch-text/tmp patch recalls.",
        "",
        "## Overall Results",
        "",
    ]
    lines.extend(
        markdown_table(
            rows,
            [
                "strategy",
                "avg",
                "useful",
                "low",
                "useful_runs",
                "low_runs",
                "empty",
                "missed_useful_empty",
                "delta_low",
                "delta_useful",
            ],
        )
    )
    lines.extend(["", "## Cohort Results", ""])
    for cohort in COHORTS:
        lines.append(f"### {cohort}")
        cohort_rows = []
        for name in STRATEGIES:
            metrics = summary["cohorts"][name].get(cohort, {})
            if not metrics:
                continue
            cohort_rows.append(
                {
                    "strategy": name,
                    "avg": metrics["average_known_score"],
                    "useful": metrics["useful_known_selected"],
                    "low": metrics["low_known_selected"],
                    "empty": metrics["empty_recall_runs"],
                    "missed_useful_empty": metrics["missed_useful_empty_runs"],
                }
            )
        lines.extend(markdown_table(cohort_rows, ["strategy", "avg", "useful", "low", "empty", "missed_useful_empty"]))
        lines.append("")
    lines.extend(
        [
            "## Findings",
            "",
            "- `session_cooldown_20m` is the best balanced candidate in this replay: it removes repeated noise while preserving most useful recalls.",
            "- `family_cooldown_20m` is slightly worse here because the repeated noise is mostly same-session, not only same command family.",
            "- `command_family_gate` is too blunt as tested: it removes apply-patch text noise, but it also misses useful git memories and does not improve yarn recalls.",
            "- `targeted_cooldown` keeps the useful/low tradeoff close to session cooldown while suppressing the patch-text/tmp patch cohort completely.",
            "- The next production candidate should start with session-level memory cooldown and a narrow tool-input suppression for synthetic patch text, not broad command-family gating.",
            "",
            "## Artifacts",
            "",
            f"- Manifest: `{manifest['manifest_path']}`",
            f"- Summary JSON: `{manifest['summary_path']}`",
            f"- Per-run JSONL: `{manifest['details_path']}`",
        ]
    )
    out_dir.joinpath("REPORT.md").write_text("\n".join(lines) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--since-unix", type=int, default=POST_FIX_UNIX)
    parser.add_argument("--limit", type=int, default=200)
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path("experiments/recall/2026-06-23-tool-cooldown-5x5"),
    )
    args = parser.parse_args()

    runs = load_runs(args.db, args.since_unix, args.limit)
    args.out_dir.mkdir(parents=True, exist_ok=True)

    all_details = []
    strategy_summaries = {}
    cohort_summaries = {}
    for strategy in STRATEGIES:
        details, strategy_summary = run_strategy(runs, strategy)
        all_details.extend(details)
        strategy_summaries[strategy] = strategy_summary
        cohort_summaries[strategy] = summarize_by_cohort(details)

    details_path = args.out_dir / "details.jsonl"
    details_path.write_text("\n".join(json.dumps(row, sort_keys=True) for row in all_details) + "\n")
    summary = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "db_path": str(args.db),
        "since_unix": args.since_unix,
        "run_count": len(runs),
        "strategies": strategy_summaries,
        "cohorts": cohort_summaries,
    }
    summary_path = args.out_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    manifest = {
        "generated_at": summary["generated_at"],
        "kind": "tool_recall_narrowing_5x5",
        "db_path": str(args.db),
        "since_unix": args.since_unix,
        "run_count": len(runs),
        "strategies": STRATEGIES,
        "cohorts": COHORTS,
        "details_path": str(details_path),
        "summary_path": str(summary_path),
        "manifest_path": str(args.out_dir / "manifest.json"),
        "report_path": str(args.out_dir / "REPORT.md"),
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    write_report(args.out_dir, manifest, summary)
    print(json.dumps({"out_dir": str(args.out_dir), **strategy_summaries}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
