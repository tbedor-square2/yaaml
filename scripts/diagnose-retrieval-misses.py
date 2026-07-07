#!/usr/bin/env python3
"""Diagnose known-useful recall memories that are absent from the candidate pool."""

from __future__ import annotations

import argparse
import collections
import json
import re
import sqlite3
from pathlib import Path
from typing import Any

Row = dict[str, Any]

IDENTITY_KEY_PREFIXES = {
    "branch",
    "flag",
    "generator",
    "metric",
    "path",
    "pr",
    "sentry",
    "signal",
    "target",
    "task",
    "ticket",
    "trigger",
}

STOPWORDS = {
    "about",
    "after",
    "again",
    "also",
    "and",
    "are",
    "because",
    "before",
    "being",
    "but",
    "can",
    "code",
    "commit",
    "could",
    "for",
    "from",
    "has",
    "have",
    "into",
    "just",
    "like",
    "more",
    "now",
    "only",
    "out",
    "over",
    "run",
    "same",
    "should",
    "that",
    "the",
    "then",
    "there",
    "this",
    "tool",
    "use",
    "user",
    "when",
    "with",
    "work",
    "would",
}


def load_json(path: Path) -> Row:
    return json.loads(path.read_text())


def read_jsonl(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def memory_rows(conn: sqlite3.Connection, memory_ids: set[int]) -> dict[int, Row]:
    if not memory_ids:
        return {}
    placeholders = ",".join("?" for _ in memory_ids)
    rows = conn.execute(
        f"""
        SELECT id, title, body, memory_kind, task_keys, project_id, is_active,
               validity, superseded_by_memory_id
        FROM memories
        WHERE id IN ({placeholders})
        """,
        sorted(memory_ids),
    ).fetchall()
    return {int(row["id"]): dict(row) for row in rows}


def parse_task_keys(raw: str | None) -> list[str]:
    if not raw:
        return []
    try:
        values = json.loads(raw)
    except json.JSONDecodeError:
        return []
    return [str(value).lower() for value in values if str(value).strip()]


def identity_keys(keys: list[str]) -> list[str]:
    return [
        key
        for key in keys
        if key.split(":", 1)[0].lower() in IDENTITY_KEY_PREFIXES
    ]


def query_identity_keys(query_text: str) -> set[str]:
    keys: set[str] = set()
    lower = query_text.lower()
    for prefix in IDENTITY_KEY_PREFIXES:
        for match in re.finditer(rf"\b{re.escape(prefix)}:([^\s,;)]+)", lower):
            keys.add(f"{prefix}:{match.group(1).strip()}")
    for match in re.finditer(r"\bpr(?:\s+|#)(\d{3,})\b", lower):
        keys.add(f"pr:{match.group(1)}")
    for match in re.finditer(r"\b(branch|flag|target|task|metric)\s+([a-z0-9][a-z0-9._/-]+)", lower):
        keys.add(f"{match.group(1)}:{match.group(2).rstrip('.,')}")
    for match in re.finditer(r"\b(?:crates|scripts|experiments|src|tests)/[a-z0-9._/-]+", lower):
        keys.add(f"path:{match.group(0).rstrip('.,')}")
    return keys


def tokens(text: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[a-z0-9][a-z0-9_-]{2,}", text.lower())
        if token not in STOPWORDS
    }


def jaccard(left: set[str], right: set[str]) -> float:
    if not left or not right:
        return 0.0
    return len(left & right) / len(left | right)


def query_noise(query_text: str) -> Row:
    lines = [line.strip() for line in query_text.splitlines() if line.strip()]
    if not lines:
        return {
            "line_count": 0,
            "duplicate_line_ratio": 0.0,
            "assistant_tool_line_ratio": 0.0,
            "user_line_ratio": 0.0,
        }
    duplicate_line_ratio = 1.0 - (len(set(lines)) / len(lines))
    assistant_tool_lines = sum(
        1
        for line in lines
        if line.startswith("assistant:")
        or line.startswith("tool call:")
        or line.startswith("tool result:")
    )
    user_lines = sum(1 for line in lines if line.startswith("user:"))
    return {
        "line_count": len(lines),
        "duplicate_line_ratio": duplicate_line_ratio,
        "assistant_tool_line_ratio": assistant_tool_lines / len(lines),
        "user_line_ratio": user_lines / len(lines),
    }


def classify(
    *,
    memory: Row | None,
    query_text: str,
    query_project_id: str | None,
    query_keys: set[str],
    lexical_overlap: float,
    noise: Row,
) -> str:
    if memory is None:
        return "memory_missing_from_db"
    if not memory.get("is_active"):
        return "memory_inactive"
    if memory.get("superseded_by_memory_id") is not None:
        return "memory_superseded"
    if memory.get("project_id") and query_project_id and memory["project_id"] != query_project_id:
        return "cross_project_memory"
    memory_keys = parse_task_keys(memory.get("task_keys"))
    memory_identity = set(identity_keys(memory_keys))
    if memory_identity and not (memory_identity & query_keys):
        return "missing_identity_key_in_query"
    if noise["duplicate_line_ratio"] >= 0.35 or noise["assistant_tool_line_ratio"] >= 0.75:
        return "query_noise_dominates"
    if lexical_overlap < 0.03:
        return "low_lexical_overlap"
    if not (set(memory_keys) & query_keys):
        return "no_task_key_overlap"
    return "unclassified_semantic_miss"


def build_cases(args: argparse.Namespace) -> tuple[list[Row], Row]:
    pool_cases = read_jsonl(args.cases_jsonl)
    retrieval_cases = [
        case for case in pool_cases if case.get("miss_class") == "missing_due_to_retrieval"
    ]
    useful_ids = {
        int(memory_id)
        for case in pool_cases
        for memory_id in case.get("oracle_useful_memory_ids") or []
    }
    conn = sqlite3.connect(args.db)
    conn.row_factory = sqlite3.Row
    memories = memory_rows(conn, useful_ids)

    active_useful_anchor_count = 0
    active_useful_in_pool_count = 0
    active_useful_selected_count = 0
    active_missing_due_to_retrieval = 0
    active_missing_due_to_selection = 0
    for case in pool_cases:
        useful_active_ids = {
            int(memory_id)
            for memory_id in case.get("oracle_useful_memory_ids") or []
            if current_active_memory(memories.get(int(memory_id)))
        }
        if not useful_active_ids:
            continue
        active_useful_anchor_count += 1
        ranking_ids = {int(memory_id) for memory_id in case.get("ranking_memory_ids") or []}
        selected_ids = {int(memory_id) for memory_id in case.get("selected_memory_ids") or []}
        useful_in_pool = bool(useful_active_ids & ranking_ids)
        useful_selected = bool(useful_active_ids & selected_ids)
        active_useful_in_pool_count += int(useful_in_pool)
        active_useful_selected_count += int(useful_selected)
        if not useful_selected:
            if useful_in_pool:
                active_missing_due_to_selection += 1
            else:
                active_missing_due_to_retrieval += 1

    rows: list[Row] = []
    cause_counts: collections.Counter[str] = collections.Counter()
    kind_counts: collections.Counter[str] = collections.Counter()
    project_counts: collections.Counter[str] = collections.Counter()
    for case in retrieval_cases:
        run_id = int(case["run_id"])
        recall = load_json(args.recall_dir / f"{args.strategy_label}.recall-{run_id}.json")
        query_text = str(recall.get("query_text") or "")
        query_project_id = recall.get("project_id")
        query_keys = query_identity_keys(query_text)
        query_token_set = tokens(query_text)
        noise = query_noise(query_text)
        for memory_id in case.get("oracle_useful_memory_ids") or []:
            memory = memories.get(int(memory_id))
            memory_text = ""
            memory_keys: list[str] = []
            memory_identity: list[str] = []
            if memory:
                memory_keys = parse_task_keys(memory.get("task_keys"))
                memory_identity = identity_keys(memory_keys)
                memory_text = "\n".join(
                    [
                        str(memory.get("title") or ""),
                        str(memory.get("body") or ""),
                        "\n".join(memory_keys),
                    ]
                )
            lexical_overlap = jaccard(query_token_set, tokens(memory_text))
            cause = classify(
                memory=memory,
                query_text=query_text,
                query_project_id=query_project_id,
                query_keys=query_keys,
                lexical_overlap=lexical_overlap,
                noise=noise,
            )
            cause_counts[cause] += 1
            if memory:
                kind_counts[str(memory.get("memory_kind") or "unknown")] += 1
                project_counts[
                    "same_project"
                    if memory.get("project_id") == query_project_id
                    else "cross_or_missing_project"
                ] += 1
            rows.append(
                {
                    "run_id": run_id,
                    "case": case.get("case"),
                    "session_id": case.get("session_id"),
                    "turn_ordinal": case.get("turn_ordinal"),
                    "memory_id": int(memory_id),
                    "cause": cause,
                    "memory_kind": memory.get("memory_kind") if memory else None,
                    "memory_title": memory.get("title") if memory else None,
                    "memory_project_id": memory.get("project_id") if memory else None,
                    "query_project_id": query_project_id,
                    "memory_identity_keys": memory_identity,
                    "query_identity_keys": sorted(query_keys),
                    "identity_key_overlap": sorted(set(memory_identity) & query_keys),
                    "lexical_overlap": round(lexical_overlap, 4),
                    "query_noise": noise,
                    "ranking_count": len(recall.get("ranking") or []),
                    "selected_memory_ids": recall.get("selected_memory_ids") or [],
                }
            )
    summary: Row = {
        "retrieval_miss_anchors": len(retrieval_cases),
        "retrieval_miss_memory_instances": len(rows),
        "active_oracle_useful_anchors": active_useful_anchor_count,
        "active_known_useful_in_pool": active_useful_in_pool_count,
        "active_known_useful_selected": active_useful_selected_count,
        "active_missing_due_to_retrieval": active_missing_due_to_retrieval,
        "active_missing_due_to_selection": active_missing_due_to_selection,
        "cause_counts": dict(cause_counts.most_common()),
        "memory_kind_counts": dict(kind_counts.most_common()),
        "project_match_counts": dict(project_counts.most_common()),
    }
    return rows, summary


def current_active_memory(memory: Row | None) -> bool:
    return bool(
        memory
        and memory.get("is_active")
        and memory.get("superseded_by_memory_id") is None
    )


def write_report(path: Path, summary: Row, manifest: Row) -> None:
    cause_lines = [
        f"- `{cause}`: {count}"
        for cause, count in summary["cause_counts"].items()
    ]
    kind_lines = [
        f"- `{kind}`: {count}"
        for kind, count in summary["memory_kind_counts"].items()
    ]
    if summary["cause_counts"].get("memory_inactive", 0) > (
        summary["retrieval_miss_memory_instances"] / 2
    ):
        interpretation = (
            "Most raw retrieval misses are currently inactive memories, which production recall intentionally excludes. "
            "On active oracle-useful anchors, selection is the larger observed gap: useful memories are usually present in the widened pool but not selected."
        )
    elif summary["active_missing_due_to_retrieval"] > summary["active_missing_due_to_selection"]:
        interpretation = (
            "After excluding inactive memories, retrieval remains the larger active miss class. Query synthesis should target the active retrieval misses before more selection work."
        )
    else:
        interpretation = (
            "After excluding inactive memories, selection is the larger active miss class. Candidate generation has room to improve, but ranking/selection should be prioritized next."
        )
    path.write_text(
        "\n".join(
            [
                "# Retrieval Miss Diagnosis",
                "",
                f"Date: {manifest['date']}",
                f"Cases: `{manifest['cases_jsonl']}`",
                f"Recall directory: `{manifest['recall_dir']}`",
                "",
                "## Summary",
                "",
                f"- Retrieval-miss anchors: {summary['retrieval_miss_anchors']}",
                f"- Useful memory instances: {summary['retrieval_miss_memory_instances']}",
                f"- Active oracle-useful anchors: {summary['active_oracle_useful_anchors']}",
                f"- Active known-useful in pool: {summary['active_known_useful_in_pool']}",
                f"- Active known-useful selected: {summary['active_known_useful_selected']}",
                f"- Active misses due to retrieval: {summary['active_missing_due_to_retrieval']}",
                f"- Active misses due to selection: {summary['active_missing_due_to_selection']}",
                "",
                "## Cause Counts",
                "",
                *cause_lines,
                "",
                "## Memory Kinds",
                "",
                *kind_lines,
                "",
                "## Interpretation",
                "",
                interpretation,
                "",
                "## Artifacts",
                "",
                "- `manifest.json` records inputs and summary metrics.",
                "- `cases.jsonl` records per-memory miss features and assigned cause.",
                "",
            ]
        )
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases-jsonl", type=Path, required=True)
    parser.add_argument("--recall-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", required=True)
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--label", default="2026-07-06")
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()

    out_dir = args.out_dir.expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    rows, summary = build_cases(args)
    manifest = {
        "date": args.label,
        "generated_by": "scripts/diagnose-retrieval-misses.py",
        "cases_jsonl": str(args.cases_jsonl.resolve()),
        "recall_dir": str(args.recall_dir.resolve()),
        "strategy_label": args.strategy_label,
        "db": str(args.db.expanduser().resolve()),
        "summary": summary,
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (out_dir / "cases.jsonl").write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows)
    )
    write_report(out_dir / "REPORT.md", summary, manifest)
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
