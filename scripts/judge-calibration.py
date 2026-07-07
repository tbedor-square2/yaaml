#!/usr/bin/env python3
"""Create and score an adjudicated calibration set for recall judge outputs."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import random
import re
import textwrap
import time
import urllib.error
import urllib.request
from collections import Counter
from pathlib import Path
from typing import Any

Row = dict[str, Any]
SCORE_LABELS = [1, 2, 3, 4, 5]
DEFAULT_ADJUDICATOR_PROVIDER = "anthropic"
DEFAULT_ADJUDICATOR_MODEL = "claude-haiku-4-5-20251001"
DEFAULT_ALIGNMENT_VARIANTS = [
    "production_eval_candidate",
    "query_memory_rubric",
    "strict_recall_decision",
]

RUBRIC_LINES = [
    "5: directly useful and actionable for the current turn.",
    "4: useful context with minor gaps or extra filtering needed.",
    "3: mixed or marginal; some relevance but not clearly worth recall.",
    "2: weak, stale, or mostly irrelevant.",
    "1: distracting, wrong-context, or actively harmful.",
]


def read_anchors(path: Path) -> list[int]:
    run_ids: list[int] = []
    for line in path.read_text().splitlines():
        if line.strip():
            run_ids.append(int(line.split("\t", 1)[0]))
    return run_ids


def load_json(path: Path) -> Row:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError as exc:
        raise SystemExit(f"missing required file: {path}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"failed to parse {path}: {exc}") from exc


def load_jsonl(path: Path) -> list[Row]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def write_jsonl(path: Path, rows: list[Row]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def numeric_score(value: Any) -> int | None:
    try:
        score = int(str(value))
    except (TypeError, ValueError):
        return None
    return score if score in SCORE_LABELS else None


def truncate(text: str, max_chars: int) -> str:
    if len(text) <= max_chars:
        return text
    return text[: max_chars - 15].rstrip() + "\n...[truncated]"


def memory_by_id(recall: Row) -> dict[int, Row]:
    memories = {}
    for memory in recall.get("memories") or []:
        if memory.get("memory_id") is not None:
            memories[int(memory["memory_id"])] = memory
    return memories


def sample(args: argparse.Namespace) -> None:
    anchors_file = args.anchors_file.expanduser().resolve()
    input_dir = args.input_dir.expanduser().resolve()
    run_ids = read_anchors(anchors_file)
    rows: list[Row] = []
    for run_id in run_ids:
        oracle = load_json(input_dir / f"oracle-{run_id}.json")
        recall = load_json(input_dir / f"{args.strategy_label}.recall-{run_id}.json")
        memories = memory_by_id(recall)
        for result in oracle.get("results") or []:
            judge_score = numeric_score(result.get("judge_score"))
            memory_id = result.get("memory_id")
            if judge_score is None or memory_id is None:
                continue
            memory = memories.get(int(memory_id), {})
            rows.append(
                {
                    "run_id": run_id,
                    "session_id": oracle.get("run", {}).get("session_id"),
                    "turn_ordinal": oracle.get("run", {}).get("turn_ordinal"),
                    "memory_id": int(memory_id),
                    "memory_title": result.get("memory_title") or memory.get("title"),
                    "memory_body": memory.get("body"),
                    "query_text": truncate(recall.get("query_text") or "", args.query_chars),
                    "judge_score": judge_score,
                    "judge_rationale": result.get("rationale"),
                    "human_score": None,
                    "human_notes": "",
                    "adjudicator_score": None,
                    "adjudicator_rationale": "",
                    "adjudicator_provider": "",
                    "adjudicator_model": "",
                }
            )
    if len(rows) < args.size:
        raise SystemExit(
            f"only {len(rows)} numeric judged rows available; cannot sample {args.size}"
        )
    rng = random.Random(args.seed)
    rng.shuffle(rows)
    sampled = rows[: args.size]
    sampled.sort(key=lambda row: (row["run_id"], row["memory_id"]))
    args.out_file.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(args.out_file, sampled)
    print(json.dumps({
        "out_file": str(args.out_file),
        "sampled_rows": len(sampled),
        "available_rows": len(rows),
        "seed": args.seed,
    }, indent=2, sort_keys=True))


def write_guide(args: argparse.Namespace) -> None:
    rows = load_jsonl(args.labels_file)
    missing = sum(1 for row in rows if numeric_score(row.get(args.label_field)) is None)
    lines = [
        "# Judge Calibration Adjudication Guide",
        "",
        f"Labels file: `{args.labels_file}`",
        f"Rows: {len(rows)}",
        f"Label field: `{args.label_field}`",
        f"Missing labels: {missing}",
        "",
        "## Rubric",
        "",
        f"Fill `{args.label_field}` in the JSONL file with an integer 1-5.",
        "",
        *(f"- {line}" for line in RUBRIC_LINES),
        "",
        "The primary judge's `judge_score` and `judge_rationale` are retained for scoring only; do not use them when assigning independent adjudicator labels.",
        "",
        "For LLM adjudication, run:",
        "",
        "```bash",
        f"python3 scripts/judge-calibration.py llm-label --labels-file {args.labels_file}",
        "```",
        "",
        "## Scoring Command",
        "",
        "```bash",
        f"python3 scripts/judge-calibration.py score --labels-file {args.labels_file} --label-field {args.label_field} --out-dir experiments/recall/2026-07-judge-calibration",
        "```",
        "",
        f"The scorer refuses to write a calibration report until every row has a valid `{args.label_field}`.",
        "",
        "## Case Index",
        "",
    ]
    for index, row in enumerate(rows, start=1):
        status = "labeled" if numeric_score(row.get(args.label_field)) is not None else "missing"
        title = row.get("memory_title") or "(untitled memory)"
        lines.extend(
            [
                f"### {index}. run {row['run_id']} memory {row['memory_id']} ({status})",
                "",
                f"- Judge score: {row['judge_score']}",
                f"- {args.label_field}: {row.get(args.label_field)}",
                f"- Title: {title}",
                "",
                "Query excerpt:",
                "",
                "```text",
                textwrap.shorten(
                    " ".join((row.get("query_text") or "").split()),
                    width=args.case_chars,
                    placeholder=" ...",
                ),
                "```",
                "",
                "Memory:",
                "",
                "```text",
                textwrap.shorten(
                    " ".join(((row.get("memory_body") or title) or "").split()),
                    width=args.case_chars,
                    placeholder=" ...",
                ),
                "```",
                "",
            ]
        )
    args.out_file.parent.mkdir(parents=True, exist_ok=True)
    args.out_file.write_text("\n".join(lines) + "\n")
    print(json.dumps({
        "labels_file": str(args.labels_file),
        "out_file": str(args.out_file),
        "rows": len(rows),
        f"missing_{args.label_field}": missing,
    }, indent=2, sort_keys=True))


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
    text_parts = [
        block.get("text", "")
        for block in payload.get("content", [])
        if block.get("type") == "text"
    ]
    return parse_json_object("\n".join(text_parts).strip())


def adjudicator_prompt(row: Row) -> str:
    title = row.get("memory_title") or "(untitled memory)"
    query = row.get("query_text") or ""
    memory = row.get("memory_body") or title
    rubric = "\n".join(f"- {line}" for line in RUBRIC_LINES)
    return textwrap.dedent(
        f"""
        Score whether this stored memory would be useful context for answering the current turn.

        Return only JSON with this exact shape:
        {{"score": <integer 1-5>, "rationale": "<one short sentence>"}}

        Rubric:
        {rubric}

        Current turn / recall query:
        ```text
        {query}
        ```

        Stored memory title:
        ```text
        {title}
        ```

        Stored memory body:
        ```text
        {memory}
        ```
        """
    ).strip()


def call_anthropic_adjudicator(
    *,
    row: Row,
    model: str,
    api_key: str,
    timeout_seconds: float,
) -> Row:
    result = call_anthropic_json(
        system=(
            "You are an independent evaluator calibrating a memory recall judge. "
            "Apply the rubric to the query and stored memory only. "
            "Do not infer or use any prior judge score. Return JSON only."
        ),
        prompt=adjudicator_prompt(row),
        model=model,
        api_key=api_key,
        timeout_seconds=timeout_seconds,
    )
    score_value = numeric_score(result.get("score"))
    if score_value is None:
        raise ValueError(f"adjudicator returned invalid score: {result}")
    return {
        "score": score_value,
        "rationale": str(result.get("rationale") or "").strip(),
    }


def llm_label(args: argparse.Namespace) -> None:
    if args.provider != "anthropic":
        raise SystemExit("only --provider anthropic is currently supported")
    api_key = os.environ.get(args.api_key_env)
    if not api_key:
        raise SystemExit(f"{args.api_key_env} is not set")

    rows = load_jsonl(args.labels_file)
    labeled = 0
    skipped = 0
    for index, row in enumerate(rows, start=1):
        if args.resume and numeric_score(row.get("adjudicator_score")) is not None:
            skipped += 1
            continue
        if args.limit is not None and labeled >= args.limit:
            break
        try:
            result = call_anthropic_adjudicator(
                row=row,
                model=args.model,
                api_key=api_key,
                timeout_seconds=args.timeout_seconds,
            )
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ValueError) as exc:
            write_jsonl(args.labels_file, rows)
            raise SystemExit(f"failed to label row {index}: {exc}") from exc

        row["adjudicator_score"] = result["score"]
        row["adjudicator_rationale"] = truncate(result["rationale"], 700)
        row["adjudicator_provider"] = args.provider
        row["adjudicator_model"] = args.model
        row["adjudicator_labeled_at"] = dt.datetime.now(dt.UTC).isoformat()
        labeled += 1
        write_jsonl(args.labels_file, rows)
        if not args.quiet:
            print(json.dumps({
                "row": index,
                "run_id": row.get("run_id"),
                "memory_id": row.get("memory_id"),
                "adjudicator_score": row["adjudicator_score"],
                "labeled": labeled,
                "skipped": skipped,
            }, sort_keys=True))
        if args.sleep_seconds:
            time.sleep(args.sleep_seconds)

    remaining = sum(1 for row in rows if numeric_score(row.get("adjudicator_score")) is None)
    print(json.dumps({
        "labels_file": str(args.labels_file),
        "rows": len(rows),
        "labeled_this_run": labeled,
        "skipped_existing": skipped,
        "remaining_missing_adjudicator_score": remaining,
        "provider": args.provider,
        "model": args.model,
    }, indent=2, sort_keys=True))


def production_eval_candidate_prompt(row: Row) -> tuple[str, str]:
    system = (
        "Rate whether recalled context helped an AI coding agent after it was incorporated into the conversation. "
        "Return only JSON with fields score and rationale. score must be a string from \"1\" to \"5\". "
        "5: recalled context was relevant, concise, and actionable. "
        "4: recalled context was relevant and concise, but not directly actionable. "
        "3: recalled context was partially relevant, but also partially irrelevant or overly long. "
        "2: recalled context had only weak relevance, was stale/misleading/outdated, or required substantial filtering before use. "
        "1: recalled context was not relevant. "
        "For scores 1 or 2, name the main failure mode in the rationale when possible: stale task state, outdated or superseded guidance, wrong context, noisy metadata, too generic, or too long."
    )
    title = row.get("memory_title") or "(untitled memory)"
    prompt = textwrap.dedent(
        f"""
        Replay turn:
        {truncate(row.get("query_text") or "", 4_000)}

        Memory title:
        {truncate(title, 500)}

        Memory body:
        {truncate(row.get("memory_body") or title, 4_000)}

        Retrieval rank: unknown
        Retrieval score: unknown
        Counterfactual citation signal: unknown

        Return JSON.
        """
    ).strip()
    return system, prompt


def query_memory_rubric_prompt(row: Row) -> tuple[str, str]:
    system = (
        "You are scoring memory recall quality for an AI coding agent before context injection. "
        "Decide whether the stored memory would be useful context for the current turn. "
        "Use only the query, memory, and rubric. Return JSON only with fields score and rationale; "
        "score must be a string from \"1\" to \"5\"."
    )
    return system, adjudicator_prompt(row)


def strict_recall_decision_prompt(row: Row) -> tuple[str, str]:
    title = row.get("memory_title") or "(untitled memory)"
    rubric = "\n".join(f"- {line}" for line in RUBRIC_LINES)
    system = (
        "You are the final quality gate for memory recall in an AI coding agent. "
        "Score inclusion value, not topical similarity. Penalize stale task state, wrong workstream, "
        "generic process advice, and memories that require the agent to filter heavily. "
        "Return JSON only with fields score and rationale; score must be a string from \"1\" to \"5\"."
    )
    prompt = textwrap.dedent(
        f"""
        A memory should score 4 or 5 only when it is worth injecting into the agent context for this exact turn.
        Score 1 or 2 when the memory would distract, point at old work, or merely shares broad repo/topic words.

        Rubric:
        {rubric}

        Current turn:
        ```text
        {truncate(row.get("query_text") or "", 4_000)}
        ```

        Stored memory title:
        ```text
        {truncate(title, 500)}
        ```

        Stored memory body:
        ```text
        {truncate(row.get("memory_body") or title, 4_000)}
        ```
        """
    ).strip()
    return system, prompt


PROMPT_VARIANTS = {
    "production_eval_candidate": production_eval_candidate_prompt,
    "query_memory_rubric": query_memory_rubric_prompt,
    "strict_recall_decision": strict_recall_decision_prompt,
}


def agreement_summary_from_pairs(
    *,
    rows: int,
    pairs: list[tuple[int, int]],
    missing_labels: int,
    label_name: str,
    score_name: str,
) -> Row:
    total = len(pairs)
    exact = sum(1 for label, judge in pairs if label == judge)
    within_one = sum(1 for label, judge in pairs if abs(label - judge) <= 1)
    useful_binary = sum(1 for label, judge in pairs if (label >= 4) == (judge >= 4))
    low_binary = sum(1 for label, judge in pairs if (label <= 2) == (judge <= 2))
    signed_error = sum(judge - label for label, judge in pairs)
    absolute_error = sum(abs(judge - label) for label, judge in pairs)
    return {
        "rows": rows,
        "labeled_rows": total,
        "missing_labels": missing_labels,
        "label_field": label_name,
        "score_field": score_name,
        "exact_agreement": exact / total if total else None,
        "within_1_agreement": within_one / total if total else None,
        "useful_binary_agreement": useful_binary / total if total else None,
        "low_binary_agreement": low_binary / total if total else None,
        "cohens_kappa_exact_1_to_5": cohen_kappa(pairs, SCORE_LABELS),
        "cohens_kappa_useful_binary": binary_kappa(pairs, lambda score: score >= 4),
        "cohens_kappa_low_binary": binary_kappa(pairs, lambda score: score <= 2),
        "mean_signed_error": signed_error / total if total else None,
        "mean_absolute_error": absolute_error / total if total else None,
    }


def alignment_summary_for_rows(rows: list[Row], *, label_field: str, score_field: str) -> Row:
    pairs: list[tuple[int, int]] = []
    missing = 0
    for row in rows:
        label = numeric_score(row.get(label_field))
        score_value = numeric_score(row.get(score_field))
        if label is None or score_value is None:
            missing += 1
            continue
        pairs.append((label, score_value))
    return agreement_summary_from_pairs(
        rows=len(rows),
        pairs=pairs,
        missing_labels=missing,
        label_name=label_field,
        score_name=score_field,
    )


def load_alignment_details(path: Path) -> list[Row]:
    if not path.exists():
        return []
    return load_jsonl(path)


def write_alignment_details(path: Path, rows: list[Row]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(path, rows)


def judge_alignment(args: argparse.Namespace) -> None:
    if args.provider != "anthropic":
        raise SystemExit("only --provider anthropic is currently supported")
    api_key = os.environ.get(args.api_key_env)
    if not api_key:
        raise SystemExit(f"{args.api_key_env} is not set")
    variants = [variant.strip() for variant in args.variants.split(",") if variant.strip()]
    unknown = sorted(set(variants) - set(PROMPT_VARIANTS))
    if unknown:
        raise SystemExit(f"unknown prompt variant(s): {', '.join(unknown)}")

    labels = load_jsonl(args.labels_file)
    missing = sum(1 for row in labels if numeric_score(row.get(args.label_field)) is None)
    if missing:
        raise SystemExit(f"{missing} rows are missing {args.label_field}; run llm-label first")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    details_path = args.out_dir / "details.jsonl"
    details = load_alignment_details(details_path) if args.resume else []
    seen = {
        (row.get("variant"), row.get("run_id"), row.get("memory_id"))
        for row in details
        if numeric_score(row.get("candidate_score")) is not None
    }
    completed_this_run = 0
    for variant in variants:
        for index, label_row in enumerate(labels, start=1):
            key = (variant, label_row.get("run_id"), label_row.get("memory_id"))
            if key in seen:
                continue
            if args.limit is not None and completed_this_run >= args.limit:
                break
            system, prompt = PROMPT_VARIANTS[variant](label_row)
            try:
                result = call_anthropic_json(
                    system=system,
                    prompt=prompt,
                    model=args.model,
                    api_key=api_key,
                    timeout_seconds=args.timeout_seconds,
                )
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ValueError) as exc:
                write_alignment_details(details_path, details)
                raise SystemExit(f"failed variant {variant} row {index}: {exc}") from exc
            score_value = numeric_score(result.get("score"))
            if score_value is None:
                write_alignment_details(details_path, details)
                raise SystemExit(f"variant {variant} row {index} returned invalid score: {result}")
            detail = {
                "variant": variant,
                "provider": args.provider,
                "model": args.model,
                "run_id": label_row.get("run_id"),
                "memory_id": label_row.get("memory_id"),
                "adjudicator_score": label_row.get(args.label_field),
                "production_judge_score": label_row.get("judge_score"),
                "candidate_score": score_value,
                "candidate_rationale": truncate(str(result.get("rationale") or ""), 700),
                "labeled_at": dt.datetime.now(dt.UTC).isoformat(),
            }
            details.append(detail)
            seen.add(key)
            completed_this_run += 1
            write_alignment_details(details_path, details)
            if not args.quiet:
                print(json.dumps({
                    "variant": variant,
                    "row": index,
                    "run_id": detail["run_id"],
                    "memory_id": detail["memory_id"],
                    "candidate_score": score_value,
                    "completed_this_run": completed_this_run,
                }, sort_keys=True))
            if args.sleep_seconds:
                time.sleep(args.sleep_seconds)
        if args.limit is not None and completed_this_run >= args.limit:
            break

    summaries: dict[str, Row] = {
        "production_existing": alignment_summary_for_rows(
            labels,
            label_field=args.label_field,
            score_field="judge_score",
        )
    }
    for variant in variants:
        variant_rows = [row for row in details if row.get("variant") == variant]
        summaries[variant] = alignment_summary_for_rows(
            variant_rows,
            label_field="adjudicator_score",
            score_field="candidate_score",
        )

    manifest = {
        "generated_at": dt.date.today().isoformat(),
        "labels_file": str(args.labels_file),
        "details_file": str(details_path),
        "label_field": args.label_field,
        "provider": args.provider,
        "model": args.model,
        "variants": variants,
        "summaries": summaries,
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

    ranked = sorted(
        summaries.items(),
        key=lambda item: (
            item[1].get("useful_binary_agreement") or -1,
            item[1].get("within_1_agreement") or -1,
            item[1].get("exact_agreement") or -1,
        ),
        reverse=True,
    )
    report = [
        "# Judge Prompt Alignment",
        "",
        f"Date: {manifest['generated_at']}",
        f"Labels file: `{manifest['labels_file']}`",
        f"Details file: `{manifest['details_file']}`",
        f"Provider/model: `{args.provider}:{args.model}`",
        "",
        "## Results",
        "",
        "| Variant | Rows | Exact | Within 1 | Useful Binary | Low Binary | Kappa Useful | MAE | Bias |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for name, summary in ranked:
        report.append(
            "| {name} | {rows} | {exact:.3f} | {within:.3f} | {useful:.3f} | {low:.3f} | {kappa:.3f} | {mae:.3f} | {bias:.3f} |".format(
                name=name,
                rows=summary["labeled_rows"],
                exact=summary["exact_agreement"] or 0.0,
                within=summary["within_1_agreement"] or 0.0,
                useful=summary["useful_binary_agreement"] or 0.0,
                low=summary["low_binary_agreement"] or 0.0,
                kappa=summary["cohens_kappa_useful_binary"] or 0.0,
                mae=summary["mean_absolute_error"] or 0.0,
                bias=summary["mean_signed_error"] or 0.0,
            )
        )
    report.extend(
        [
            "",
            "## Prompt Variants",
            "",
            "- `production_existing`: stored production judge scores from the calibration label file.",
            "- `production_eval_candidate`: current offline eval prompt shape replayed against query and memory.",
            "- `query_memory_rubric`: query-plus-memory prompt aligned to the calibration rubric.",
            "- `strict_recall_decision`: stricter inclusion-value prompt that penalizes topical-but-not-actionable recall.",
            "",
            "## Interpretation",
            "",
            f"Best variant by useful-binary, within-1, then exact agreement: `{ranked[0][0]}`.",
            "These scores compare prompt variants against LLM adjudicator labels, not human ground truth.",
            "",
        ]
    )
    (args.out_dir / "REPORT.md").write_text("\n".join(report))
    print(json.dumps(manifest, indent=2, sort_keys=True))


def cohen_kappa(pairs: list[tuple[int, int]], labels: list[int]) -> float | None:
    if not pairs:
        return None
    total = len(pairs)
    observed = sum(1 for human, judge in pairs if human == judge) / total
    human_counts = Counter(human for human, _ in pairs)
    judge_counts = Counter(judge for _, judge in pairs)
    expected = sum(
        (human_counts[label] / total) * (judge_counts[label] / total)
        for label in labels
    )
    if expected == 1.0:
        return 1.0 if observed == 1.0 else None
    return (observed - expected) / (1.0 - expected)


def binary_kappa(
    pairs: list[tuple[int, int]],
    predicate,
) -> float | None:
    binary_pairs = [(1 if predicate(h) else 0, 1 if predicate(j) else 0) for h, j in pairs]
    return cohen_kappa(binary_pairs, [0, 1])


def score(args: argparse.Namespace) -> None:
    rows = load_jsonl(args.labels_file)
    pairs: list[tuple[int, int]] = []
    missing = 0
    for row in rows:
        label = numeric_score(row.get(args.label_field))
        judge = numeric_score(row.get("judge_score"))
        if label is None:
            missing += 1
            continue
        if judge is None:
            raise SystemExit(f"row has invalid judge_score: {row}")
        pairs.append((label, judge))
    if missing and not args.allow_incomplete:
        raise SystemExit(
            f"{missing} rows are missing valid {args.label_field} labels; refusing to write incomplete calibration report"
        )
    total = len(pairs)
    exact = sum(1 for label, judge in pairs if label == judge)
    within_one = sum(1 for label, judge in pairs if abs(label - judge) <= 1)
    useful_binary = sum(1 for label, judge in pairs if (label >= 4) == (judge >= 4))
    low_binary = sum(1 for label, judge in pairs if (label <= 2) == (judge <= 2))
    label_source_counts = Counter(
        f"{row.get('adjudicator_provider') or 'unknown'}:{row.get('adjudicator_model') or 'unknown'}"
        for row in rows
        if numeric_score(row.get(args.label_field)) is not None
    )
    summary = {
        "labels_file": str(args.labels_file),
        "label_field": args.label_field,
        "generated_at": dt.date.today().isoformat(),
        "rows": len(rows),
        "labeled_rows": total,
        "missing_labels": missing,
        "label_source_counts": dict(sorted(label_source_counts.items())),
        "exact_agreement": exact / total if total else None,
        "within_1_agreement": within_one / total if total else None,
        "useful_binary_agreement": useful_binary / total if total else None,
        "low_binary_agreement": low_binary / total if total else None,
        "cohens_kappa_exact_1_to_5": cohen_kappa(pairs, SCORE_LABELS),
        "cohens_kappa_useful_binary": binary_kappa(pairs, lambda score: score >= 4),
        "cohens_kappa_low_binary": binary_kappa(pairs, lambda score: score <= 2),
    }
    if args.out_dir is not None:
        args.out_dir.mkdir(parents=True, exist_ok=True)
        (args.out_dir / "manifest.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
        report = [
            "# Judge Calibration",
            "",
            f"Date: {summary['generated_at']}",
            f"Labels file: `{summary['labels_file']}`",
            "",
            "## Results",
            "",
            f"- Rows: {summary['rows']}",
            f"- Labeled rows: {summary['labeled_rows']}",
            f"- Label field: `{summary['label_field']}`",
            f"- Missing labels: {summary['missing_labels']}",
            f"- Label sources: `{summary['label_source_counts']}`",
            f"- Exact agreement: {summary['exact_agreement']}",
            f"- Within-1 agreement: {summary['within_1_agreement']}",
            f"- Useful binary agreement: {summary['useful_binary_agreement']}",
            f"- Low binary agreement: {summary['low_binary_agreement']}",
            f"- Cohen's kappa, exact 1-5: {summary['cohens_kappa_exact_1_to_5']}",
            f"- Cohen's kappa, useful binary: {summary['cohens_kappa_useful_binary']}",
            f"- Cohen's kappa, low binary: {summary['cohens_kappa_low_binary']}",
            "",
            "## Interpretation",
            "",
            "These metrics compare the production recall judge against an independent adjudicator label set. They validate judge agreement for tuning purposes, not human ground truth.",
            "",
        ]
        (args.out_dir / "REPORT.md").write_text("\n".join(report))
    print(json.dumps(summary, indent=2, sort_keys=True))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    sample_parser = subparsers.add_parser("sample")
    sample_parser.add_argument("--anchors-file", type=Path, required=True)
    sample_parser.add_argument("--input-dir", type=Path, required=True)
    sample_parser.add_argument("--strategy-label", default="anchor-refresh")
    sample_parser.add_argument("--size", type=int, default=100)
    sample_parser.add_argument("--seed", type=int, default=20260706)
    sample_parser.add_argument("--query-chars", type=int, default=4000)
    sample_parser.add_argument("--out-file", type=Path, required=True)
    sample_parser.set_defaults(func=sample)

    score_parser = subparsers.add_parser("score")
    score_parser.add_argument("--labels-file", type=Path, required=True)
    score_parser.add_argument("--label-field", default="adjudicator_score")
    score_parser.add_argument("--out-dir", type=Path)
    score_parser.add_argument("--allow-incomplete", action="store_true",
                              help="print partial metrics even if some labels are missing")
    score_parser.set_defaults(func=score)

    guide_parser = subparsers.add_parser("guide")
    guide_parser.add_argument("--labels-file", type=Path, required=True)
    guide_parser.add_argument("--out-file", type=Path, required=True)
    guide_parser.add_argument("--label-field", default="adjudicator_score")
    guide_parser.add_argument("--case-chars", type=int, default=900)
    guide_parser.set_defaults(func=write_guide)

    llm_parser = subparsers.add_parser("llm-label")
    llm_parser.add_argument("--labels-file", type=Path, required=True)
    llm_parser.add_argument("--provider", default=DEFAULT_ADJUDICATOR_PROVIDER)
    llm_parser.add_argument("--model", default=DEFAULT_ADJUDICATOR_MODEL)
    llm_parser.add_argument("--api-key-env", default="ANTHROPIC_API_KEY")
    llm_parser.add_argument("--limit", type=int)
    llm_parser.add_argument("--resume", action=argparse.BooleanOptionalAction, default=True)
    llm_parser.add_argument("--sleep-seconds", type=float, default=0.0)
    llm_parser.add_argument("--timeout-seconds", type=float, default=60.0)
    llm_parser.add_argument("--quiet", action="store_true")
    llm_parser.set_defaults(func=llm_label)

    alignment_parser = subparsers.add_parser("judge-alignment")
    alignment_parser.add_argument("--labels-file", type=Path, required=True)
    alignment_parser.add_argument("--out-dir", type=Path, required=True)
    alignment_parser.add_argument("--label-field", default="adjudicator_score")
    alignment_parser.add_argument("--provider", default=DEFAULT_ADJUDICATOR_PROVIDER)
    alignment_parser.add_argument("--model", default=DEFAULT_ADJUDICATOR_MODEL)
    alignment_parser.add_argument("--api-key-env", default="ANTHROPIC_API_KEY")
    alignment_parser.add_argument("--variants", default=",".join(DEFAULT_ALIGNMENT_VARIANTS))
    alignment_parser.add_argument("--limit", type=int)
    alignment_parser.add_argument("--resume", action=argparse.BooleanOptionalAction, default=True)
    alignment_parser.add_argument("--sleep-seconds", type=float, default=0.0)
    alignment_parser.add_argument("--timeout-seconds", type=float, default=60.0)
    alignment_parser.add_argument("--quiet", action="store_true")
    alignment_parser.set_defaults(func=judge_alignment)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
