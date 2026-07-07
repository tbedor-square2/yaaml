#!/usr/bin/env python3
"""Adjudicator-gated source-overlap restoration, simulated offline.

Backlog follow-up to the 2026-07-07 selection-tuning result: the blanket
`drop:source_turn_already_in_query` removal is holdout-confirmed useful but
~56% redundant, and lexical containment cannot separate the two (calibration:
incremental mean containment 0.444 vs redundant 0.481). This simulates the
gate that does discriminate — the redundancy-aware adjudicator — by restoring
a source-overlap-dropped candidate only when its cached redundancy-aware
score is >= --gate-threshold. Candidates without a cached score stay dropped
(conservative).

Metrics are reported under two oracles:
- raw dense labels (calibrated instrument, redundancy-blind), and
- adjusted labels (dense, overridden by redundancy-aware scores for the
  restored selections that have them).

Caveat recorded in the report: the gate and the adjustment share an
instrument, so adjusted deltas partially evaluate the gate with its own
judge; the dense-label deltas and the two-instrument agreement are the
evidence, and forward production evals are the confirmation path.
"""

from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

SCRIPTS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS_DIR))

from recall_experiment_stats import paired_bootstrap_deltas

spec = importlib.util.spec_from_file_location(
    "selection_tuning", SCRIPTS_DIR / "selection-tuning-experiment.py"
)
ST = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ST)

Row = dict[str, Any]
METRICS = ST.METRICS


def load_redundancy_scores(path: Path) -> dict[tuple[int, int], int]:
    scores = {}
    for row in ST.load_jsonl(path):
        value = row.get("redundancy_aware_score")
        if value is not None:
            scores[(int(row["run_id"]), int(row["memory_id"]))] = int(value)
    return scores


def gated_selection(
    recall: Row,
    run_id: int,
    redundancy: dict[tuple[int, int], int],
    gate_threshold: int,
    limit: int,
) -> list[int]:
    params = dict(ST.PRODUCTION_PARAMS)
    candidates = []
    for candidate in recall.get("ranking") or []:
        if ST.eligible_for_filter_pool(candidate):
            candidates.append(candidate)
            continue
        if ST.eligible_for_filter_pool(candidate, allow_source_overlap=True):
            score = redundancy.get((run_id, int(candidate["memory_id"])))
            if score is not None and score >= gate_threshold:
                candidates.append(candidate)
    candidates.sort(key=lambda row: (-float(row["score"]), int(row["memory_id"])))
    return ST.strict_kind_selection(candidates, params, limit)


def adjusted_oracle(oracle: Row, run_id: int, redundancy: dict[tuple[int, int], int]) -> Row:
    results = []
    for result in oracle.get("results") or []:
        memory_id = int(result["memory_id"])
        override = redundancy.get((run_id, memory_id))
        if override is not None:
            result = {**result, "judge_score": str(override), "label_source": "redundancy_aware"}
        results.append(result)
    return {**oracle, "results": results}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", default="anchor-refresh")
    parser.add_argument("--oracle-dir", type=Path, required=True)
    parser.add_argument("--redundancy-cases", type=Path, required=True)
    parser.add_argument("--gate-threshold", type=int, default=4)
    parser.add_argument("--selection-limit", type=int, default=2)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--date", default=dt.date.today().isoformat())
    args = parser.parse_args()

    anchors = ST.read_anchors(args.anchors_file.expanduser().resolve())
    redundancy = load_redundancy_scores(args.redundancy_cases.expanduser().resolve())
    args.out_dir.mkdir(parents=True, exist_ok=True)

    arms: dict[str, dict[str, list[Row]]] = {
        "replay_default": {"raw": [], "adjusted": []},
        "allow_source_overlap": {"raw": [], "adjusted": []},
        "gated_overlap": {"raw": [], "adjusted": []},
    }
    restored_by_gate = 0
    for anchor in anchors:
        run_id = anchor["run_id"]
        recall = ST.load_json(args.input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        oracle = ST.load_json(args.oracle_dir / f"oracle-{run_id}.json")
        adj = adjusted_oracle(oracle, run_id, redundancy)
        selections = {
            "replay_default": ST.variant_selection(recall, {}, args.selection_limit),
            "allow_source_overlap": ST.variant_selection(
                recall, {"allow_source_overlap": True}, args.selection_limit
            ),
            "gated_overlap": gated_selection(
                recall, run_id, redundancy, args.gate_threshold, args.selection_limit
            ),
        }
        restored_by_gate += len(
            set(selections["gated_overlap"]) - set(selections["replay_default"])
        )
        for label, selected in selections.items():
            arms[label]["raw"].append(ST.details_row(label, anchor, oracle, selected))
            arms[label]["adjusted"].append(
                ST.details_row(f"{label}_adjusted", anchor, adj, selected)
            )

    for label, rows in arms.items():
        (args.out_dir / f"{label}.details.jsonl").write_text(
            "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows["raw"])
        )

    deltas = {}
    summaries = []
    for oracle_kind in ("raw", "adjusted"):
        baseline = arms["replay_default"][oracle_kind]
        for label in ("allow_source_overlap", "gated_overlap"):
            deltas[f"{label}_{oracle_kind}"] = paired_bootstrap_deltas(
                baseline, arms[label][oracle_kind], METRICS
            )
        for label in arms:
            summaries.append(ST.summarize(f"{label}_{oracle_kind}", arms[label][oracle_kind]))

    gated_adjusted = deltas["gated_overlap_adjusted"]
    useful_ci = gated_adjusted["useful_capture_runs"]["ci_95"]
    low_ci = gated_adjusted["low_selection_runs"]["ci_95"]
    if useful_ci and useful_ci[0] > 0 and (low_ci is None or low_ci[0] <= 0):
        decision = (
            "Gated restoration is promising: adjusted useful-capture CI excludes zero"
            " without a confirmed low-selection increase. Next step is a runtime"
            " implementation behind an env flag (async daemon-side adjudicator call"
            " for source-overlap-dropped candidates) with forward production evals"
            " as the independent confirmation, given the shared-instrument caveat."
        )
    else:
        decision = (
            "Gated restoration did not clear the adjusted useful-capture bar;"
            " record as no detectable effect."
        )

    manifest = {
        "date": args.date,
        "kind": "source_overlap_gate_simulation",
        "input_dir": str(args.input_dir),
        "anchors_file": str(args.anchors_file),
        "oracle_dir": str(args.oracle_dir),
        "redundancy_cases": str(args.redundancy_cases),
        "gate_threshold": args.gate_threshold,
        "selection_limit": args.selection_limit,
        "gate_restored_selections": restored_by_gate,
        "summaries": summaries,
        "deltas": deltas,
        "decision": decision,
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

    lines = [
        "# Adjudicator-Gated Source-Overlap Restoration (Offline Simulation)",
        "",
        f"Date: {manifest['date']}",
        f"Gate: restore `drop:source_turn_already_in_query` candidates with cached"
        f" redundancy-aware score >= {args.gate_threshold}; unscored candidates stay dropped.",
        f"Gate-restored selections: {restored_by_gate}",
        "",
        "## Summary (raw dense oracle and redundancy-adjusted oracle)",
        "",
        "| Strategy | Avg score | Useful sel | Low sel | Useful runs | Low runs | Avg mem | Empty | Missed-useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for item in summaries:
        avg = item["average_known_score"]
        lines.append(
            "| {strategy} | {avg} | {useful:.0f} | {low:.0f} | {uruns:.0f} | {lruns:.0f} | {mem:.2f} | {empty:.0f} | {missed:.0f} |".format(
                strategy=item["strategy"],
                avg="n/a" if avg is None else f"{avg:.2f}",
                useful=item["useful_known_selected"],
                low=item["low_known_selected"],
                uruns=item["useful_capture_runs"],
                lruns=item["low_selection_runs"],
                mem=item["average_selected_per_anchor"],
                empty=item["empty_recall_runs"],
                missed=item["missed_useful_empty_runs"],
            )
        )
    lines.extend([
        "",
        "## Significance (paired bootstrap vs `replay_default`, 95% CI)",
        "",
        "| Arm x oracle | Metric | Delta | 95% CI | Verdict |",
        "| --- | --- | ---: | ---: | --- |",
    ])
    for label, delta_set in deltas.items():
        for metric in METRICS:
            entry = delta_set[metric]
            ci = entry["ci_95"]
            point = entry["point_delta"]
            lines.append(
                "| {label} | `{metric}` | {point} | {ci} | {verdict} |".format(
                    label=label,
                    metric=metric,
                    point="n/a" if point is None else f"{point:+.3f}",
                    ci="n/a" if ci is None else f"[{ci[0]:+.3f}, {ci[1]:+.3f}]",
                    verdict=entry["verdict"],
                )
            )
    lines.extend([
        "",
        "## Caveats",
        "",
        "- The gate and the adjusted oracle share the redundancy-aware"
        " adjudicator, so adjusted deltas partially evaluate the gate with its"
        " own judge. The independent evidence is the raw dense-label deltas"
        " plus two-instrument agreement; forward production evals are the"
        " confirmation path before any default-on ship.",
        "- The lexical-containment alternative was invalidated by calibration:"
        " incremental mean containment 0.444 vs redundant 0.481 on the 82"
        " labeled restored selections — no usable separation at any threshold.",
        "- Unscored source-overlap candidates stay dropped in this simulation;"
        " a runtime gate would score them live (async daemon path).",
        "",
        "## Decision",
        "",
        decision,
        "",
    ])
    (args.out_dir / "REPORT.md").write_text("\n".join(lines))
    print(json.dumps({"out_dir": str(args.out_dir), "decision": decision,
                      "gate_restored_selections": restored_by_gate}, indent=2))


if __name__ == "__main__":
    main()
