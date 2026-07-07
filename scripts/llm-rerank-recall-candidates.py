#!/usr/bin/env python3
"""Replay recall selection with LLM-scored saved candidates.

This is a saved-candidate experiment. It reads recall JSON produced with
`--debug-ranking`, scores the top eligible candidates with an LLM, caches those
scores, tunes a threshold on training folds, and compares held-out selection
against the saved production selection.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import sqlite3
import sys
import textwrap
import time
import urllib.error
import urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from recall_experiment_stats import metric_from_rows, paired_bootstrap_deltas

Row = dict[str, Any]

DEFAULT_MODEL = "claude-haiku-4-5-20251001"
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
THRESHOLDS = [1, 2, 3, 4, 5]
RUBRIC_LINES = [
    "5: directly useful and actionable for the current turn.",
    "4: useful context with minor gaps or extra filtering needed.",
    "3: mixed or marginal; some relevance but not clearly worth recall.",
    "2: weak, stale, or mostly irrelevant.",
    "1: distracting, wrong-context, or actively harmful.",
]


def load_json(path: Path) -> Row:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError as exc:
        raise SystemExit(f"missing required file: {path}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse {path}: {exc}") from exc


def load_jsonl(path: Path) -> list[Row]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def read_anchors(path: Path) -> list[Row]:
    anchors = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        run_id, session_id, turn_ordinal, case = line.split("\t")
        anchors.append({
            "run_id": int(run_id),
            "session_id": session_id,
            "turn_ordinal": int(turn_ordinal),
            "case": case,
        })
    return anchors


def numeric_score(value: Any) -> int | None:
    try:
        score = int(str(value))
    except (TypeError, ValueError):
        return None
    return score if 1 <= score <= 5 else None


def truncate(text: str, max_chars: int) -> str:
    if len(text) <= max_chars:
        return text
    return text[: max_chars - 15].rstrip() + "\n...[truncated]"


def parse_json_object(text: str) -> Row:
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        match = re.search(r"\{.*\}", text, re.DOTALL)
        if match is None:
            raise ValueError(f"response did not contain a JSON object: {text[:200]}")
        value = json.loads(match.group(0))
    if not isinstance(value, dict):
        raise ValueError("response JSON was not an object")
    return value


def call_anthropic_json(
    *,
    system: str,
    prompt: str,
    model: str,
    api_key: str,
    timeout_seconds: float,
) -> Row:
    body = {
        "model": model,
        "max_tokens": 512,
        "temperature": 0,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
    }
    request = urllib.request.Request(
        "https://api.anthropic.com/v1/messages",
        data=json.dumps(body).encode("utf-8"),
        headers={
            "content-type": "application/json",
            "anthropic-version": "2023-06-01",
            "x-api-key": api_key,
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
        payload = json.loads(response.read().decode("utf-8"))
    text = "\n".join(
        block.get("text", "")
        for block in payload.get("content", [])
        if block.get("type") == "text"
    )
    return parse_json_object(text)


def load_memories(db_path: Path, memory_ids: set[int]) -> dict[int, Row]:
    if not memory_ids:
        return {}
    placeholders = ",".join("?" for _ in memory_ids)
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        f"""
        SELECT id, title, body, memory_kind, task_keys, project_id, project_descriptor,
               is_active, superseded_by_memory_id
        FROM memories
        WHERE id IN ({placeholders})
        """,
        sorted(memory_ids),
    ).fetchall()
    return {int(row["id"]): dict(row) for row in rows}


def oracle_scores(oracle: Row) -> dict[int, int]:
    scores = {}
    for result in oracle.get("results") or []:
        score = numeric_score(result.get("judge_score"))
        memory_id = result.get("memory_id")
        if score is not None and memory_id is not None:
            scores[int(memory_id)] = score
    return scores


def eligible_for_llm_pool(candidate: Row) -> bool:
    return any(
        reason.startswith("keep:") and reason != "keep:strict_kind_diverse"
        for reason in candidate.get("filter_reasons") or []
    )


def candidate_pool(recall: Row, top_k: int) -> list[Row]:
    eligible = [
        candidate
        for candidate in recall.get("ranking") or []
        if eligible_for_llm_pool(candidate)
    ]
    return eligible[:top_k]


def score_prompt(recall: Row, candidate: Row, memory: Row) -> tuple[str, str]:
    rank = candidate.get("rank") or {}
    rubric = "\n".join(f"- {line}" for line in RUBRIC_LINES)
    system = (
        "You are scoring memory recall quality for an AI coding agent before context injection. "
        "Decide whether the stored memory would be useful context for the current turn. "
        "Use only the query, memory, and rubric. Rank metadata is diagnostic context, not a label. "
        "Return JSON only with fields score and rationale; score must be a string from \"1\" to \"5\"."
    )
    prompt = textwrap.dedent(
        f"""
        Score whether this stored memory would be useful context for answering the current turn.

        Return only JSON with this exact shape:
        {{"score": <integer 1-5>, "rationale": "<one short sentence>"}}

        Rubric:
        {rubric}

        Current turn / recall query:
        ```text
        {truncate(recall.get("query_text") or "", 4_000)}
        ```

        Candidate rank metadata:
        ```json
        {json.dumps({
            "rank_index": candidate.get("rank_index"),
            "memory_id": candidate.get("memory_id"),
            "memory_kind": candidate.get("memory_kind"),
            "score": candidate.get("score"),
            "similarity": candidate.get("similarity"),
            "matched_task_keys": rank.get("matched_task_keys") or [],
            "filter_reasons": candidate.get("filter_reasons") or [],
            "penalties": rank.get("penalties") or [],
        }, sort_keys=True)}
        ```

        Stored memory title:
        ```text
        {truncate(str(memory.get("title") or "(untitled memory)"), 500)}
        ```

        Stored memory body:
        ```text
        {truncate(str(memory.get("body") or memory.get("title") or ""), 4_000)}
        ```
        """
    ).strip()
    return system, prompt


def load_score_cache(path: Path) -> dict[tuple[int, int, str], Row]:
    cache = {}
    for row in load_jsonl(path):
        score = numeric_score(row.get("llm_score"))
        if score is None:
            continue
        cache[(int(row["run_id"]), int(row["memory_id"]), str(row["model"]))] = row
    return cache


def append_score(path: Path, row: Row) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as handle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")


def build_run_inputs(args: argparse.Namespace) -> tuple[list[Row], dict[int, Row], dict[int, Row], dict[int, list[Row]]]:
    anchors = read_anchors(args.anchors_file.expanduser().resolve())
    recalls: dict[int, Row] = {}
    oracles: dict[int, Row] = {}
    pools: dict[int, list[Row]] = {}
    all_memory_ids: set[int] = set()
    for anchor in anchors:
        run_id = anchor["run_id"]
        recall = load_json(args.input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        oracle = load_json(args.input_dir / f"oracle-{run_id}.json")
        pool = candidate_pool(recall, args.top_k)
        for rank_index, candidate in enumerate(recall.get("ranking") or []):
            candidate["rank_index"] = rank_index
        all_memory_ids.update(int(candidate["memory_id"]) for candidate in pool)
        recalls[run_id] = recall
        oracles[run_id] = oracle
        pools[run_id] = pool
    memories = load_memories(args.db.expanduser(), all_memory_ids)
    return anchors, recalls, oracles, pools, memories


def score_missing(args: argparse.Namespace) -> int:
    if args.provider != "anthropic":
        raise SystemExit("only --provider anthropic is currently supported")
    api_key = os.environ.get(args.api_key_env)
    if not api_key:
        raise SystemExit(f"{args.api_key_env} is not set")
    anchors, recalls, _oracles, pools, memories = build_run_inputs(args)
    score_path = args.out_dir / "llm_scores.jsonl"
    cache = load_score_cache(score_path)
    tasks = []
    for anchor in anchors:
        run_id = anchor["run_id"]
        for candidate in pools[run_id]:
            memory_id = int(candidate["memory_id"])
            key = (run_id, memory_id, args.model)
            if key in cache:
                continue
            memory = memories.get(memory_id)
            if memory is None:
                continue
            tasks.append((run_id, candidate, memory))
    if args.limit is not None:
        tasks = tasks[: args.limit]
    if args.workers <= 1:
        return score_missing_sequential(args, api_key, recalls, score_path, cache, tasks)

    write_lock = Lock()
    completed = 0
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = [
            executor.submit(score_one_candidate, args, api_key, recalls[run_id], run_id, candidate, memory)
            for run_id, candidate, memory in tasks
        ]
        for future in as_completed(futures):
            detail = future.result()
            with write_lock:
                append_score(score_path, detail)
            completed += 1
            if not args.quiet:
                print(json.dumps({
                    "completed": completed,
                    "run_id": detail["run_id"],
                    "memory_id": detail["memory_id"],
                    "llm_score": detail["llm_score"],
                }, sort_keys=True))
    return completed


def score_missing_sequential(
    args: argparse.Namespace,
    api_key: str,
    recalls: dict[int, Row],
    score_path: Path,
    cache: dict[tuple[int, int, str], Row],
    tasks: list[tuple[int, Row, Row]],
) -> int:
    completed = 0
    for run_id, candidate, memory in tasks:
        memory_id = int(candidate["memory_id"])
        try:
            detail = score_one_candidate(args, api_key, recalls[run_id], run_id, candidate, memory)
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ValueError) as exc:
            raise SystemExit(f"failed scoring run {run_id} memory {memory_id}: {exc}") from exc
        append_score(score_path, detail)
        cache[(run_id, memory_id, args.model)] = detail
        completed += 1
        if not args.quiet:
            print(json.dumps({
                "completed": completed,
                "run_id": run_id,
                "memory_id": memory_id,
                "llm_score": detail["llm_score"],
            }, sort_keys=True))
        if args.sleep_seconds:
            time.sleep(args.sleep_seconds)
    return completed


def score_one_candidate(
    args: argparse.Namespace,
    api_key: str,
    recall: Row,
    run_id: int,
    candidate: Row,
    memory: Row,
) -> Row:
    memory_id = int(candidate["memory_id"])
    system, prompt = score_prompt(recall, candidate, memory)
    response = call_anthropic_json(
        system=system,
        prompt=prompt,
        model=args.model,
        api_key=api_key,
        timeout_seconds=args.timeout_seconds,
    )
    score = numeric_score(response.get("score"))
    if score is None:
        raise ValueError(f"invalid score for run {run_id} memory {memory_id}: {response}")
    return {
        "run_id": run_id,
        "memory_id": memory_id,
        "model": args.model,
        "provider": args.provider,
        "llm_score": score,
        "llm_rationale": truncate(str(response.get("rationale") or ""), 700),
        "rank_index": candidate.get("rank_index"),
        "rank_score": candidate.get("score"),
        "similarity": candidate.get("similarity"),
        "memory_kind": candidate.get("memory_kind"),
        "scored_at": dt.datetime.now(dt.UTC).isoformat(),
    }


def details_row(label: str, anchor: Row, oracle: Row, selected_ids: list[int]) -> Row:
    scores = oracle_scores(oracle)
    known_scores = [scores[memory_id] for memory_id in selected_ids if memory_id in scores]
    oracle_score_values = list(scores.values())
    return {
        "strategy": label,
        "case": anchor["case"],
        "run_id": anchor["run_id"],
        "session_id": anchor["session_id"],
        "turn_ordinal": anchor["turn_ordinal"],
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": sum(known_scores) / len(known_scores) if known_scores else None,
        "useful_known_selected": sum(1 for score in known_scores if score >= 4),
        "low_known_selected": sum(1 for score in known_scores if score <= 2),
        "captured_any_known_useful": any(score >= 4 for score in known_scores),
        "selected_any_known_low": any(score <= 2 for score in known_scores),
        "oracle_has_useful": any(score >= 4 for score in oracle_score_values),
        "oracle_best_score": max(oracle_score_values) if oracle_score_values else None,
        "empty_recall": len(selected_ids) == 0,
    }


def select_with_threshold(
    candidates: list[Row],
    score_by_memory: dict[int, int],
    threshold: int,
    limit: int,
) -> list[int]:
    scored = [
        (
            int(score_by_memory[int(candidate["memory_id"])]),
            -int(candidate.get("rank_index") or 0),
            int(candidate["memory_id"]),
        )
        for candidate in candidates
        if int(candidate["memory_id"]) in score_by_memory
        and int(score_by_memory[int(candidate["memory_id"])]) >= threshold
    ]
    scored.sort(key=lambda item: (-item[0], item[1], item[2]))
    return [memory_id for _score, _rank, memory_id in scored[:limit]]


def summary_from_details(label: str, rows: list[Row]) -> Row:
    return {
        "strategy": label,
        "anchors": len(rows),
        "selected_memories": sum(row["selected_count"] for row in rows),
        "average_selected_per_anchor": metric_from_rows("average_selected_per_anchor", rows),
        "known_selected_memories": sum(row["known_selected_count"] for row in rows),
        "unknown_selected_memories": sum(row["unknown_selected_count"] for row in rows),
        "average_known_score": metric_from_rows("average_known_score", rows),
        "useful_known_selected": metric_from_rows("useful_known_selected", rows),
        "low_known_selected": metric_from_rows("low_known_selected", rows),
        "useful_capture_runs": metric_from_rows("useful_capture_runs", rows),
        "low_selection_runs": metric_from_rows("low_selection_runs", rows),
        "oracle_useful_runs": sum(1 for row in rows if row["oracle_has_useful"]),
        "empty_recall_runs": metric_from_rows("empty_recall_runs", rows),
        "missed_useful_empty_runs": metric_from_rows("missed_useful_empty_runs", rows),
        "clean_abstention_runs": metric_from_rows("clean_abstention_runs", rows),
    }


def utility(rows: list[Row]) -> float:
    return (
        sum(row["useful_known_selected"] for row in rows) * 3.0
        + sum(1 for row in rows if row["captured_any_known_useful"]) * 2.0
        - sum(row["low_known_selected"] for row in rows) * 3.0
        - sum(row["selected_count"] for row in rows) * 0.40
        - sum(1 for row in rows if row["empty_recall"] and row["oracle_has_useful"]) * 0.75
    )


def tune_threshold(
    anchors: list[Row],
    oracles: dict[int, Row],
    pools: dict[int, list[Row]],
    scores: dict[tuple[int, int, str], Row],
    model: str,
    train_fold: set[int],
    limit: int,
) -> int:
    best_threshold = THRESHOLDS[0]
    best_utility = float("-inf")
    train_anchors = [anchor for index, anchor in enumerate(anchors) if index % 5 in train_fold]
    for threshold in THRESHOLDS:
        rows = []
        for anchor in train_anchors:
            run_id = anchor["run_id"]
            score_by_memory = {
                memory_id: int(row["llm_score"])
                for (score_run_id, memory_id, score_model), row in scores.items()
                if score_run_id == run_id and score_model == model
            }
            selected = select_with_threshold(pools[run_id], score_by_memory, threshold, limit)
            rows.append(details_row(f"llm_threshold_{threshold}", anchor, oracles[run_id], selected))
        score = utility(rows)
        if score > best_utility:
            best_utility = score
            best_threshold = threshold
    return best_threshold


def build_report(manifest: Row) -> str:
    baseline = manifest["baseline_summary"]
    candidate = manifest["candidate_summary"]
    lines = [
        "# LLM Scoring Rerank",
        "",
        f"Date: {manifest['date']}",
        f"Input directory: `{manifest['input_dir']}`",
        "",
        "## Experiment",
        "",
        "Scored the top saved recall candidates with the aligned query-plus-memory rubric prompt, tuned a score threshold on training folds, and replayed held-out selection without rerunning retrieval.",
        "",
        "## Results",
        "",
        "| Metric | Production | LLM rerank | Delta | 95% CI | Verdict |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for metric in METRICS:
        delta = manifest["deltas"][metric]
        ci = delta["ci_95"]
        ci_text = "n/a" if ci is None else f"[{ci[0]:.3f}, {ci[1]:.3f}]"
        point = delta["point_delta"]
        point_text = "n/a" if point is None else f"{point:.3f}"
        lines.append(
            f"| `{metric}` | {baseline[metric]:.3f} | {candidate[metric]:.3f} | {point_text} | {ci_text} | {delta['verdict']} |"
        )
    lines.extend(
        [
            "",
            "## Threshold Diagnostics",
            "",
            "| Threshold | Selected | Avg selected | Useful known | Low known | Useful runs | Empty | Missed-useful empty |",
            "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for item in manifest["fixed_threshold_summaries"]:
        summary = item["summary"]
        lines.append(
            f"| {item['threshold']} | {summary['selected_memories']:.0f} | {summary['average_selected_per_anchor']:.3f} | {summary['useful_known_selected']:.0f} | {summary['low_known_selected']:.0f} | {summary['useful_capture_runs']:.0f} | {summary['empty_recall_runs']:.0f} | {summary['missed_useful_empty_runs']:.0f} |"
        )
    lines.extend(
        [
            "",
            "## Scoring",
            "",
            f"- Model: `{manifest['model']}`",
            f"- Candidate pool: top {manifest['top_k']} production-eligible candidates per anchor.",
            f"- Scored candidates: {manifest['scored_candidates']}",
            f"- Thresholds by fold: {', '.join(str(value) for value in manifest['thresholds'])}",
            f"- Selection limit: {manifest['selection_limit']}",
            "",
            "## Decision",
            "",
            manifest["decision"],
            "",
            "## Artifacts",
            "",
            "- `llm_scores.jsonl` records cached candidate scores.",
            "- `production.details.jsonl` records saved production selections.",
            "- `llm_rerank.details.jsonl` records held-out LLM-rerank selections.",
            "- `manifest.json` records summaries and paired deltas.",
            "",
        ]
    )
    return "\n".join(lines)


def evaluate(args: argparse.Namespace) -> Row:
    anchors, recalls, oracles, pools, _memories = build_run_inputs(args)
    score_path = args.out_dir / "llm_scores.jsonl"
    scores = load_score_cache(score_path)
    missing = 0
    for anchor in anchors:
        run_id = anchor["run_id"]
        for candidate in pools[run_id]:
            if (run_id, int(candidate["memory_id"]), args.model) not in scores:
                missing += 1
    if missing:
        raise SystemExit(f"{missing} candidate scores are missing; rerun without --evaluate-only or increase --limit")

    baseline_rows = []
    candidate_rows = []
    thresholds = []
    for fold in range(5):
        train_folds = set(range(5)) - {fold}
        threshold = tune_threshold(
            anchors,
            oracles,
            pools,
            scores,
            args.model,
            train_folds,
            args.selection_limit,
        )
        thresholds.append(threshold)
        for index, anchor in enumerate(anchors):
            if index % 5 != fold:
                continue
            run_id = anchor["run_id"]
            production_selected = [int(memory_id) for memory_id in recalls[run_id].get("selected_memory_ids") or []]
            score_by_memory = {
                memory_id: int(row["llm_score"])
                for (score_run_id, memory_id, score_model), row in scores.items()
                if score_run_id == run_id and score_model == args.model
            }
            rerank_selected = select_with_threshold(
                pools[run_id],
                score_by_memory,
                threshold,
                args.selection_limit,
            )
            baseline_rows.append(details_row("production", anchor, oracles[run_id], production_selected))
            candidate_rows.append(details_row(args.label, anchor, oracles[run_id], rerank_selected))
    baseline_rows.sort(key=lambda row: row["run_id"])
    candidate_rows.sort(key=lambda row: row["run_id"])
    baseline_path = args.out_dir / "production.details.jsonl"
    candidate_path = args.out_dir / "llm_rerank.details.jsonl"
    write_jsonl(baseline_path, baseline_rows)
    write_jsonl(candidate_path, candidate_rows)
    deltas = paired_bootstrap_deltas(baseline_rows, candidate_rows, METRICS)
    fixed_threshold_summaries = []
    for threshold in THRESHOLDS:
        threshold_rows = []
        for anchor in anchors:
            run_id = anchor["run_id"]
            score_by_memory = {
                memory_id: int(row["llm_score"])
                for (score_run_id, memory_id, score_model), row in scores.items()
                if score_run_id == run_id and score_model == args.model
            }
            selected = select_with_threshold(
                pools[run_id],
                score_by_memory,
                threshold,
                args.selection_limit,
            )
            threshold_rows.append(
                details_row(f"llm_threshold_{threshold}", anchor, oracles[run_id], selected)
            )
        fixed_threshold_summaries.append({
            "threshold": threshold,
            "summary": summary_from_details(f"llm_threshold_{threshold}", threshold_rows),
        })
    useful_delta = deltas["useful_known_selected"]
    low_delta = deltas["low_known_selected"]
    decision = (
        "Do not ship this LLM rerank. Useful-known selection did not improve with a CI excluding zero."
    )
    if (
        useful_delta["ci_95"] is not None
        and useful_delta["ci_95"][0] > 0
        and (low_delta["ci_95"] is None or low_delta["ci_95"][1] <= 0)
    ):
        decision = (
            "Candidate is promising on screening: useful-known selection improved without a confirmed low-selection increase. Confirm on holdout before shipping."
        )
    manifest = {
        "date": args.date,
        "input_dir": str(args.input_dir),
        "anchors_file": str(args.anchors_file),
        "strategy_label": args.strategy_label,
        "label": args.label,
        "model": args.model,
        "top_k": args.top_k,
        "selection_limit": args.selection_limit,
        "scored_candidates": len(scores),
        "thresholds": thresholds,
        "baseline_details": str(baseline_path),
        "candidate_details": str(candidate_path),
        "baseline_summary": summary_from_details("production", baseline_rows),
        "candidate_summary": summary_from_details(args.label, candidate_rows),
        "deltas": deltas,
        "fixed_threshold_summaries": fixed_threshold_summaries,
        "decision": decision,
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (args.out_dir / "REPORT.md").write_text(build_report(manifest))
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", required=True)
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--label", default="llm-scoring-rerank")
    parser.add_argument("--date", default=dt.date.today().isoformat())
    parser.add_argument("--provider", default="anthropic")
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--api-key-env", default="ANTHROPIC_API_KEY")
    parser.add_argument("--top-k", type=int, default=8)
    parser.add_argument("--selection-limit", type=int, default=2)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--sleep-seconds", type=float, default=0.0)
    parser.add_argument("--timeout-seconds", type=float, default=60.0)
    parser.add_argument("--evaluate-only", action="store_true")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    completed = 0
    if not args.evaluate_only:
        completed = score_missing(args)
    manifest = None
    try:
        manifest = evaluate(args)
    except SystemExit:
        if args.limit is not None:
            print(json.dumps({
                "completed_this_run": completed,
                "score_cache": str(args.out_dir / "llm_scores.jsonl"),
                "evaluation": "incomplete",
            }, indent=2, sort_keys=True))
            return
        raise
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
