#!/usr/bin/env python3
"""Report forward exposure for formation-time activation conditions.

This is a forward memory-write experiment reporter. It reads the live YAAML
database, assigns memories to activation-condition cohorts, joins downstream
recall eval outcomes, and writes the artifact set required by
experiments/recall/README.md.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sqlite3
from pathlib import Path
from typing import Any


Row = dict[str, Any]


def expand_path(path: str) -> Path:
    return Path(path).expanduser()


def now_utc() -> str:
    return dt.datetime.now(dt.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def timestamp_seconds(value: str | None) -> int | None:
    if not value:
        return None
    if value.startswith("unix:"):
        try:
            return int(value.split(":", 1)[1])
        except ValueError:
            return None
    try:
        normalized = value.replace("Z", "+00:00")
        return int(dt.datetime.fromisoformat(normalized).timestamp())
    except ValueError:
        return None


def read_json_array(value: str | None) -> list[Any]:
    if not value:
        return []
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return []
    return parsed if isinstance(parsed, list) else []


def write_json(path: Path, value: Row) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def connect_readonly(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def table_exists(conn: sqlite3.Connection, name: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        (name,),
    ).fetchone()
    return row is not None


def load_memories(conn: sqlite3.Connection, has_activation_table: bool) -> list[Row]:
    activation_join = ""
    activation_columns = "'[]' AS activation_triggers_json, '[]' AS activation_anti_triggers_json, NULL AS activation_updated_at"
    if has_activation_table:
        activation_join = """
        LEFT JOIN memory_activation_conditions ac ON ac.memory_id = m.id
        """
        activation_columns = """
        COALESCE(ac.activation_triggers_json, '[]') AS activation_triggers_json,
        COALESCE(ac.activation_anti_triggers_json, '[]') AS activation_anti_triggers_json,
        ac.updated_at AS activation_updated_at
        """
    rows = conn.execute(
        f"""
        SELECT
          m.id,
          m.title,
          m.body,
          m.scope,
          m.memory_kind,
          m.validity,
          m.task_keys,
          m.source_turn_refs,
          m.created_at,
          m.updated_at,
          m.is_active,
          m.session_id,
          m.project_id,
          m.origin_segment_id,
          m.superseded_by_memory_id,
          {activation_columns}
        FROM memories m
        {activation_join}
        ORDER BY m.id
        """
    ).fetchall()
    return [dict(row) for row in rows]


def load_eval_outcomes(conn: sqlite3.Connection) -> dict[int, Row]:
    outcomes: dict[int, Row] = {}
    if not table_exists(conn, "eval_results"):
        return outcomes
    rows = conn.execute(
        """
        SELECT
          er.memory_id,
          er.judge_score,
          er.rationale,
          er.created_at,
          er.eval_run_id,
          r.recall_origin
        FROM eval_results er
        LEFT JOIN eval_runs r ON r.id = er.eval_run_id
        WHERE er.memory_id IS NOT NULL
        ORDER BY er.id
        """
    ).fetchall()
    for row in rows:
        memory_id = int(row["memory_id"])
        outcome = outcomes.setdefault(
            memory_id,
            {
                "selected_count": 0,
                "judged_count": 0,
                "useful_count": 0,
                "low_count": 0,
                "neutral_count": 0,
                "insufficient_context_count": 0,
                "numeric_scores": [],
                "latest_eval_run_id": None,
                "latest_eval_created_at": None,
                "latest_eval_score": None,
                "recall_origins": {},
            },
        )
        outcome["selected_count"] += 1
        score = row["judge_score"]
        origin = row["recall_origin"] or "unknown"
        outcome["recall_origins"][origin] = outcome["recall_origins"].get(origin, 0) + 1
        if score == "insufficient_context":
            outcome["insufficient_context_count"] += 1
        else:
            try:
                numeric_score = int(str(score))
            except (TypeError, ValueError):
                numeric_score = None
            if numeric_score is not None and 1 <= numeric_score <= 5:
                outcome["judged_count"] += 1
                outcome["numeric_scores"].append(numeric_score)
                if numeric_score >= 4:
                    outcome["useful_count"] += 1
                elif numeric_score <= 2:
                    outcome["low_count"] += 1
                else:
                    outcome["neutral_count"] += 1
        if outcome["latest_eval_run_id"] is None or int(row["eval_run_id"]) >= int(
            outcome["latest_eval_run_id"]
        ):
            outcome["latest_eval_run_id"] = row["eval_run_id"]
            outcome["latest_eval_created_at"] = row["created_at"]
            outcome["latest_eval_score"] = score
    for outcome in outcomes.values():
        scores = outcome.pop("numeric_scores")
        outcome["average_score"] = sum(scores) / len(scores) if scores else None
    return outcomes


def pending_condition_eval_tasks(
    conn: sqlite3.Connection,
    condition_memory_ids: set[int],
    now_seconds: int,
) -> Row:
    if not condition_memory_ids or not table_exists(conn, "tasks"):
        return {
            "task_count": 0,
            "due_task_count": 0,
            "with_later_turns_count": 0,
            "selected_condition_memory_count": 0,
            "selected_condition_memory_ids": [],
            "tasks": [],
        }
    rows = conn.execute(
        """
        SELECT id, status, next_run_at, payload_json
        FROM tasks
        WHERE kind = 'recall_eval'
          AND status IN ('queued', 'running', 'parked')
        ORDER BY id
        """
    ).fetchall()
    tasks = []
    selected_condition_ids: set[int] = set()
    for row in rows:
        try:
            payload = json.loads(row["payload_json"] or "{}")
        except json.JSONDecodeError:
            continue
        memory_ids = {
            int(memory_id)
            for memory_id in payload.get("memory_ids") or []
            if str(memory_id).lstrip("-").isdigit()
        }
        condition_ids = sorted(memory_ids & condition_memory_ids)
        if not condition_ids:
            continue
        selected_condition_ids.update(condition_ids)
        due_at_seconds = timestamp_seconds(row["next_run_at"])
        session_id = payload.get("session_id")
        turn_ordinal = payload.get("turn_ordinal")
        later_turns = 0
        if isinstance(session_id, str) and isinstance(turn_ordinal, int):
            later_turns = int(
                conn.execute(
                    """
                    SELECT COUNT(*)
                    FROM turns
                    WHERE session_id = ?1
                      AND ordinal > ?2
                      AND status = 'completed'
                    """,
                    (session_id, turn_ordinal),
                ).fetchone()[0]
            )
        tasks.append(
            {
                "task_id": row["id"],
                "status": row["status"],
                "next_run_at": row["next_run_at"],
                "due": due_at_seconds is None or due_at_seconds <= now_seconds,
                "session_id": session_id,
                "turn_ordinal": turn_ordinal,
                "later_completed_turns": later_turns,
                "condition_memory_ids": condition_ids,
            }
        )
    return {
        "task_count": len(tasks),
        "due_task_count": sum(1 for task in tasks if task["due"]),
        "with_later_turns_count": sum(
            1 for task in tasks if task["later_completed_turns"] > 0
        ),
        "selected_condition_memory_count": len(selected_condition_ids),
        "selected_condition_memory_ids": sorted(selected_condition_ids),
        "tasks": tasks[:20],
    }


def cohort_for_memory(memory: Row, policy_start_seconds: int | None) -> str:
    has_conditions = bool(memory["activation_triggers"] or memory["activation_anti_triggers"])
    created_seconds = timestamp_seconds(memory.get("created_at"))
    if has_conditions:
        return "new_policy_activation_conditions"
    if policy_start_seconds is not None and created_seconds is not None:
        if created_seconds >= policy_start_seconds:
            return "new_policy_no_activation_conditions"
    return "comparison_no_activation_conditions"


def cohort_row(memory: Row, outcome: Row | None, policy_start_seconds: int | None) -> Row:
    triggers = read_json_array(memory.get("activation_triggers_json"))
    anti_triggers = read_json_array(memory.get("activation_anti_triggers_json"))
    memory["activation_triggers"] = [str(value) for value in triggers]
    memory["activation_anti_triggers"] = [str(value) for value in anti_triggers]
    source_refs = read_json_array(memory.get("source_turn_refs"))
    task_keys = read_json_array(memory.get("task_keys"))
    outcome = outcome or {}
    return {
        "memory_id": memory["id"],
        "cohort": cohort_for_memory(memory, policy_start_seconds),
        "title": memory["title"],
        "scope": memory["scope"],
        "kind": memory["memory_kind"],
        "validity": memory["validity"],
        "is_active": bool(memory["is_active"]),
        "session_id": memory["session_id"],
        "project_id": memory["project_id"],
        "created_at": memory["created_at"],
        "updated_at": memory["updated_at"],
        "activation_updated_at": memory["activation_updated_at"],
        "activation_triggers": memory["activation_triggers"],
        "activation_anti_triggers": memory["activation_anti_triggers"],
        "activation_trigger_count": len(memory["activation_triggers"]),
        "activation_anti_trigger_count": len(memory["activation_anti_triggers"]),
        "task_keys": task_keys,
        "source_turn_ref_count": len(source_refs),
        "origin_segment_id": memory["origin_segment_id"],
        "superseded_by_memory_id": memory["superseded_by_memory_id"],
        "selected_count": outcome.get("selected_count", 0),
        "judged_count": outcome.get("judged_count", 0),
        "useful_count": outcome.get("useful_count", 0),
        "low_count": outcome.get("low_count", 0),
        "neutral_count": outcome.get("neutral_count", 0),
        "insufficient_context_count": outcome.get("insufficient_context_count", 0),
        "average_score": outcome.get("average_score"),
        "latest_eval_run_id": outcome.get("latest_eval_run_id"),
        "latest_eval_created_at": outcome.get("latest_eval_created_at"),
        "latest_eval_score": outcome.get("latest_eval_score"),
        "recall_origins": outcome.get("recall_origins", {}),
        "source_faithfulness_score": None,
        "durability_classification_correct": None,
        "specificity_actionability_score": None,
        "false_positive_creation": None,
        "missed_creation_evidence": None,
    }


def summarize_cohort(rows: list[Row]) -> Row:
    judged_scores = [
        float(row["average_score"])
        for row in rows
        if row.get("average_score") is not None and row.get("judged_count", 0) > 0
    ]
    return {
        "memory_count": len(rows),
        "active_memory_count": sum(1 for row in rows if row["is_active"]),
        "condition_memory_count": sum(
            1
            for row in rows
            if row["activation_trigger_count"] > 0 or row["activation_anti_trigger_count"] > 0
        ),
        "condition_coverage_rate": (
            sum(
                1
                for row in rows
                if row["activation_trigger_count"] > 0 or row["activation_anti_trigger_count"] > 0
            )
            / len(rows)
            if rows
            else None
        ),
        "selected_count": sum(row["selected_count"] for row in rows),
        "judged_count": sum(row["judged_count"] for row in rows),
        "useful_count": sum(row["useful_count"] for row in rows),
        "low_count": sum(row["low_count"] for row in rows),
        "insufficient_context_count": sum(row["insufficient_context_count"] for row in rows),
        "average_memory_score": sum(judged_scores) / len(judged_scores)
        if judged_scores
        else None,
        "created_at_min": min((row["created_at"] for row in rows), default=None),
        "created_at_max": max((row["created_at"] for row in rows), default=None),
    }


def condition_examples(rows: list[Row], limit: int = 8) -> list[Row]:
    examples = []
    for row in rows:
        if row["activation_triggers"] or row["activation_anti_triggers"]:
            examples.append(
                {
                    "memory_id": row["memory_id"],
                    "title": row["title"],
                    "activation_triggers": row["activation_triggers"],
                    "activation_anti_triggers": row["activation_anti_triggers"],
                }
            )
        if len(examples) >= limit:
            break
    return examples


def downstream_eval_examples(rows: list[Row], limit: int = 8) -> list[Row]:
    examples = []
    for row in sorted(
        (row for row in rows if row.get("judged_count", 0) > 0),
        key=lambda row: (row.get("latest_eval_run_id") or 0, row["memory_id"]),
        reverse=True,
    ):
        examples.append(
            {
                "memory_id": row["memory_id"],
                "title": row["title"],
                "latest_eval_run_id": row["latest_eval_run_id"],
                "latest_eval_score": row["latest_eval_score"],
                "selected_count": row["selected_count"],
                "judged_count": row["judged_count"],
                "useful_count": row["useful_count"],
                "low_count": row["low_count"],
            }
        )
        if len(examples) >= limit:
            break
    return examples


def report_text(summary: Row, manifest: Row) -> str:
    lines = [
        "# Formation-Time Activation Conditions Forward Readiness",
        "",
        "## Question",
        "",
        "Do newly written memories have activation-condition metadata, and is there enough downstream recall-eval exposure to evaluate the policy?",
        "",
        "## Exposure",
        "",
        f"- Generated at: `{manifest['generated_at']}`",
        f"- Database: `{manifest['db_path']}`",
        f"- Policy start: `{manifest['policy_start'] or 'not provided'}`",
        f"- Activation table present: `{manifest['activation_table_present']}`",
        "",
        "## Cohorts",
        "",
    ]
    for cohort, metrics in summary["cohorts"].items():
        coverage = metrics["condition_coverage_rate"]
        coverage_text = "n/a" if coverage is None else f"{coverage:.1%}"
        avg = metrics["average_memory_score"]
        avg_text = "n/a" if avg is None else f"{avg:.2f}"
        lines.extend(
            [
                f"### {cohort}",
                "",
                f"- Memories: {metrics['memory_count']} ({metrics['active_memory_count']} active)",
                f"- Condition coverage: {metrics['condition_memory_count']}/{metrics['memory_count']} ({coverage_text})",
                f"- Downstream eval rows: selected={metrics['selected_count']}, judged={metrics['judged_count']}, useful={metrics['useful_count']}, low={metrics['low_count']}, insufficient_context={metrics['insufficient_context_count']}",
                f"- Average memory score: {avg_text}",
                f"- Creation window: `{metrics['created_at_min']}` to `{metrics['created_at_max']}`",
                "",
            ]
        )
    lines.extend(
        [
            "## Activation Examples",
            "",
        ]
    )
    if summary["condition_examples"]:
        for example in summary["condition_examples"]:
            lines.append(
                f"- Memory {example['memory_id']} `{example['title']}`: triggers={example['activation_triggers']}; anti_triggers={example['activation_anti_triggers']}"
            )
    else:
        lines.append("- No activation-condition rows were observed.")
    lines.extend(
        [
            "",
            "## Downstream Eval Examples",
            "",
        ]
    )
    if summary["downstream_eval_examples"]:
        for example in summary["downstream_eval_examples"]:
            lines.append(
                f"- Memory {example['memory_id']} `{example['title']}`: latest_score={example['latest_eval_score']}, latest_run={example['latest_eval_run_id']}, selected={example['selected_count']}, judged={example['judged_count']}, useful={example['useful_count']}, low={example['low_count']}"
            )
    else:
        lines.append("- No activation-condition memories have judged downstream evals yet.")
    lines.extend(
        [
            "",
            "## Readiness",
            "",
            f"- Minimum condition memories: {summary['readiness']['minimum_condition_memories']}",
            f"- Minimum judged downstream evals: {summary['readiness']['minimum_judged_evals']}",
            f"- Observed condition memories: {summary['readiness']['observed_condition_memories']}",
            f"- Observed judged downstream evals for condition memories: {summary['readiness']['observed_condition_judged_evals']}",
            f"- Pending recall-eval tasks selecting condition memories: {summary['pending_downstream_eval_tasks']['task_count']}",
            f"- Pending condition eval tasks with later turns: {summary['pending_downstream_eval_tasks']['with_later_turns_count']}",
            f"- Ready for decision: `{summary['readiness']['ready_for_decision']}`",
            "",
            "## Decision",
            "",
            summary["decision"],
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", default="~/.yaaml/yaaml.db")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path("experiments/recall/2026-07-07-activation-conditions-forward-readiness"),
    )
    parser.add_argument(
        "--policy-start",
        help="Optional start timestamp for the new policy, e.g. unix:1783407600.",
    )
    parser.add_argument("--min-condition-memories", type=int, default=20)
    parser.add_argument("--min-judged-evals", type=int, default=20)
    args = parser.parse_args()

    db_path = expand_path(args.db)
    policy_start_seconds = timestamp_seconds(args.policy_start)
    if args.policy_start and policy_start_seconds is None:
        raise SystemExit(f"could not parse --policy-start: {args.policy_start}")

    conn = connect_readonly(db_path)
    has_activation_table = table_exists(conn, "memory_activation_conditions")
    memories = load_memories(conn, has_activation_table)
    outcomes = load_eval_outcomes(conn)
    rows = [
        cohort_row(memory, outcomes.get(int(memory["id"])), policy_start_seconds)
        for memory in memories
    ]

    by_cohort: dict[str, list[Row]] = {}
    for row in rows:
        by_cohort.setdefault(row["cohort"], []).append(row)
    cohorts = {cohort: summarize_cohort(cohort_rows) for cohort, cohort_rows in by_cohort.items()}
    condition_rows = [
        row
        for row in rows
        if row["activation_trigger_count"] > 0 or row["activation_anti_trigger_count"] > 0
    ]
    condition_memory_ids = {int(row["memory_id"]) for row in condition_rows}
    observed_condition_judged_evals = sum(row["judged_count"] for row in condition_rows)
    now_seconds = int(dt.datetime.now(dt.UTC).timestamp())
    pending_eval_tasks = pending_condition_eval_tasks(conn, condition_memory_ids, now_seconds)
    ready = (
        len(condition_rows) >= args.min_condition_memories
        and observed_condition_judged_evals >= args.min_judged_evals
    )
    decision = (
        "Decision-ready: enough activation-condition memories and downstream judged evals exist for a policy readout."
        if ready
        else "Do not retire the backlog item yet: the activation-condition cohort has not accumulated enough forward exposure for a decision."
    )
    summary = {
        "total_memories": len(rows),
        "cohorts": cohorts,
        "condition_examples": condition_examples(condition_rows),
        "downstream_eval_examples": downstream_eval_examples(condition_rows),
        "pending_downstream_eval_tasks": pending_eval_tasks,
        "readiness": {
            "minimum_condition_memories": args.min_condition_memories,
            "minimum_judged_evals": args.min_judged_evals,
            "observed_condition_memories": len(condition_rows),
            "observed_condition_judged_evals": observed_condition_judged_evals,
            "ready_for_decision": ready,
        },
        "decision": decision,
    }
    generated_at = now_utc()
    manifest = {
        "experiment": "formation-time activation conditions forward readiness",
        "generated_at": generated_at,
        "db_path": str(db_path),
        "policy_start": args.policy_start,
        "activation_table_present": has_activation_table,
        "cohort_definitions": {
            "new_policy_activation_conditions": "memories with non-empty memory_activation_conditions trigger or anti-trigger JSON",
            "new_policy_no_activation_conditions": "memories created at or after policy_start without activation-condition metadata",
            "comparison_no_activation_conditions": "memories without activation-condition metadata created before policy_start, or all such memories when policy_start is omitted",
        },
        "exposure_window": {
            cohort: {
                "created_at_min": metrics["created_at_min"],
                "created_at_max": metrics["created_at_max"],
            }
            for cohort, metrics in cohorts.items()
        },
        "outputs": {
            "cohorts": "cohorts.jsonl",
            "summary": "summary.json",
            "report": "REPORT.md",
        },
    }

    args.out_dir.mkdir(parents=True, exist_ok=True)
    write_json(args.out_dir / "manifest.json", manifest)
    write_json(args.out_dir / "summary.json", summary)
    write_jsonl(args.out_dir / "cohorts.jsonl", rows)
    (args.out_dir / "REPORT.md").write_text(report_text(summary, manifest))
    print(json.dumps(summary["readiness"], indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
