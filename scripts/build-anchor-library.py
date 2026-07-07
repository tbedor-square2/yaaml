#!/usr/bin/env python3
"""Build frozen screening/holdout anchor libraries from a backtest run.

Implements the "Phase 0 Runbook: Anchor Libraries" procedure in
experiments/recall/README.md. Input is a scripts/backtest-recall-strategy.sh
output directory (anchors.tsv plus oracle-<run_id>.json files). The anchor
pool is split deterministically into a screening set and a holdout set,
stratified so each set keeps a proportional share of anchors with known
oracle labels, with zero overlap between the sets.

Outputs, under --out-dir:

- <label>-screening.tsv
- <label>-holdout.tsv
- <label>-manifest.json  (counts, known-score coverage, seed, provenance)

Example:

    python3 scripts/build-anchor-library.py \
      --input-dir target/anchor-refresh-2026-07 \
      --label 2026-07 \
      --screening 200 --holdout 100
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import random
import re
import sys
from pathlib import Path
from typing import Any

MIN_COVERAGE = 0.30
DEFAULT_SEED = 20260706


def read_anchors(path: Path) -> list[dict[str, Any]]:
    anchors = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) != 4:
            raise SystemExit(f"malformed anchor line (expected 4 tab-separated fields): {line!r}")
        run_id, session_id, turn_ordinal, case_label = fields
        anchors.append(
            {
                "run_id": int(run_id),
                "session_id": session_id,
                "turn_ordinal": int(turn_ordinal),
                "case_label": case_label,
            }
        )
    if not anchors:
        raise SystemExit(f"no anchors in {path}")
    return anchors


def annotate_oracle(anchors: list[dict[str, Any]], input_dir: Path) -> None:
    score_pattern = re.compile(r"^[1-5]$")
    for anchor in anchors:
        oracle_path = input_dir / f"oracle-{anchor['run_id']}.json"
        anchor["has_known_labels"] = False
        anchor["oracle_has_useful"] = False
        if not oracle_path.exists():
            continue
        try:
            oracle = json.loads(oracle_path.read_text())
        except json.JSONDecodeError:
            continue
        scores = [
            int(result["judge_score"])
            for result in oracle.get("results") or []
            if score_pattern.match(str(result.get("judge_score", "")))
        ]
        anchor["has_known_labels"] = bool(scores)
        anchor["oracle_has_useful"] = any(score >= 4 for score in scores)


def stratified_split(
    anchors: list[dict[str, Any]],
    screening_size: int,
    holdout_size: int,
    seed: int,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    total_needed = screening_size + holdout_size
    if len(anchors) < total_needed:
        raise SystemExit(
            f"anchor pool has {len(anchors)} anchors but "
            f"--screening {screening_size} + --holdout {holdout_size} = "
            f"{total_needed} requested; regenerate with a larger "
            f"BACKTEST_ANCHOR_LIMIT or reduce the set sizes"
        )
    rng = random.Random(seed)
    labeled = [anchor for anchor in anchors if anchor["has_known_labels"]]
    unlabeled = [anchor for anchor in anchors if not anchor["has_known_labels"]]
    rng.shuffle(labeled)
    rng.shuffle(unlabeled)

    holdout_fraction = holdout_size / total_needed
    holdout_labeled = round(len(labeled) * total_needed / len(anchors) * holdout_fraction)
    holdout_labeled = min(holdout_labeled, len(labeled), holdout_size)

    holdout = labeled[:holdout_labeled]
    holdout += unlabeled[: holdout_size - len(holdout)]
    remaining_labeled = labeled[holdout_labeled:]
    remaining_unlabeled = unlabeled[holdout_size - holdout_labeled :]

    screening_labeled = min(
        round(len(remaining_labeled) * screening_size / max(1, len(anchors) - holdout_size)),
        len(remaining_labeled),
        screening_size,
    )
    screening = remaining_labeled[:screening_labeled]
    screening += remaining_unlabeled[: screening_size - len(screening)]

    if len(screening) < screening_size or len(holdout) < holdout_size:
        raise SystemExit(
            "stratified split could not fill both sets; check labeled/unlabeled balance"
        )
    return screening, holdout


def coverage(anchors: list[dict[str, Any]]) -> float:
    return sum(1 for anchor in anchors if anchor["has_known_labels"]) / len(anchors)


def load_backtest_summary(input_dir: Path) -> dict[str, Any] | None:
    summaries = sorted(input_dir.glob("*.summary.json"))
    if not summaries:
        return None
    if len(summaries) > 1:
        raise SystemExit(
            f"expected at most one *.summary.json in {input_dir}, found {len(summaries)}"
        )
    try:
        return json.loads(summaries[0].read_text())
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse backtest summary {summaries[0]}: {exc}") from exc


def database_snapshot() -> dict[str, Any]:
    db_path = Path(os.environ.get("BACKTEST_DB", "~/.yaaml/yaaml.db")).expanduser()
    snapshot: dict[str, Any] = {"path": str(db_path)}
    try:
        stat = db_path.stat()
    except FileNotFoundError:
        snapshot["exists"] = False
        return snapshot
    snapshot.update(
        {
            "exists": True,
            "size_bytes": stat.st_size,
            "modified_at": dt.datetime.fromtimestamp(
                stat.st_mtime,
                tz=dt.timezone.utc,
            ).isoformat(),
        }
    )
    return snapshot


def selection_query_parameters(summary: dict[str, Any] | None, pool_size: int) -> dict[str, Any]:
    anchor_origins = [
        origin.strip()
        for origin in os.environ.get(
            "BACKTEST_ANCHOR_ORIGINS",
            "session_background,tool_pre_use",
        ).split(",")
        if origin.strip()
    ]
    return {
        "anchor_source": (summary or {}).get("anchor_source", "unknown"),
        "anchor_limit": os.environ.get("BACKTEST_ANCHOR_LIMIT", str(pool_size)),
        "exclude_context_embedded": os.environ.get(
            "BACKTEST_EXCLUDE_CONTEXT_EMBEDDED",
            "1",
        )
        == "1",
        "recall_origins": anchor_origins,
        "requires_completed_turn_at_or_before_anchor": True,
        "dedupe": "latest_per_session_turn",
        "order": "run_id DESC",
    }


def write_tsv(path: Path, anchors: list[dict[str, Any]]) -> None:
    lines = [
        f"{a['run_id']}\t{a['session_id']}\t{a['turn_ordinal']}\t{a['case_label']}"
        for a in sorted(anchors, key=lambda a: -a["run_id"])
    ]
    path.write_text("\n".join(lines) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True,
                        help="backtest-recall-strategy.sh output dir (anchors.tsv + oracle-*.json)")
    parser.add_argument("--label", default=dt.date.today().strftime("%Y-%m"))
    parser.add_argument("--screening", type=int, default=200)
    parser.add_argument("--holdout", type=int, default=100)
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "experiments" / "recall" / "anchor-libraries",
    )
    args = parser.parse_args()

    input_dir = args.input_dir.expanduser().resolve()
    anchors = read_anchors(input_dir / "anchors.tsv")
    annotate_oracle(anchors, input_dir)
    summary = load_backtest_summary(input_dir)

    screening, holdout = stratified_split(anchors, args.screening, args.holdout, args.seed)

    screening_ids = {a["run_id"] for a in screening}
    holdout_ids = {a["run_id"] for a in holdout}
    overlap = screening_ids & holdout_ids
    if overlap:
        raise SystemExit(f"internal error: screening/holdout overlap on run_ids {sorted(overlap)}")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    screening_path = args.out_dir / f"{args.label}-screening.tsv"
    holdout_path = args.out_dir / f"{args.label}-holdout.tsv"
    manifest_path = args.out_dir / f"{args.label}-manifest.json"
    for path in (screening_path, holdout_path, manifest_path):
        if path.exists():
            raise SystemExit(f"refusing to overwrite existing library file: {path}")

    write_tsv(screening_path, screening)
    write_tsv(holdout_path, holdout)

    manifest = {
        "label": args.label,
        "generated_at": dt.date.today().isoformat(),
        "generated_by": "scripts/build-anchor-library.py",
        "seed": args.seed,
        "input_dir": str(input_dir),
        "database_snapshot": database_snapshot(),
        "selection_query_parameters": selection_query_parameters(summary, len(anchors)),
        "backtest_summary": None
        if summary is None
        else {
            "strategy": summary.get("strategy"),
            "anchor_source": summary.get("anchor_source"),
            "anchors": summary.get("anchors"),
            "selected_memories": summary.get("selected_memories"),
            "empty_recall_runs": summary.get("empty_recall_runs"),
            "oracle_useful_runs": summary.get("oracle_useful_runs"),
        },
        "pool_anchors": len(anchors),
        "pool_known_score_coverage": coverage(anchors),
        "screening": {
            "path": str(screening_path),
            "anchors": len(screening),
            "known_score_coverage": coverage(screening),
            "oracle_useful_anchors": sum(1 for a in screening if a["oracle_has_useful"]),
        },
        "holdout": {
            "path": str(holdout_path),
            "anchors": len(holdout),
            "known_score_coverage": coverage(holdout),
            "oracle_useful_anchors": sum(1 for a in holdout if a["oracle_has_useful"]),
        },
    }
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(json.dumps(manifest, indent=2, sort_keys=True))

    for name, anchor_set in (("screening", screening), ("holdout", holdout)):
        set_coverage = coverage(anchor_set)
        if set_coverage < MIN_COVERAGE:
            print(
                f"WARNING: {name} known-score coverage {set_coverage:.1%} is below "
                f"the {MIN_COVERAGE:.0%} done-criteria threshold in "
                f"experiments/recall/README.md; densify oracle labels before "
                f"using this library for decisions",
                file=sys.stderr,
            )


if __name__ == "__main__":
    main()
