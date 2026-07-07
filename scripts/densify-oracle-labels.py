#!/usr/bin/env python3
"""Densify oracle labels by adjudicator-scoring saved recall candidate pools.

The 2026-07 judge calibration showed the historical production judge has weak
agreement with the reference adjudicator (exact kappa 0.135), and useful-metric
label coverage is too sparse for readable experiment deltas (~3-4 useful-known
selections per arm). This script rebuilds the oracle: for each anchor it takes
the saved ranked candidate pool (plus production-selected and
historical-oracle memory ids), scores every (query, memory) pair with the
calibration adjudicator prompt imported from scripts/judge-calibration.py, and
writes dense per-run oracle files in the `yaaml eval show --json` shape.

Backtests consume the dense oracle by setting BACKTEST_ORACLE_DIR to the
dense-oracles output directory; scripts/backtest-recall-strategy.sh copies
those files instead of reading historical `yaaml eval show` rows.

Outputs under --out-dir:

- labels.jsonl              append-only score cache keyed (run_id, memory_id, model)
- dense-oracles/oracle-<run_id>.json
- manifest.json / REPORT.md coverage and label-distribution readout

Example:

    python3 scripts/densify-oracle-labels.py \
      --anchors-file experiments/recall/anchor-libraries/2026-07-screening.tsv \
      --anchors-file experiments/recall/anchor-libraries/2026-07-holdout.tsv \
      --input-dir target/anchor-refresh-2026-07 \
      --strategy-label anchor-refresh \
      --out-dir experiments/recall/oracle-labels/2026-07 \
      --workers 8
"""

from __future__ import annotations

import argparse
import datetime as dt
import http.client
import importlib.util
import json
import os
import sqlite3
import sys
import time
import urllib.error
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock
from typing import Any

SCRIPTS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS_DIR))

Row = dict[str, Any]

DEFAULT_MODEL = "claude-haiku-4-5-20251001"


def load_calibration_module():
    """Import judge-calibration.py so the adjudicator instrument stays single-sourced."""
    spec = importlib.util.spec_from_file_location(
        "judge_calibration", SCRIPTS_DIR / "judge-calibration.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CALIBRATION = load_calibration_module()


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


def read_anchor_rows(paths: list[Path]) -> list[Row]:
    anchors: dict[int, Row] = {}
    for path in paths:
        for line in path.read_text().splitlines():
            if not line.strip():
                continue
            run_id, session_id, turn_ordinal, case = line.split("\t")
            anchors[int(run_id)] = {
                "run_id": int(run_id),
                "session_id": session_id,
                "turn_ordinal": int(turn_ordinal),
                "case": case,
                "anchors_file": str(path),
            }
    return [anchors[run_id] for run_id in sorted(anchors)]


def load_memories(db_path: Path, memory_ids: set[int]) -> dict[int, Row]:
    if not memory_ids:
        return {}
    placeholders = ",".join("?" for _ in memory_ids)
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        f"""
        SELECT id, title, body, memory_kind, is_active
        FROM memories
        WHERE id IN ({placeholders})
        """,
        sorted(memory_ids),
    ).fetchall()
    return {int(row["id"]): dict(row) for row in rows}


def candidate_memory_ids(recall: Row, oracle: Row, top_k: int) -> set[int]:
    ids: set[int] = set()
    for candidate in (recall.get("ranking") or [])[:top_k]:
        if candidate.get("memory_id") is not None:
            ids.add(int(candidate["memory_id"]))
    for memory_id in recall.get("selected_memory_ids") or []:
        ids.add(int(memory_id))
    for result in oracle.get("results") or []:
        if result.get("memory_id") is not None:
            ids.add(int(result["memory_id"]))
    return ids


def load_label_cache(path: Path, model: str) -> dict[tuple[int, int], Row]:
    cache = {}
    for row in load_jsonl(path):
        if str(row.get("model")) != model:
            continue
        score = CALIBRATION.numeric_score(row.get("adjudicator_score"))
        if score is None:
            continue
        cache[(int(row["run_id"]), int(row["memory_id"]))] = row
    return cache


def score_one(
    *,
    api_key: str,
    model: str,
    timeout_seconds: float,
    query_chars: int,
    run_id: int,
    recall: Row,
    memory_id: int,
    memory: Row,
) -> Row:
    row = {
        "query_text": CALIBRATION.truncate(str(recall.get("query_text") or ""), query_chars),
        "memory_title": CALIBRATION.truncate(str(memory.get("title") or "(untitled memory)"), 500),
        "memory_body": CALIBRATION.truncate(str(memory.get("body") or memory.get("title") or ""), 4_000),
    }
    attempts = 4
    for attempt in range(1, attempts + 1):
        try:
            result = CALIBRATION.call_anthropic_adjudicator(
                row=row,
                model=model,
                api_key=api_key,
                timeout_seconds=timeout_seconds,
            )
            break
        except (
            urllib.error.HTTPError,
            urllib.error.URLError,
            http.client.HTTPException,
            ConnectionError,
            TimeoutError,
            ValueError,
        ):
            if attempt == attempts:
                raise
            time.sleep(2 * attempt)
    return {
        "run_id": run_id,
        "memory_id": memory_id,
        "model": model,
        "adjudicator_score": result["score"],
        "adjudicator_rationale": CALIBRATION.truncate(result["rationale"], 700),
        "memory_kind": memory.get("memory_kind"),
        "memory_is_active": bool(memory.get("is_active")),
        "label_source": "dense_adjudicator",
        "scored_at": dt.datetime.now(dt.UTC).isoformat(),
    }


def write_dense_oracles(
    out_dir: Path,
    anchors: list[Row],
    recalls: dict[int, Row],
    oracles: dict[int, Row],
    cache: dict[tuple[int, int], Row],
    memories: dict[int, Row],
) -> Row:
    oracle_dir = out_dir / "dense-oracles"
    oracle_dir.mkdir(parents=True, exist_ok=True)
    stats = {
        "anchors": 0,
        "labels": 0,
        "useful_labels": 0,
        "low_labels": 0,
        "anchors_with_useful": 0,
        "anchors_with_any_label": 0,
        "score_histogram": {str(score): 0 for score in range(1, 6)},
    }
    for anchor in anchors:
        run_id = anchor["run_id"]
        results = []
        for (cache_run_id, memory_id), row in cache.items():
            if cache_run_id != run_id:
                continue
            score = int(row["adjudicator_score"])
            memory = memories.get(memory_id) or {}
            results.append(
                {
                    "memory_id": memory_id,
                    "judge_score": str(score),
                    "memory_title": str(memory.get("title") or ""),
                    "judge_rationale": row.get("adjudicator_rationale") or "",
                    "label_source": "dense_adjudicator",
                    "adjudicator_model": row["model"],
                }
            )
            stats["labels"] += 1
            stats["score_histogram"][str(score)] += 1
            if score >= 4:
                stats["useful_labels"] += 1
            if score <= 2:
                stats["low_labels"] += 1
        results.sort(key=lambda result: result["memory_id"])
        dense = {
            "run": {
                "session_id": anchor["session_id"],
                "turn_ordinal": anchor["turn_ordinal"],
                "source_run_id": run_id,
                "oracle_kind": "dense_adjudicator",
            },
            "results": results,
        }
        (oracle_dir / f"oracle-{run_id}.json").write_text(
            json.dumps(dense, indent=2, sort_keys=True) + "\n"
        )
        stats["anchors"] += 1
        if results:
            stats["anchors_with_any_label"] += 1
        if any(int(result["judge_score"]) >= 4 for result in results):
            stats["anchors_with_useful"] += 1
    return stats


def build_report(manifest: Row) -> str:
    stats = manifest["stats"]
    histogram = stats["score_histogram"]
    lines = [
        "# Dense Oracle Labels",
        "",
        f"Date: {manifest['date']}",
        f"Adjudicator model: `{manifest['model']}`",
        f"Input directory: `{manifest['input_dir']}`",
        "",
        "## Method",
        "",
        "Scored the saved ranked candidate pool (top "
        f"{manifest['top_k']}, plus production-selected and historical-oracle "
        "memory ids) for every anchor with the calibration adjudicator prompt "
        "(query + memory + rubric only; no rank metadata, no prior judge "
        "score). Dense per-run oracle files replace historical "
        "production-judge rows when experiments set `BACKTEST_ORACLE_DIR`.",
        "",
        "## Coverage",
        "",
        f"- Anchors: {stats['anchors']}",
        f"- Anchors with at least one label: {stats['anchors_with_any_label']}",
        f"- Anchors with a useful (>=4) label: {stats['anchors_with_useful']}",
        f"- Total labels: {stats['labels']}",
        f"- Useful labels (>=4): {stats['useful_labels']}",
        f"- Low labels (<=2): {stats['low_labels']}",
        f"- Skipped candidates missing from the memory database: {manifest['missing_memories']}",
        "",
        "## Score Distribution",
        "",
        "| Score | Labels |",
        "| ---: | ---: |",
        *(f"| {score} | {histogram[str(score)]} |" for score in range(1, 6)),
        "",
        "## Caveats",
        "",
        "- Labels are LLM adjudication, not human ground truth; the "
        "adjudicator's own validity rests on the 2026-07 judge-calibration "
        "study.",
        "- Labels cover the saved candidate pools only. A future strategy "
        "that surfaces memories outside these pools needs a label top-up run "
        "against its own recall artifacts (the cache is append-only and "
        "keyed by run/memory/model).",
        "",
        "## Artifacts",
        "",
        "- `labels.jsonl`: append-only adjudicator score cache.",
        "- `dense-oracles/oracle-<run_id>.json`: per-run dense oracle files "
        "(`BACKTEST_ORACLE_DIR` target).",
        "- `manifest.json`: inputs, parameters, and coverage stats.",
        "",
    ]
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, action="append", required=True)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", default="anchor-refresh")
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--date", default=dt.date.today().isoformat())
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--api-key-env", default="ANTHROPIC_API_KEY")
    parser.add_argument("--top-k", type=int, default=16)
    parser.add_argument("--query-chars", type=int, default=4_000)
    parser.add_argument("--limit", type=int, help="score at most this many missing candidates")
    parser.add_argument("--workers", type=int, default=8)
    parser.add_argument("--timeout-seconds", type=float, default=60.0)
    parser.add_argument("--score-only", action="store_true",
                        help="only extend the label cache; skip dense oracle emission")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    api_key = os.environ.get(args.api_key_env)
    if not api_key:
        raise SystemExit(f"{args.api_key_env} is not set")

    anchors = read_anchor_rows([path.expanduser().resolve() for path in args.anchors_file])
    recalls: dict[int, Row] = {}
    oracles: dict[int, Row] = {}
    wanted: dict[int, set[int]] = {}
    all_memory_ids: set[int] = set()
    for anchor in anchors:
        run_id = anchor["run_id"]
        recall = load_json(args.input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        oracle = load_json(args.input_dir / f"oracle-{run_id}.json")
        ids = candidate_memory_ids(recall, oracle, args.top_k)
        recalls[run_id] = recall
        oracles[run_id] = oracle
        wanted[run_id] = ids
        all_memory_ids.update(ids)

    memories = load_memories(args.db.expanduser(), all_memory_ids)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    labels_path = args.out_dir / "labels.jsonl"
    cache = load_label_cache(labels_path, args.model)

    tasks = []
    missing_memories = 0
    for anchor in anchors:
        run_id = anchor["run_id"]
        for memory_id in sorted(wanted[run_id]):
            if (run_id, memory_id) in cache:
                continue
            memory = memories.get(memory_id)
            if memory is None:
                missing_memories += 1
                continue
            tasks.append((run_id, memory_id, memory))
    if args.limit is not None:
        tasks = tasks[: args.limit]

    write_lock = Lock()
    completed = 0
    errors: list[str] = []
    if tasks:
        with ThreadPoolExecutor(max_workers=max(1, args.workers)) as executor:
            futures = {
                executor.submit(
                    score_one,
                    api_key=api_key,
                    model=args.model,
                    timeout_seconds=args.timeout_seconds,
                    query_chars=args.query_chars,
                    run_id=run_id,
                    recall=recalls[run_id],
                    memory_id=memory_id,
                    memory=memory,
                ): (run_id, memory_id)
                for run_id, memory_id, memory in tasks
            }
            for future in as_completed(futures):
                run_id, memory_id = futures[future]
                try:
                    detail = future.result()
                except (
                    urllib.error.HTTPError,
                    urllib.error.URLError,
                    http.client.HTTPException,
                    ConnectionError,
                    TimeoutError,
                    ValueError,
                ) as exc:
                    errors.append(f"run {run_id} memory {memory_id}: {exc}")
                    continue
                with write_lock:
                    with labels_path.open("a") as handle:
                        handle.write(json.dumps(detail, sort_keys=True) + "\n")
                    cache[(run_id, memory_id)] = detail
                completed += 1
                if not args.quiet and completed % 100 == 0:
                    print(f"scored {completed}/{len(tasks)}", file=sys.stderr)

    if errors:
        for line in errors[:10]:
            print(f"ERROR: {line}", file=sys.stderr)
        raise SystemExit(
            f"{len(errors)} candidates failed scoring; rerun to retry (cache preserves progress)"
        )

    if args.score_only:
        print(json.dumps({"scored_this_run": completed, "cache": str(labels_path)}, indent=2))
        return

    stats = write_dense_oracles(args.out_dir, anchors, recalls, oracles, cache, memories)
    manifest = {
        "date": args.date,
        "kind": "dense_oracle_labels",
        "model": args.model,
        "prompt_source": "scripts/judge-calibration.py adjudicator (query+memory+rubric)",
        "input_dir": str(args.input_dir),
        "strategy_label": args.strategy_label,
        "anchors_files": [str(path) for path in args.anchors_file],
        "top_k": args.top_k,
        "query_chars": args.query_chars,
        "scored_this_run": completed,
        "missing_memories": missing_memories,
        "stats": stats,
        "labels_path": str(labels_path),
        "dense_oracle_dir": str(args.out_dir / "dense-oracles"),
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (args.out_dir / "REPORT.md").write_text(build_report(manifest))
    print(json.dumps(manifest, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
