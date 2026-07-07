#!/usr/bin/env python3
"""Selection/health-rerank tuning replay against the dense oracle.

Backlog item "Selection/health-rerank tuning on dense labels". Production's
dominant failure under the dense oracle is over-abstention (93 of 101 empty
recalls had a useful memory available), and the pre-CI revalidation confirmed
`health_action_rerank` adds missed-useful empties. This experiment replays
saved production candidate rankings through parameterized variants of the
strict-kind selection policy (abstention threshold, second-slot threshold,
scaled negative health deltas) plus a cached-LLM-score arm, and scores every
arm against the dense adjudicator oracle with paired bootstrap CIs.

Saved-candidate replay only: no provider calls, no retrieval reruns.

Example:

    python3 scripts/selection-tuning-experiment.py \
      --anchors-file experiments/recall/anchor-libraries/2026-07-screening.tsv \
      --input-dir target/anchor-refresh-2026-07 \
      --strategy-label anchor-refresh \
      --oracle-dir experiments/recall/oracle-labels/2026-07/dense-oracles \
      --llm-scores experiments/recall/2026-07-07-llm-scoring-rerank/llm_scores.jsonl \
      --out-dir experiments/recall/2026-07-07-selection-tuning
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
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

PRODUCTION_PARAMS = {
    "abstain": 0.75,
    "second": 0.90,
    "risky": 1.50,
    "health_neg_scale": 1.0,
    "allow_source_overlap": False,
}

VARIANTS: dict[str, Row] = {
    "replay_default": {},
    "abstain_055": {"abstain": 0.55},
    "abstain_040": {"abstain": 0.40},
    "second_075": {"second": 0.75},
    "health_neg_half": {"health_neg_scale": 0.5},
    "combo_soft": {"abstain": 0.55, "second": 0.75, "health_neg_scale": 0.5},
    "allow_source_overlap": {"allow_source_overlap": True},
    "overlap_plus_soft": {
        "allow_source_overlap": True,
        "abstain": 0.55,
        "second": 0.75,
        "health_neg_scale": 0.5,
    },
}


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


def score_map(oracle: Row) -> dict[int, int]:
    scores = {}
    for result in oracle.get("results") or []:
        try:
            score = int(str(result.get("judge_score")))
        except (TypeError, ValueError):
            continue
        if 1 <= score <= 5 and result.get("memory_id") is not None:
            scores[int(result["memory_id"])] = score
    return scores


def details_row(label: str, anchor: Row, oracle: Row, selected_ids: list[int]) -> Row:
    scores = score_map(oracle)
    known = [scores[memory_id] for memory_id in selected_ids if memory_id in scores]
    oracle_values = list(scores.values())
    return {
        "strategy": label,
        "case": anchor["case"],
        "run_id": anchor["run_id"],
        "session_id": anchor["session_id"],
        "turn_ordinal": anchor["turn_ordinal"],
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known),
        "unknown_selected_count": len(selected_ids) - len(known),
        "average_known_score": sum(known) / len(known) if known else None,
        "useful_known_selected": sum(1 for score in known if score >= 4),
        "low_known_selected": sum(1 for score in known if score <= 2),
        "captured_any_known_useful": any(score >= 4 for score in known),
        "selected_any_known_low": any(score <= 2 for score in known),
        "oracle_has_useful": any(score >= 4 for score in oracle_values),
        "oracle_best_score": max(oracle_values) if oracle_values else None,
        "empty_recall": len(selected_ids) == 0,
    }


def eligible_for_filter_pool(candidate: Row, *, allow_source_overlap: bool = False) -> bool:
    reasons = candidate.get("filter_reasons") or []
    has_keep = any(
        reason.startswith("keep:") and reason != "keep:strict_kind_diverse"
        for reason in reasons
    )
    drops = [reason for reason in reasons if reason.startswith("drop:")]
    if allow_source_overlap:
        drops = [reason for reason in drops if reason != "drop:source_turn_already_in_query"]
    return has_keep and not drops


def negative_health_delta(candidate: Row) -> float:
    delta = 0.0
    for penalty in candidate.get("rank", {}).get("penalties") or []:
        match = re.match(r"health_action_rerank:[^:]+:[^:]+:([-+]?[0-9.]+)$", penalty)
        if match:
            value = float(match.group(1))
            if value < 0:
                delta += value
    return delta


def scale_negative_health(candidate: Row, scale: float) -> Row:
    if scale == 1.0:
        return candidate
    adjusted = dict(candidate)
    delta = negative_health_delta(candidate)
    adjusted["score"] = float(candidate["score"]) - delta + scale * delta
    return adjusted


def has_specific_task_match(candidate: Row) -> bool:
    reasons = candidate.get("rank", {}).get("filter_reasons") or candidate.get("filter_reasons") or []
    if "keep:strong_task_key_match" in reasons:
        return True
    return any(
        not key.startswith(("label:", "topic:", "tool:"))
        for key in candidate.get("rank", {}).get("matched_task_keys") or []
    )


def has_positive_health_signal(candidate: Row) -> bool:
    return any(
        ":proven_useful:" in penalty
        for penalty in candidate.get("rank", {}).get("penalties") or []
    )


def active_segment_task_state(candidate: Row) -> bool:
    return "keep:task_state_same_active_segment" in (
        candidate.get("rank", {}).get("filter_reasons") or candidate.get("filter_reasons") or []
    )


def risky_unproven_procedural(candidate: Row, risky_threshold: float) -> bool:
    kind = candidate.get("memory_kind") or "unknown"
    return (
        kind in {"lesson", "workflow"}
        and not has_specific_task_match(candidate)
        and not has_positive_health_signal(candidate)
        and float(candidate["score"]) < risky_threshold
    )


def strict_kind_selection(candidates: list[Row], params: Row, limit: int = 2) -> list[int]:
    if not candidates:
        return []
    top = candidates[0]
    active = [candidate for candidate in candidates if active_segment_task_state(candidate)]
    if float(top["score"]) < params["abstain"] and not active:
        return []
    selected: list[Row] = []
    for candidate in active:
        selected.append(candidate)
        if len(selected) == limit:
            return [int(row["memory_id"]) for row in selected]
    seen_kinds = {row.get("memory_kind") or "unknown" for row in selected}
    selected_ids = {int(row["memory_id"]) for row in selected}
    for index, candidate in enumerate(candidates):
        memory_id = int(candidate["memory_id"])
        if memory_id in selected_ids:
            continue
        kind = candidate.get("memory_kind") or "unknown"
        keep = (
            (index == 0 and not risky_unproven_procedural(candidate, params["risky"]))
            or has_specific_task_match(candidate)
            or (kind not in seen_kinds and has_positive_health_signal(candidate))
        ) and float(candidate["score"]) >= params["second"]
        if keep:
            selected.append(candidate)
            selected_ids.add(memory_id)
            seen_kinds.add(kind)
        if len(selected) == limit:
            break
    return [int(row["memory_id"]) for row in selected]


def variant_selection(recall: Row, overrides: Row, limit: int) -> list[int]:
    params = {**PRODUCTION_PARAMS, **overrides}
    candidates = [
        scale_negative_health(candidate, params["health_neg_scale"])
        for candidate in recall.get("ranking") or []
        if eligible_for_filter_pool(
            candidate, allow_source_overlap=params["allow_source_overlap"]
        )
    ]
    candidates.sort(key=lambda row: (-float(row["score"]), int(row["memory_id"])))
    return strict_kind_selection(candidates, params, limit)


def llm_score_selection(
    recall: Row,
    scores_by_memory: dict[int, int],
    threshold: int,
    limit: int,
) -> tuple[list[int], int, int]:
    eligible = [
        candidate
        for candidate in recall.get("ranking") or []
        if eligible_for_filter_pool(candidate)
    ]
    covered = sum(1 for candidate in eligible if int(candidate["memory_id"]) in scores_by_memory)
    scored = [
        (
            scores_by_memory[int(candidate["memory_id"])],
            index,
            int(candidate["memory_id"]),
        )
        for index, candidate in enumerate(eligible)
        if int(candidate["memory_id"]) in scores_by_memory
        and scores_by_memory[int(candidate["memory_id"])] >= threshold
    ]
    scored.sort(key=lambda item: (-item[0], item[1], item[2]))
    return [memory_id for _s, _i, memory_id in scored[:limit]], covered, len(eligible)


def summarize(label: str, rows: list[Row]) -> Row:
    return {
        "strategy": label,
        "anchors": len(rows),
        "selected_memories": sum(row["selected_count"] for row in rows),
        "average_selected_per_anchor": metric_from_rows("average_selected_per_anchor", rows),
        "average_known_score": metric_from_rows("average_known_score", rows),
        "known_selected_memories": sum(row["known_selected_count"] for row in rows),
        "unknown_selected_memories": sum(row["unknown_selected_count"] for row in rows),
        "useful_known_selected": metric_from_rows("useful_known_selected", rows),
        "low_known_selected": metric_from_rows("low_known_selected", rows),
        "useful_capture_runs": metric_from_rows("useful_capture_runs", rows),
        "low_selection_runs": metric_from_rows("low_selection_runs", rows),
        "oracle_useful_runs": sum(1 for row in rows if row["oracle_has_useful"]),
        "empty_recall_runs": metric_from_rows("empty_recall_runs", rows),
        "missed_useful_empty_runs": metric_from_rows("missed_useful_empty_runs", rows),
        "clean_abstention_runs": metric_from_rows("clean_abstention_runs", rows),
    }


def build_report(manifest: Row) -> str:
    lines = [
        "# Selection/Health-Rerank Tuning on Dense Labels",
        "",
        f"Date: {manifest['date']}",
        f"Input directory: `{manifest['input_dir']}`",
        f"Oracle: `{manifest['oracle_dir']}` (dense adjudicator labels)",
        "",
        "## Experiment",
        "",
        "Replayed saved production candidate rankings through parameterized",
        "strict-kind selection variants (abstention threshold, second-slot",
        "threshold, scaled negative health-rerank deltas) plus a cached",
        "LLM-score arm, scored against the dense oracle. Saved-candidate",
        "replay; no provider calls.",
        "",
        f"Replay fidelity: the default-parameter replay reproduced production",
        f"selections on {manifest['replay_agreement_rate']:.1%} of anchors.",
        "",
        "## Summary",
        "",
        "| Strategy | Avg score | Useful sel | Low sel | Useful runs | Low runs | Avg mem | Empty | Missed-useful empty |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for item in manifest["summaries"]:
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
        "Parameter arms are compared proxy-vs-proxy against the faithful",
        "default-parameter replay so replay drift does not confound the",
        "deltas. `replay_default_vs_production` shows the residual replay",
        "gap against true saved production selections.",
        "",
        "| Strategy | Metric | Delta | 95% CI | Verdict |",
        "| --- | --- | ---: | ---: | --- |",
    ])
    for label, deltas in manifest["deltas"].items():
        for metric in METRICS:
            entry = deltas[metric]
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
    if manifest.get("llm_coverage") is not None:
        lines.extend([
            "",
            "## LLM Arm Coverage",
            "",
            f"- Eligible candidates with a cached LLM score: {manifest['llm_coverage']['covered']}"
            f" of {manifest['llm_coverage']['eligible']}"
            f" ({manifest['llm_coverage']['rate']:.1%}). Scores were cached from"
            " wide-v2 pools; uncovered candidates cannot be selected by this arm.",
        ])
    lines.extend([
        "",
        "## Decision",
        "",
        manifest["decision"],
        "",
        "## Artifacts",
        "",
        "- `manifest.json`: parameters, summaries, paired deltas.",
        "- `<strategy>.details.jsonl`: per-anchor selections per arm.",
        "",
    ])
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anchors-file", type=Path, required=True)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--strategy-label", default="anchor-refresh")
    parser.add_argument("--oracle-dir", type=Path, required=True)
    parser.add_argument("--llm-scores", type=Path)
    parser.add_argument("--llm-threshold", type=int, default=2)
    parser.add_argument("--selection-limit", type=int, default=2)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--date", default=dt.date.today().isoformat())
    args = parser.parse_args()

    anchors = read_anchors(args.anchors_file.expanduser().resolve())
    args.out_dir.mkdir(parents=True, exist_ok=True)

    llm_scores: dict[int, dict[int, int]] = {}
    if args.llm_scores:
        for row in load_jsonl(args.llm_scores.expanduser().resolve()):
            llm_scores.setdefault(int(row["run_id"]), {})[int(row["memory_id"])] = int(row["llm_score"])

    baseline_rows: list[Row] = []
    variant_rows: dict[str, list[Row]] = {name: [] for name in VARIANTS}
    llm_rows: list[Row] = []
    llm_covered = llm_eligible = 0
    replay_matches = 0

    for anchor in anchors:
        run_id = anchor["run_id"]
        recall = load_json(args.input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        oracle = load_json(args.oracle_dir / f"oracle-{run_id}.json")
        production_selected = [int(m) for m in recall.get("selected_memory_ids") or []]
        baseline_rows.append(details_row("production", anchor, oracle, production_selected))
        for name, overrides in VARIANTS.items():
            selected = variant_selection(recall, overrides, args.selection_limit)
            if name == "replay_default" and selected == production_selected:
                replay_matches += 1
            variant_rows[name].append(details_row(name, anchor, oracle, selected))
        if llm_scores:
            selected, covered, eligible = llm_score_selection(
                recall,
                llm_scores.get(run_id, {}),
                args.llm_threshold,
                args.selection_limit,
            )
            llm_covered += covered
            llm_eligible += eligible
            llm_rows.append(details_row(f"llm_t{args.llm_threshold}", anchor, oracle, selected))

    all_arms: dict[str, list[Row]] = dict(variant_rows)
    if llm_rows:
        all_arms[f"llm_t{args.llm_threshold}"] = llm_rows

    for label, rows in [("production", baseline_rows), *all_arms.items()]:
        (args.out_dir / f"{label}.details.jsonl").write_text(
            "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows)
        )

    replay_baseline = all_arms["replay_default"]
    deltas = {
        label: paired_bootstrap_deltas(replay_baseline, rows, METRICS)
        for label, rows in all_arms.items()
        if label != "replay_default"
    }
    deltas["replay_default_vs_production"] = paired_bootstrap_deltas(
        baseline_rows, replay_baseline, METRICS
    )
    summaries = [summarize("production", baseline_rows)] + [
        summarize(label, rows) for label, rows in all_arms.items()
    ]

    promising = []
    for label, delta in deltas.items():
        if label == "replay_default_vs_production":
            continue
        useful_ci = delta["useful_capture_runs"]["ci_95"]
        low_ci = delta["low_selection_runs"]["ci_95"]
        if useful_ci and useful_ci[0] > 0 and (low_ci is None or low_ci[0] <= 0):
            promising.append(label)
    if promising:
        decision = (
            "Promising on screening: "
            + ", ".join(f"`{label}`" for label in promising)
            + " improved useful-capture runs with a CI excluding zero. Confirm the"
            " best arm on the holdout before shipping, and weigh the confirmed"
            " low-selection and context-volume costs."
        )
    else:
        decision = (
            "No arm improved useful-capture runs with a CI excluding zero;"
            " record as no detectable effect and do not ship."
        )

    manifest = {
        "date": args.date,
        "kind": "selection_tuning_dense_labels",
        "input_dir": str(args.input_dir),
        "strategy_label": args.strategy_label,
        "anchors_file": str(args.anchors_file),
        "oracle_dir": str(args.oracle_dir),
        "oracle_kind": "dense_adjudicator",
        "selection_limit": args.selection_limit,
        "variants": {name: {**PRODUCTION_PARAMS, **overrides} for name, overrides in VARIANTS.items()},
        "replay_agreement_rate": replay_matches / len(anchors) if anchors else 0.0,
        "llm_coverage": (
            {
                "covered": llm_covered,
                "eligible": llm_eligible,
                "rate": llm_covered / llm_eligible if llm_eligible else 0.0,
            }
            if llm_rows
            else None
        ),
        "summaries": summaries,
        "deltas": deltas,
        "decision": decision,
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (args.out_dir / "REPORT.md").write_text(build_report(manifest))
    print(json.dumps({"out_dir": str(args.out_dir), "decision": decision,
                      "replay_agreement_rate": manifest["replay_agreement_rate"]}, indent=2))


if __name__ == "__main__":
    main()
