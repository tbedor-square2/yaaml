#!/usr/bin/env python3
"""Mine repeated correction patterns that may indicate missed memory formation.

This is a diagnostic, not a strategy comparison. It scans indexed YAAML
sessions, reads their transcript files, extracts user turns that look like
corrections or repeated preferences, groups them by project/topic signature,
and checks whether an active memory appears to cover each repeated cluster.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
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
    "module",
    "path",
    "pr",
    "service",
    "target",
    "task",
    "ticket",
    "tool",
}

STOPWORDS = {
    "about",
    "after",
    "again",
    "also",
    "and",
    "are",
    "been",
    "but",
    "can",
    "code",
    "continue",
    "could",
    "from",
    "have",
    "into",
    "just",
    "lets",
    "make",
    "more",
    "need",
    "next",
    "only",
    "please",
    "proceed",
    "repo",
    "run",
    "same",
    "should",
    "that",
    "the",
    "then",
    "there",
    "this",
    "through",
    "turn",
    "use",
    "user",
    "what",
    "when",
    "where",
    "with",
    "work",
    "would",
    "you",
}

CORRECTION_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("explicit_wrong", re.compile(r"\b(wrong|incorrect|not right|not what)\b", re.I)),
    ("agent_missed", re.compile(r"\byou (missed|forgot|ignored|didn'?t|did not)\b", re.I)),
    ("should_have", re.compile(r"\b(should have|should've|should not have|shouldn't have)\b", re.I)),
    (
        "do_not",
        re.compile(
            r"\b(i don'?t want|i dont want|don'?t think (we|you) got|dont think (we|you) got|"
            r"do not (use|send|commit|push|revert)|don'?t (use|send|commit|push|revert))\b",
            re.I,
        ),
    ),
    ("instead", re.compile(r"\binstead\b", re.I)),
    ("no_correction", re.compile(r"(^|\n)\s*no[,.\s]", re.I)),
    ("repeated_correction", re.compile(r"\b(as i said|like i said|same mistake again)\b", re.I)),
    ("why_did", re.compile(r"\bwhy (did|are|would) you\b", re.I)),
]


def load_json_lines(path: Path) -> list[Row]:
    rows = []
    with path.open() as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def text_from_content(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if not isinstance(item, dict):
                continue
            if item.get("type") in {"input_text", "text"} and item.get("text"):
                parts.append(str(item["text"]))
            elif item.get("type") == "tool_result" and item.get("content"):
                parts.append(text_from_content(item["content"]))
        return "\n".join(parts)
    if isinstance(content, dict):
        if content.get("text"):
            return str(content["text"])
        if content.get("content"):
            return text_from_content(content["content"])
    return ""


def codex_user_messages(path: Path) -> list[str]:
    messages = []
    for row in load_json_lines(path):
        payload = row.get("payload")
        if not isinstance(payload, dict):
            continue
        if row.get("type") == "response_item" and payload.get("role") == "user":
            messages.append(text_from_content(payload.get("content")))
    return messages


def claude_user_messages(path: Path) -> list[str]:
    messages = []
    for row in load_json_lines(path):
        message = row.get("message")
        if isinstance(message, dict) and message.get("role") == "user":
            messages.append(text_from_content(message.get("content")))
        elif row.get("type") == "user" and row.get("message"):
            messages.append(text_from_content(row.get("message")))
    return messages


def transcript_user_messages(path: Path) -> list[str]:
    if not path.exists():
        return []
    messages = codex_user_messages(path)
    if messages:
        return messages
    return claude_user_messages(path)


def usable_user_text(text: str) -> bool:
    if not text.strip():
        return False
    blocked_markers = [
        "<environment_context>",
        "<codex_internal_context",
        "# AGENTS.md instructions",
        "<INSTRUCTIONS>",
        "<user_instructions>",
        "Base instructions",
    ]
    return not any(marker in text for marker in blocked_markers)


def correction_reasons(text: str) -> list[str]:
    return [label for label, pattern in CORRECTION_PATTERNS if pattern.search(text)]


def normalize_project(project_id: str) -> str:
    return Path(project_id).name or project_id


def identity_keys(text: str) -> set[str]:
    lower = text.lower()
    keys: set[str] = set()
    for prefix in IDENTITY_KEY_PREFIXES:
        for match in re.finditer(rf"\b{re.escape(prefix)}:([a-z0-9][a-z0-9._/-]*)", lower):
            keys.add(f"{prefix}:{match.group(1).rstrip('.,;:')}")
    for match in re.finditer(r"\bpr(?:\s+|#)(\d{2,})\b", lower):
        keys.add(f"pr:{match.group(1)}")
    for match in re.finditer(r"\b[a-z0-9_.-]+/[a-z0-9][a-z0-9._/-]*\b", lower):
        keys.add(f"path:{match.group(0).rstrip('.,;:')}")
    return keys


def tokens(text: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[a-z0-9][a-z0-9_-]{2,}", text.lower())
        if token not in STOPWORDS and not token.isdigit()
    }


def signature(project_id: str, text: str) -> str:
    keys = sorted(identity_keys(text))
    project = normalize_project(project_id)
    if keys:
        return f"{project}|{keys[0]}"
    ranked = [
        token
        for token, _count in collections.Counter(tokens(text)).most_common(4)
        if len(token) >= 4
    ]
    if not ranked:
        ranked = ["generic-correction"]
    return f"{project}|{'-'.join(ranked[:3])}"


def load_sessions(conn: sqlite3.Connection, limit: int | None) -> list[Row]:
    sql = """
        SELECT id, agent_type, project_id, transcript_file_path, started_at, last_seen_at
        FROM sessions
        ORDER BY COALESCE(last_seen_at, started_at, '') DESC
    """
    if limit is not None:
        sql += " LIMIT ?"
        rows = conn.execute(sql, (limit,)).fetchall()
    else:
        rows = conn.execute(sql).fetchall()
    return [dict(row) for row in rows]


def load_active_memories(conn: sqlite3.Connection) -> list[Row]:
    rows = conn.execute(
        """
        SELECT id, title, body, project_id, memory_kind, task_keys
        FROM memories
        WHERE is_active = 1
          AND superseded_by_memory_id IS NULL
        """
    ).fetchall()
    memories = []
    for row in rows:
        memory = dict(row)
        try:
            memory["task_keys_list"] = json.loads(memory.get("task_keys") or "[]")
        except json.JSONDecodeError:
            memory["task_keys_list"] = []
        memory["text_tokens"] = tokens(f"{memory['title']}\n{memory['body']}")
        memory["identity_keys"] = set(memory["task_keys_list"]) | identity_keys(
            f"{memory['title']}\n{memory['body']}"
        )
        memories.append(memory)
    return memories


def coverage_for_cluster(project_id: str, cluster_tokens: set[str], cluster_keys: set[str], memories: list[Row]) -> Row | None:
    best: tuple[float, Row] | None = None
    for memory in memories:
        memory_project = memory.get("project_id")
        if memory_project not in {None, project_id}:
            continue
        key_overlap = len(cluster_keys & memory["identity_keys"])
        token_overlap = len(cluster_tokens & memory["text_tokens"])
        token_score = token_overlap / max(1, min(len(cluster_tokens), 12))
        if key_overlap == 0 and token_score < 0.60:
            continue
        score = key_overlap * 2.0 + token_score
        if best is None or score > best[0]:
            best = (score, memory)
    if best is None:
        return None
    memory = best[1]
    return {
        "memory_id": memory["id"],
        "title": memory["title"],
        "project_id": memory["project_id"],
        "memory_kind": memory["memory_kind"],
        "coverage_score": round(best[0], 3),
    }


def write_json(path: Path, value: Row) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def build_report(summary: Row, manifest: Row) -> str:
    top_misses = summary["top_missed_clusters"][:10]
    lines = [
        "# Formation-Miss Mining",
        "",
        f"Date: {manifest['date']}",
        f"Database: `{manifest['db']}`",
        "",
        "## Question",
        "",
        "Where do repeated user corrections appear across sessions without an active memory that appears to cover the correction?",
        "",
        "## Results",
        "",
        f"- Sessions scanned: {summary['sessions_scanned']}",
        f"- Transcript files read: {summary['transcripts_read']}",
        f"- Correction-like user turns: {summary['correction_turns']}",
        f"- Repeated correction clusters: {summary['repeated_clusters']}",
        f"- Clusters with apparent active-memory coverage: {summary['covered_clusters']}",
        f"- Missed-formation clusters: {summary['missed_clusters']}",
        f"- Missed correction turns: {summary['missed_turns']}",
        "",
        "## Top Missed Clusters",
        "",
    ]
    if top_misses:
        for cluster in top_misses:
            lines.append(
                f"- `{cluster['signature']}`: {cluster['turn_count']} turns across "
                f"{cluster['session_count']} sessions; reasons={', '.join(cluster['reasons'])}"
            )
    else:
        lines.append("- None found with the configured repeated-session threshold.")
    lines.extend(
        [
            "",
            "## Interpretation",
            "",
            summary["decision"],
            "",
            "## Artifacts",
            "",
            "- `manifest.json` records inputs and heuristic thresholds.",
            "- `cases.jsonl` records repeated correction clusters, examples, and coverage classification.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--session-limit", type=int, default=None)
    parser.add_argument("--min-sessions", type=int, default=2)
    parser.add_argument("--out-dir", type=Path, default=Path("experiments/recall/2026-07-07-formation-miss-mining"))
    parser.add_argument("--date", default=dt.date.today().isoformat())
    args = parser.parse_args()

    db_path = args.db.expanduser().resolve()
    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    sessions = load_sessions(conn, args.session_limit)
    memories = load_active_memories(conn)

    corrections: list[Row] = []
    transcripts_read = 0
    for session in sessions:
        path = Path(session["transcript_file_path"]).expanduser()
        messages = transcript_user_messages(path)
        if messages:
            transcripts_read += 1
        for index, text in enumerate(messages):
            if not usable_user_text(text):
                continue
            reasons = correction_reasons(text)
            if not reasons:
                continue
            corrections.append(
                {
                    "session_id": session["id"],
                    "project_id": session["project_id"],
                    "transcript_file_path": str(path),
                    "message_index": index,
                    "signature": signature(session["project_id"], text),
                    "identity_keys": sorted(identity_keys(text)),
                    "tokens": sorted(tokens(text)),
                    "reasons": reasons,
                    "excerpt": re.sub(r"\s+", " ", text).strip()[:500],
                }
            )

    grouped: dict[str, list[Row]] = collections.defaultdict(list)
    for correction in corrections:
        grouped[correction["signature"]].append(correction)

    cases: list[Row] = []
    for sig, rows in sorted(grouped.items()):
        session_ids = {row["session_id"] for row in rows}
        if len(session_ids) < args.min_sessions:
            continue
        project_id = rows[0]["project_id"]
        cluster_tokens = set().union(*(set(row["tokens"]) for row in rows))
        cluster_keys = set().union(*(set(row["identity_keys"]) for row in rows))
        coverage = coverage_for_cluster(project_id, cluster_tokens, cluster_keys, memories)
        reason_counts = collections.Counter(reason for row in rows for reason in row["reasons"])
        cases.append(
            {
                "signature": sig,
                "project_id": project_id,
                "turn_count": len(rows),
                "session_count": len(session_ids),
                "reasons": sorted(reason_counts),
                "reason_counts": dict(sorted(reason_counts.items())),
                "identity_keys": sorted(cluster_keys),
                "covered_by_active_memory": coverage is not None,
                "covering_memory": coverage,
                "missed_opportunity": coverage is None,
                "examples": [
                    {
                        "session_id": row["session_id"],
                        "transcript_file_path": row["transcript_file_path"],
                        "message_index": row["message_index"],
                        "excerpt": row["excerpt"],
                        "reasons": row["reasons"],
                    }
                    for row in rows[:5]
                ],
            }
        )

    cases.sort(
        key=lambda row: (
            row["missed_opportunity"],
            row["session_count"],
            row["turn_count"],
            row["signature"],
        ),
        reverse=True,
    )
    missed = [case for case in cases if case["missed_opportunity"]]
    covered = [case for case in cases if not case["missed_opportunity"]]
    summary: Row = {
        "sessions_scanned": len(sessions),
        "transcripts_read": transcripts_read,
        "active_memories_scanned": len(memories),
        "correction_turns": len(corrections),
        "repeated_clusters": len(cases),
        "covered_clusters": len(covered),
        "missed_clusters": len(missed),
        "missed_turns": sum(case["turn_count"] for case in missed),
        "top_missed_clusters": missed[:20],
        "decision": (
            "Formation misses are observable with transcript-only mining. "
            "Use the missed clusters as seed cases for a formation-time activation "
            "condition prompt, but do not treat this heuristic diagnostic as a "
            "ship/no-ship evaluation of the write policy."
        ),
    }
    manifest: Row = {
        "date": args.date,
        "question": "Find repeated user corrections across sessions with no apparent active memory.",
        "db": str(db_path),
        "out_dir": str(out_dir),
        "session_limit": args.session_limit,
        "min_sessions": args.min_sessions,
        "heuristics": {
            "correction_patterns": [label for label, _pattern in CORRECTION_PATTERNS],
            "coverage": "same-project/global active memory with identity-key overlap or lexical overlap score >= 0.35",
        },
        "summary": {key: value for key, value in summary.items() if key != "top_missed_clusters"},
    }

    write_json(out_dir / "manifest.json", manifest)
    write_json(out_dir / "summary.json", summary)
    write_jsonl(out_dir / "cases.jsonl", cases)
    (out_dir / "REPORT.md").write_text(build_report(summary, manifest))
    print(json.dumps(manifest["summary"], indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
