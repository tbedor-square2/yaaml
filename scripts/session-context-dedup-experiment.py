#!/usr/bin/env python3
"""Replay a session-level context dedup policy over saved recall selections."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sqlite3
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from recall_experiment_stats import metric_from_rows, paired_bootstrap_deltas

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

STOPWORDS = {
    "about",
    "after",
    "also",
    "and",
    "are",
    "because",
    "been",
    "but",
    "can",
    "code",
    "current",
    "from",
    "have",
    "into",
    "just",
    "memory",
    "more",
    "only",
    "query",
    "recall",
    "should",
    "that",
    "the",
    "then",
    "there",
    "this",
    "turn",
    "user",
    "when",
    "with",
    "work",
    "would",
}


def read_jsonl(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def load_json(path: Path) -> Row:
    return json.loads(path.read_text())


def write_json(path: Path, value: Row) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def tokens(text: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[a-z0-9][a-z0-9_-]{2,}", text.lower())
        if token not in STOPWORDS and not token.isdigit()
    }


def identity_keys(text: str) -> set[str]:
    keys: set[str] = set()
    lower = text.lower()
    for prefix in [
        "branch",
        "flag",
        "generator",
        "metric",
        "module",
        "path",
        "pr",
        "service",
        "target",
        "task",
        "ticket",
        "tool",
    ]:
        for match in re.finditer(rf"\b{prefix}:([a-z0-9][a-z0-9._/-]*)", lower):
            keys.add(f"{prefix}:{match.group(1).rstrip('.,;:')}")
    for match in re.finditer(r"\bpr(?:\s+|#)(\d{2,})\b", lower):
        keys.add(f"pr:{match.group(1)}")
    for match in re.finditer(r"\b[a-z0-9_.-]+/[a-z0-9][a-z0-9._/-]*\b", lower):
        keys.add(f"path:{match.group(0).rstrip('.,;:')}")
    return keys


def hydrate_memories(db_path: Path, memory_ids: set[int]) -> dict[int, Row]:
    if not memory_ids:
        return {}
    placeholders = ",".join("?" for _ in memory_ids)
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        f"""
        SELECT id, title, body, task_keys
        FROM memories
        WHERE id IN ({placeholders})
        """,
        sorted(memory_ids),
    ).fetchall()
    memories: dict[int, Row] = {}
    for row in rows:
        memory = dict(row)
        try:
            task_keys = json.loads(memory.get("task_keys") or "[]")
        except json.JSONDecodeError:
            task_keys = []
        text = f"{memory['title']}\n{memory['body']}\n{' '.join(task_keys)}"
        memory["tokens"] = sorted(tokens(text))
        memory["identity_keys"] = sorted(set(task_keys) | identity_keys(text))
        memories[int(memory["id"])] = memory
    return memories


def oracle_score_map(oracle: Row) -> dict[int, int]:
    scores: dict[int, int] = {}
    for result in oracle.get("results") or []:
        memory_id = result.get("memory_id")
        if memory_id is None:
            continue
        try:
            scores[int(memory_id)] = int(str(result.get("judge_score")))
        except ValueError:
            continue
    return scores


def avg(values: list[int]) -> float | None:
    return sum(values) / len(values) if values else None


def details_row(label: str, baseline: Row, selected_ids: list[int], scores: dict[int, int]) -> Row:
    known_scores = [scores[memory_id] for memory_id in selected_ids if memory_id in scores]
    oracle_scores = list(scores.values())
    return {
        "strategy": label,
        "case": baseline["case"],
        "run_id": int(baseline["run_id"]),
        "session_id": baseline["session_id"],
        "turn_ordinal": int(baseline["turn_ordinal"]),
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": avg(known_scores),
        "useful_known_selected": sum(1 for score in known_scores if score >= 4),
        "low_known_selected": sum(1 for score in known_scores if score <= 2),
        "captured_any_known_useful": any(score >= 4 for score in known_scores),
        "selected_any_known_low": any(score <= 2 for score in known_scores),
        "oracle_has_useful": any(score >= 4 for score in oracle_scores),
        "oracle_best_score": max(oracle_scores) if oracle_scores else None,
        "empty_recall": len(selected_ids) == 0,
    }


def visible_in_query(memory: Row, query_text: str, overlap_threshold: float) -> bool:
    query_tokens = tokens(query_text)
    memory_tokens = set(memory.get("tokens") or [])
    if len(memory_tokens) < 5:
        return False
    overlap = len(memory_tokens & query_tokens) / min(len(memory_tokens), 24)
    if overlap >= overlap_threshold:
        return True
    memory_keys = set(memory.get("identity_keys") or [])
    query_keys = identity_keys(query_text)
    return bool(memory_keys) and memory_keys.issubset(query_keys)


def dedup_selected(
    rows: list[Row],
    recall_dir: Path,
    strategy_label: str,
    memories: dict[int, Row],
    overlap_threshold: float,
) -> tuple[list[Row], list[Row]]:
    sorted_rows = sorted(rows, key=lambda row: (row["session_id"], int(row["turn_ordinal"])))
    seen_by_session: dict[str, set[int]] = {}
    candidate_rows: list[Row] = []
    drops: list[Row] = []
    for baseline in sorted_rows:
        run_id = int(baseline["run_id"])
        recall = load_json(recall_dir / f"{strategy_label}.recall-{run_id}.json")
        query_text = str(recall.get("query_text") or "")
        session_seen = seen_by_session.setdefault(baseline["session_id"], set())
        selected_ids = [int(memory_id) for memory_id in baseline.get("selected_memory_ids") or []]
        kept: list[int] = []
        for memory_id in selected_ids:
            memory = memories.get(memory_id)
            reason = None
            if memory_id in session_seen:
                reason = "previously_recalled_same_session"
            elif memory is not None and visible_in_query(memory, query_text, overlap_threshold):
                reason = "visible_in_current_query"
            if reason:
                drops.append(
                    {
                        "run_id": run_id,
                        "session_id": baseline["session_id"],
                        "turn_ordinal": int(baseline["turn_ordinal"]),
                        "memory_id": memory_id,
                        "reason": reason,
                        "memory_title": memory.get("title") if memory else None,
                    }
                )
            else:
                kept.append(memory_id)
        scores = oracle_score_map(load_json(recall_dir / f"oracle-{run_id}.json"))
        candidate_rows.append(details_row("session-context-dedup", baseline, kept, scores))
        session_seen.update(selected_ids)
    candidate_rows.sort(key=lambda row: int(row["run_id"]), reverse=True)
    return candidate_rows, drops


def summarize(rows: list[Row]) -> Row:
    summary = {metric: metric_from_rows(metric, rows) for metric in METRICS}
    summary["anchors"] = len(rows)
    summary["selected_memories"] = sum(int(row["selected_count"]) for row in rows)
    summary["known_selected_memories"] = sum(int(row["known_selected_count"]) for row in rows)
    summary["unknown_selected_memories"] = sum(int(row["unknown_selected_count"]) for row in rows)
    return summary


def format_value(value: Any) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, float):
        return f"{value:.3f}"
    return str(value)


def build_report(manifest: Row, baseline_summary: Row, candidate_summary: Row, deltas: Row) -> str:
    lines = [
        "# Session-Level Context Dedup",
        "",
        f"Date: {manifest['date']}",
        f"Baseline: `{manifest['baseline_label']}`",
        f"Anchors: `{manifest['anchors_file']}`",
        "",
        "## Experiment",
        "",
        "Replay saved selections and drop a selected memory when it was already selected earlier in the same session, or when the memory text has strong visible overlap with the current recall query.",
        "",
        "## Results",
        "",
        "| Metric | Baseline | Dedup | Delta | 95% CI | Verdict |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for metric in METRICS:
        delta = deltas[metric]
        ci = delta["ci_95"]
        ci_text = "n/a" if ci is None else f"[{ci[0]:.3f}, {ci[1]:.3f}]"
        lines.append(
            f"| `{metric}` | {format_value(baseline_summary[metric])} | "
            f"{format_value(candidate_summary[metric])} | "
            f"{format_value(delta['point_delta'])} | {ci_text} | {delta['verdict']} |"
        )
    lines.extend(
        [
            "",
            "## Dedup Drops",
            "",
            f"- Selected memories dropped: {manifest['drop_counts']['total']}",
            f"- Previously recalled same session: {manifest['drop_counts']['previously_recalled_same_session']}",
            f"- Visible in current query: {manifest['drop_counts']['visible_in_current_query']}",
            "",
            "## Decision",
            "",
            manifest["decision"],
            "",
            "## Artifacts",
            "",
            "- `manifest.json` records inputs, summaries, deltas, and drop counts.",
            "- `baseline.details.jsonl` is the copied baseline detail rows.",
            "- `session_context_dedup.details.jsonl` is the replayed candidate rows.",
            "- `drops.jsonl` records dropped memory ids and reasons.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--details", type=Path, required=True)
    parser.add_argument("--recall-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", required=True)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-07-07-session-context-dedup"))
    parser.add_argument("--date", default=dt.date.today().isoformat())
    parser.add_argument("--overlap-threshold", type=float, default=0.72)
    args = parser.parse_args()

    baseline_rows = read_jsonl(args.details)
    selected_ids = {
        int(memory_id)
        for row in baseline_rows
        for memory_id in row.get("selected_memory_ids") or []
    }
    memories = hydrate_memories(args.db.expanduser().resolve(), selected_ids)
    candidate_rows, drops = dedup_selected(
        baseline_rows,
        args.recall_dir,
        args.strategy_label,
        memories,
        args.overlap_threshold,
    )
    deltas = paired_bootstrap_deltas(baseline_rows, candidate_rows, METRICS)
    baseline_summary = summarize(baseline_rows)
    candidate_summary = summarize(candidate_rows)
    drop_counter: dict[str, int] = {"total": len(drops)}
    for reason in ["previously_recalled_same_session", "visible_in_current_query"]:
        drop_counter[reason] = sum(1 for drop in drops if drop["reason"] == reason)
    decision = (
        "Do not ship this drop-only context-dedup policy as a recall-quality change. "
        "It reduces selected context, but the screening set has too few known selected "
        "labels to prove it preserves useful recall; use these drops as diagnostic "
        "cases for a richer session-context feature rather than a hard gate."
    )
    manifest: Row = {
        "date": args.date,
        "anchors_file": str(args.anchors_file),
        "baseline_details": str(args.details),
        "baseline_label": args.strategy_label,
        "recall_dir": str(args.recall_dir),
        "db": str(args.db.expanduser().resolve()),
        "strategy": "session-context-dedup",
        "overlap_threshold": args.overlap_threshold,
        "baseline_summary": baseline_summary,
        "candidate_summary": candidate_summary,
        "deltas": deltas,
        "drop_counts": drop_counter,
        "decision": decision,
    }

    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    write_json(out_dir / "manifest.json", manifest)
    write_jsonl(out_dir / "baseline.details.jsonl", baseline_rows)
    write_jsonl(out_dir / "session_context_dedup.details.jsonl", candidate_rows)
    write_jsonl(out_dir / "drops.jsonl", drops)
    (out_dir / "REPORT.md").write_text(build_report(manifest, baseline_summary, candidate_summary, deltas))
    print(json.dumps({key: manifest[key] for key in ["candidate_summary", "drop_counts", "deltas"]}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
