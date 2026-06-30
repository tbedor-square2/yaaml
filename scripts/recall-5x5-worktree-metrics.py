#!/usr/bin/env python3
"""Run comparable YAAML recall metrics for 5x5 worktree experiments.

This script is the metrics runner for the agent-driven 5x5 loop:

1. implement candidate approaches in separate worktrees,
2. run the same recall backtest over each worktree,
3. collate comparable metrics and deltas against a baseline,
4. write structured artifacts and a report.

It delegates per-worktree recall probing to scripts/backtest-recall-strategy.sh.
The baseline run chooses anchors unless --anchors-file is supplied; candidate
runs reuse that anchor file so metrics are directly comparable.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
from pathlib import Path
from typing import Any


Row = dict[str, Any]


def parse_named_path(value: str) -> tuple[str, Path]:
    if "=" not in value:
        raise argparse.ArgumentTypeError("expected NAME=PATH")
    name, path = value.split("=", 1)
    name = safe_name(name.strip())
    if not name:
        raise argparse.ArgumentTypeError("candidate name cannot be empty")
    return name, Path(path).expanduser().resolve()


def safe_name(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "-", value).strip("-")


def load_json(path: Path) -> Row:
    return json.loads(path.read_text())


def average(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def read_jsonl(path: Path) -> list[Row]:
    return [
        json.loads(line)
        for line in path.read_text().splitlines()
        if line.strip()
    ]


def run_backtest(
    *,
    script: Path,
    repo: Path,
    label: str,
    out_dir: Path,
    db: Path,
    anchor_source: str,
    anchor_limit: str,
    anchors_file: Path | None,
    oracle_bin: str,
) -> None:
    env = os.environ.copy()
    env["BACKTEST_OUT_DIR"] = str(out_dir)
    env["BACKTEST_DB"] = str(db)
    env["BACKTEST_ANCHOR_SOURCE"] = anchor_source
    env["BACKTEST_ANCHOR_LIMIT"] = anchor_limit
    env["YAAML_ORACLE_BIN"] = oracle_bin
    if anchors_file is not None:
        env["BACKTEST_ANCHORS_FILE"] = str(anchors_file)
    subprocess.run(
        [str(script), str(repo), label],
        check=True,
        cwd=repo,
        env=env,
    )


def metric_summary(label: str, out_dir: Path) -> Row:
    details_path = out_dir / f"{label}.details.jsonl"
    summary_path = out_dir / f"{label}.summary.json"
    if not details_path.exists():
        raise FileNotFoundError(f"missing details file: {details_path}")
    if not summary_path.exists():
        raise FileNotFoundError(f"missing summary file: {summary_path}")
    summary = load_json(summary_path)
    rows = read_jsonl(details_path)
    anchors = len(rows)
    empty_recall_runs = sum(1 for row in rows if row["empty_recall"])
    missed_useful_empty_runs = sum(
        1 for row in rows if row["empty_recall"] and row["oracle_has_useful"]
    )
    clean_abstention_runs = sum(
        1 for row in rows if row["empty_recall"] and not row["oracle_has_useful"]
    )
    oracle_useful_runs = sum(1 for row in rows if row["oracle_has_useful"])
    selected_counts = [float(row["selected_count"]) for row in rows]
    low_selection_runs = sum(1 for row in rows if row["selected_any_known_low"])
    useful_capture_runs = sum(1 for row in rows if row["captured_any_known_useful"])
    known_scores = [
        float(row["average_known_score"])
        for row in rows
        if row["average_known_score"] is not None
    ]
    return {
        "strategy": label,
        "anchors": anchors,
        "selected_memories": summary["selected_memories"],
        "average_selected_per_anchor": average(selected_counts),
        "known_selected_memories": summary["known_selected_memories"],
        "unknown_selected_memories": summary["unknown_selected_memories"],
        "average_known_score": average(known_scores),
        "useful_known_selected": summary["useful_known_selected"],
        "low_known_selected": summary["low_known_selected"],
        "useful_capture_runs": useful_capture_runs,
        "low_selection_runs": low_selection_runs,
        "oracle_useful_runs": oracle_useful_runs,
        "empty_recall_runs": empty_recall_runs,
        "empty_recall_rate": empty_recall_runs / anchors if anchors else None,
        "missed_useful_empty_runs": missed_useful_empty_runs,
        "missed_useful_empty_rate": (
            missed_useful_empty_runs / oracle_useful_runs
            if oracle_useful_runs
            else None
        ),
        "clean_abstention_runs": clean_abstention_runs,
        "clean_abstention_rate": clean_abstention_runs / anchors if anchors else None,
        "details_path": str(details_path),
        "summary_path": str(summary_path),
    }


def add_deltas(summary: Row, baseline: Row) -> Row:
    delta_keys = [
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
    summary["delta_vs_baseline"] = {
        key: (
            None
            if summary.get(key) is None or baseline.get(key) is None
            else summary[key] - baseline[key]
        )
        for key in delta_keys
    }
    return summary


def format_float(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.2f}"


def format_rate(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.1%}"


def format_delta(value: float | int | None, *, digits: int = 2) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, int):
        return f"{value:+}"
    return f"{value:+.{digits}f}"


def write_report(path: Path, manifest: Row, summaries: list[Row]) -> None:
    baseline = summaries[0]
    candidate_summaries = summaries[1:]
    balanced = [
        summary
        for summary in candidate_summaries
        if summary["useful_capture_runs"] >= baseline["useful_capture_runs"]
        and summary["missed_useful_empty_runs"] <= baseline["missed_useful_empty_runs"] + 2
    ]
    best = (
        min(
            balanced,
            key=lambda summary: (
                summary["low_known_selected"],
                summary["average_selected_per_anchor"] or 99.0,
                -summary["useful_known_selected"],
            ),
        )
        if balanced
        else None
    )
    lines = [
        "# YAAML 5x5 Worktree Metrics",
        "",
        f"Date: {manifest['date']}",
        f"Anchor source: `{manifest['anchor_source']}`",
        f"Anchor limit: `{manifest['anchor_limit']}`",
        f"Anchors: {baseline['anchors']}",
        "",
        "## Method",
        "",
        "Each row is a separate implementation worktree evaluated with the same saved anchor set.",
        "Per-worktree recall probes are generated by `scripts/backtest-recall-strategy.sh`.",
        "Empty recall is reported as abstention, not as a low score.",
        "",
        "## Summary",
        "",
        "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for summary in summaries:
        lines.append(
            "| {strategy} | {score} | {useful} | {low} | {useful_runs} | {low_runs} | {avg_mem} | {empty} | {missed} |".format(
                strategy=summary["strategy"],
                score=format_float(summary["average_known_score"]),
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
        "## Deltas Vs Baseline",
        "",
        "| Strategy | Avg score | Useful selected | Low selected | Useful runs | Low runs | Avg memories | Empty runs | Missed useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ])
    for summary in candidate_summaries:
        delta = summary["delta_vs_baseline"]
        lines.append(
            "| {strategy} | {score} | {useful} | {low} | {useful_runs} | {low_runs} | {avg_mem} | {empty} | {missed} |".format(
                strategy=summary["strategy"],
                score=format_delta(delta["average_known_score"]),
                useful=format_delta(delta["useful_known_selected"], digits=0),
                low=format_delta(delta["low_known_selected"], digits=0),
                useful_runs=format_delta(delta["useful_capture_runs"], digits=0),
                low_runs=format_delta(delta["low_selection_runs"], digits=0),
                avg_mem=format_delta(delta["average_selected_per_anchor"]),
                empty=format_delta(delta["empty_recall_runs"], digits=0),
                missed=format_delta(delta["missed_useful_empty_runs"], digits=0),
            )
        )
    lines.extend(["", "## Recommendation", ""])
    if best is None:
        lines.append("No candidate preserved useful-run coverage within the missed-useful budget.")
    else:
        lines.append(
            "Most promising candidate: `{}`. It preserved useful-run coverage while producing {} low selected memories and {:.2f} average memories per anchor.".format(
                best["strategy"],
                best["low_known_selected"],
                best["average_selected_per_anchor"] or 0.0,
            )
        )
    lines.extend([
        "",
        "## Artifacts",
        "",
        f"1. Manifest: `{manifest['manifest_path']}`",
        f"2. Summary JSON: `{manifest['summary_path']}`",
    ])
    for index, summary in enumerate(summaries, start=3):
        lines.append(f"{index}. `{summary['strategy']}` details: `{summary['details_path']}`")
    path.write_text("\n".join(lines) + "\n")


def main() -> None:
    default_date = dt.date.today().isoformat()
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--baseline",
        type=parse_named_path,
        default=("baseline", Path.cwd().resolve()),
        help="Baseline worktree as NAME=PATH. Defaults to baseline=$PWD.",
    )
    parser.add_argument(
        "--candidate",
        action="append",
        type=parse_named_path,
        default=[],
        help="Candidate worktree as NAME=PATH. Repeat once per approach.",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(f"experiments/recall/{default_date}-5x5-worktree-metrics"),
    )
    parser.add_argument("--date", default=default_date)
    parser.add_argument("--anchor-source", default="eval-library")
    parser.add_argument("--anchor-limit", default="200")
    parser.add_argument("--anchors-file", type=Path)
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--oracle-bin", default="yaaml")
    parser.add_argument(
        "--backtest-script",
        type=Path,
        default=Path(__file__).with_name("backtest-recall-strategy.sh"),
    )
    parser.add_argument(
        "--skip-run",
        action="store_true",
        help="Only collate existing per-worktree backtest outputs under --out-dir/runs.",
    )
    args = parser.parse_args()

    worktrees = [args.baseline, *args.candidate]
    if len(worktrees) < 2:
        raise SystemExit("provide at least one --candidate NAME=PATH")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    runs_dir = args.out_dir / "runs"
    runs_dir.mkdir(parents=True, exist_ok=True)
    resolved_script = args.backtest_script.expanduser().resolve()
    if not resolved_script.exists():
        raise SystemExit(f"backtest script not found: {resolved_script}")

    shared_anchors = args.anchors_file.expanduser().resolve() if args.anchors_file else None
    if not args.skip_run:
        for index, (name, repo) in enumerate(worktrees):
            if not repo.exists():
                raise SystemExit(f"worktree not found for {name}: {repo}")
            run_dir = runs_dir / name
            run_dir.mkdir(parents=True, exist_ok=True)
            anchors_file = shared_anchors
            if index > 0 and anchors_file is None:
                anchors_file = runs_dir / worktrees[0][0] / "anchors.tsv"
            run_backtest(
                script=resolved_script,
                repo=repo,
                label=name,
                out_dir=run_dir,
                db=args.db.expanduser().resolve(),
                anchor_source=args.anchor_source,
                anchor_limit=args.anchor_limit,
                anchors_file=anchors_file,
                oracle_bin=args.oracle_bin,
            )

    summaries = []
    for name, _repo in worktrees:
        summaries.append(metric_summary(name, runs_dir / name))
    baseline = summaries[0]
    summaries = [baseline, *[add_deltas(summary, baseline) for summary in summaries[1:]]]

    summary = {
        "date": args.date,
        "anchor_source": args.anchor_source,
        "anchor_limit": args.anchor_limit,
        "worktrees": [{"name": name, "path": str(path)} for name, path in worktrees],
        "summaries": summaries,
    }
    summary_path = args.out_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    manifest = {
        "date": args.date,
        "kind": "recall_5x5_worktree_metrics",
        "anchor_source": args.anchor_source,
        "anchor_limit": args.anchor_limit,
        "anchors_file": str(shared_anchors) if shared_anchors else str(runs_dir / worktrees[0][0] / "anchors.tsv"),
        "db_path": str(args.db.expanduser().resolve()),
        "backtest_script": str(resolved_script),
        "worktrees": [{"name": name, "path": str(path)} for name, path in worktrees],
        "summary_path": str(summary_path),
        "manifest_path": str(args.out_dir / "manifest.json"),
        "report_path": str(args.out_dir / "REPORT.md"),
    }
    manifest_path = args.out_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    write_report(args.out_dir / "REPORT.md", manifest, summaries)
    print(json.dumps({"out_dir": str(args.out_dir), "summaries": summaries}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
