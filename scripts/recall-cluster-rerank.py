#!/usr/bin/env python3
"""Backtest eval-context cluster reranking over saved recall outputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sqlite3
import struct
import urllib.error
import urllib.request
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


Case = dict[str, Any]


@dataclass
class EvalRecord:
    eval_run_id: int
    memory_id: int
    score: int
    vector: tuple[float, ...]


@dataclass
class Cluster:
    polarity: str
    count: int
    centroid: tuple[float, ...]


@dataclass
class ClusterSignal:
    positive_similarity: float | None = None
    positive_count: int = 0
    negative_similarity: float | None = None
    negative_count: int = 0


def decode_f32_blob(blob: bytes) -> tuple[float, ...]:
    if len(blob) % 4 != 0:
        return ()
    return struct.unpack(f"<{len(blob) // 4}f", blob)


def cosine(left: tuple[float, ...], right: tuple[float, ...]) -> float | None:
    if len(left) != len(right) or not left:
        return None
    dot = sum(l * r for l, r in zip(left, right))
    left_norm = math.sqrt(sum(l * l for l in left))
    right_norm = math.sqrt(sum(r * r for r in right))
    if left_norm == 0.0 or right_norm == 0.0:
        return None
    return dot / (left_norm * right_norm)


def centroid(vectors: list[tuple[float, ...]]) -> tuple[float, ...]:
    if not vectors:
        return ()
    dimensions = len(vectors[0])
    totals = [0.0] * dimensions
    for vector in vectors:
        for index, value in enumerate(vector):
            totals[index] += value
    return tuple(value / len(vectors) for value in totals)


def cluster_records(records: list[EvalRecord], threshold: float) -> list[Cluster]:
    clusters: list[tuple[str, list[tuple[float, ...]]]] = []
    for record in records:
        polarity = "positive" if record.score >= 4 else "negative"
        best_index = None
        best_similarity = None
        for index, (cluster_polarity, vectors) in enumerate(clusters):
            if cluster_polarity != polarity:
                continue
            similarity = cosine(record.vector, centroid(vectors))
            if similarity is not None and (best_similarity is None or similarity > best_similarity):
                best_index = index
                best_similarity = similarity
        if best_index is not None and best_similarity is not None and best_similarity >= threshold:
            clusters[best_index][1].append(record.vector)
        else:
            clusters.append((polarity, [record.vector]))
    return [
        Cluster(polarity=polarity, count=len(vectors), centroid=centroid(vectors))
        for polarity, vectors in clusters
    ]


def load_eval_clusters(db_path: Path, model: str, threshold: float) -> dict[int, list[Cluster]]:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=30)
    try:
        rows = conn.execute(
            """
            SELECT eval_run_id, memory_id, judge_score, embedding_blob
            FROM eval_context_embeddings
            WHERE embedding_model = ?
              AND judge_score IN ('1', '2', '4', '5')
            ORDER BY memory_id, eval_run_id
            """,
            (model,),
        ).fetchall()
    finally:
        conn.close()
    records_by_memory: dict[int, list[EvalRecord]] = defaultdict(list)
    for eval_run_id, memory_id, judge_score, blob in rows:
        vector = decode_f32_blob(blob)
        if vector:
            records_by_memory[int(memory_id)].append(
                EvalRecord(
                    eval_run_id=int(eval_run_id),
                    memory_id=int(memory_id),
                    score=int(judge_score),
                    vector=vector,
                )
            )
    return {
        memory_id: cluster_records(records, threshold)
        for memory_id, records in records_by_memory.items()
    }


def strip_query_noise(text: str) -> str:
    lines = []
    suppressed_until = None
    for line in text.splitlines():
        stripped = line.lstrip()
        if suppressed_until is not None:
            if stripped.startswith(suppressed_until):
                suppressed_until = None
            continue
        if stripped.startswith("<codex_internal_context"):
            suppressed_until = "</codex_internal_context>"
            continue
        if stripped.startswith("<environment_context>"):
            suppressed_until = "</environment_context>"
            continue
        if line.strip().startswith(("Working (", "Thinking (")):
            continue
        lines.append(line)
    return "\n".join(lines)


def recall_query(rows: list[tuple[str | None]], max_chars: int, tool_output_chars: int) -> str:
    lines = []
    total = 0
    for (display_text,) in rows:
        if not display_text:
            continue
        for line in strip_query_noise(display_text).splitlines():
            if line.lstrip().startswith("tool output:"):
                line = line[:tool_output_chars]
            lines.append(line)
            total += len(line) + 1
            if total >= max_chars:
                return "\n".join(lines)[:max_chars]
    return "\n".join(lines)


def load_anchor_queries(
    db_path: Path,
    oracle_paths: list[Path],
    window: int,
    max_chars: int,
    tool_output_chars: int,
) -> dict[int, str]:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=30)
    queries = {}
    try:
        for oracle_path in oracle_paths:
            run = json.loads(oracle_path.read_text())["run"]
            run_id = int(run["id"])
            session_id = str(run["session_id"])
            turn_ordinal = int(run["turn_ordinal"])
            start = max(0, turn_ordinal + 1 - max(1, window))
            rows = conn.execute(
                """
                SELECT display_text
                FROM turns
                WHERE session_id = ?
                  AND status = 'completed'
                  AND ordinal >= ?
                  AND ordinal < ?
                ORDER BY ordinal
                """,
                (session_id, start, turn_ordinal + 1),
            ).fetchall()
            query = recall_query(rows, max_chars, tool_output_chars)
            if query.strip():
                queries[run_id] = query
    finally:
        conn.close()
    return queries


def text_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def openai_embeddings(texts: list[str], model: str) -> list[tuple[float, ...]]:
    api_key = os.environ.get("OPENAI_API_KEY")
    if not api_key:
        raise RuntimeError("OPENAI_API_KEY is not set")
    request = urllib.request.Request(
        "https://api.openai.com/v1/embeddings",
        data=json.dumps({"model": model, "input": texts}).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"OpenAI embeddings request failed: {error.code} {body}") from error
    data = sorted(payload["data"], key=lambda item: int(item["index"]))
    return [tuple(float(value) for value in item["embedding"]) for item in data]


def query_embeddings(
    queries: dict[int, str],
    model: str,
    cache_path: Path,
    batch_size: int,
) -> dict[int, tuple[float, ...]]:
    cache = json.loads(cache_path.read_text()) if cache_path.exists() else {}
    if cache.get("model") not in (None, model):
        cache = {}
    cache["model"] = model
    entries = cache.setdefault("entries", {})
    vectors = {}
    missing = []
    for run_id, query in queries.items():
        digest = text_hash(query)
        cached = entries.get(str(run_id))
        if cached and cached.get("text_hash") == digest and cached.get("vector"):
            vectors[run_id] = tuple(float(value) for value in cached["vector"])
        else:
            missing.append((run_id, query, digest))
    for offset in range(0, len(missing), batch_size):
        batch = missing[offset : offset + batch_size]
        fetched = openai_embeddings([query for _, query, _ in batch], model)
        for (run_id, query, digest), vector in zip(batch, fetched):
            entries[str(run_id)] = {
                "text_hash": digest,
                "chars": len(query),
                "vector": list(vector),
            }
            vectors[run_id] = vector
        cache_path.parent.mkdir(parents=True, exist_ok=True)
        cache_path.write_text(json.dumps(cache, indent=2, sort_keys=True) + "\n")
    return vectors


def cluster_signal(
    memory_id: int,
    query_vector: tuple[float, ...] | None,
    clusters_by_memory: dict[int, list[Cluster]],
) -> ClusterSignal:
    if query_vector is None:
        return ClusterSignal()
    signal = ClusterSignal()
    for cluster in clusters_by_memory.get(memory_id, []):
        similarity = cosine(query_vector, cluster.centroid)
        if similarity is None:
            continue
        if cluster.polarity == "positive" and (
            signal.positive_similarity is None or similarity > signal.positive_similarity
        ):
            signal.positive_similarity = similarity
            signal.positive_count = cluster.count
        elif cluster.polarity == "negative" and (
            signal.negative_similarity is None or similarity > signal.negative_similarity
        ):
            signal.negative_similarity = similarity
            signal.negative_count = cluster.count
    return signal


def signal_adjustment(signal: ClusterSignal, policy: str) -> float:
    pos = signal.positive_similarity or 0.0
    neg = signal.negative_similarity or 0.0
    margin = pos - neg
    if policy == "cluster_boost":
        return 0.16 if pos >= 0.82 and margin >= 0.02 else 0.0
    if policy == "cluster_demote":
        return -0.18 if neg >= 0.82 and margin <= -0.02 else 0.0
    if policy == "cluster_margin":
        return max(-0.20, min(0.16, margin * 0.35)) if max(pos, neg) >= 0.78 else 0.0
    if policy == "cluster_gate":
        if neg >= 0.84 and margin <= -0.04:
            return -0.24
        if pos >= 0.84 and margin >= 0.04:
            return 0.14
        return 0.0
    if policy == "cluster_count_weighted":
        pos_weight = min(3, signal.positive_count) * max(0.0, pos - 0.78)
        neg_weight = min(3, signal.negative_count) * max(0.0, neg - 0.78)
        return max(-0.22, min(0.18, (pos_weight - neg_weight) * 0.45))
    return 0.0


def memory_id(candidate: Case) -> int:
    return int(candidate["memory_id"])


def score(candidate: Case) -> float:
    return float(candidate.get("score") or 0.0)


def kind(candidate: Case) -> str:
    return str(candidate.get("memory_kind") or "unknown")


def strong_task(candidate: Case) -> bool:
    reasons = set(candidate.get("filter_reasons") or [])
    return "keep:strong_task_key_match" in reasons or bool(
        candidate.get("rank", {}).get("matched_task_keys") or []
    )


def strict_kind_select(candidates: list[Case], limit: int = 3) -> list[int]:
    if not candidates or score(candidates[0]) < 0.75:
        return []
    selected = []
    seen_kinds = set()
    for index, candidate in enumerate(candidates):
        keep = index == 0 or strong_task(candidate) or kind(candidate) not in seen_kinds
        keep = keep and score(candidate) >= 0.90
        if keep:
            selected.append(memory_id(candidate))
            seen_kinds.add(kind(candidate))
        if len(selected) == limit:
            break
    if not selected and score(candidates[0]) >= 0.75:
        return [memory_id(candidates[0])]
    return selected


def selected_from_recall(candidates: list[Case]) -> list[int]:
    return [memory_id(candidate) for candidate in candidates if candidate.get("selected")]


def adjusted_candidates(
    candidates: list[Case],
    signals: dict[int, ClusterSignal],
    policy: str,
) -> list[Case]:
    adjusted = []
    for candidate in candidates:
        clone = dict(candidate)
        clone["score"] = score(candidate) + signal_adjustment(signals[memory_id(candidate)], policy)
        adjusted.append(clone)
    adjusted.sort(key=lambda item: (-score(item), memory_id(item)))
    return adjusted


def load_scores(oracle_path: Path) -> dict[str, int]:
    oracle = json.loads(oracle_path.read_text())
    scores = {}
    for result in oracle.get("results", []):
        judge_score = str(result.get("judge_score"))
        if judge_score in {"1", "2", "3", "4", "5"}:
            scores[str(result.get("memory_id"))] = int(judge_score)
    return scores


def average(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def unique(values: list[int]) -> list[int]:
    seen = set()
    out = []
    for value in values:
        if value not in seen:
            out.append(value)
            seen.add(value)
    return out


def case_metrics(run_id: int, selected_ids: list[int], scores: dict[str, int], signal_count: int) -> Case:
    selected_ids = unique(selected_ids)[:3]
    known_scores = [scores[str(memory_id)] for memory_id in selected_ids if str(memory_id) in scores]
    return {
        "run_id": run_id,
        "selected_memory_ids": selected_ids,
        "selected_count": len(selected_ids),
        "known_selected_count": len(known_scores),
        "unknown_selected_count": len(selected_ids) - len(known_scores),
        "average_known_score": average([float(value) for value in known_scores]),
        "useful_known_selected": sum(1 for value in known_scores if value >= 4),
        "low_known_selected": sum(1 for value in known_scores if value <= 2),
        "captured_any_known_useful": any(value >= 4 for value in known_scores),
        "selected_any_known_low": any(value <= 2 for value in known_scores),
        "oracle_has_useful": any(value >= 4 for value in scores.values()),
        "empty_recall": len(selected_ids) == 0,
        "candidate_signal_count": signal_count,
    }


def summarize(strategy: str, cases: list[Case]) -> Case:
    selected = sum(case["selected_count"] for case in cases)
    known = sum(case["known_selected_count"] for case in cases)
    useful = sum(case["useful_known_selected"] for case in cases)
    low = sum(case["low_known_selected"] for case in cases)
    known_averages = [case["average_known_score"] for case in cases if case["average_known_score"] is not None]
    return {
        "strategy": strategy,
        "anchors": len(cases),
        "selected_memories": selected,
        "average_selected_per_anchor": selected / len(cases),
        "known_selected_memories": known,
        "unknown_selected_memories": selected - known,
        "average_known_score": average(known_averages),
        "useful_known_selected": useful,
        "low_known_selected": low,
        "useful_capture_runs": sum(1 for case in cases if case["captured_any_known_useful"]),
        "low_selection_runs": sum(1 for case in cases if case["selected_any_known_low"]),
        "oracle_useful_runs": sum(1 for case in cases if case["oracle_has_useful"]),
        "empty_recall_runs": sum(1 for case in cases if case["empty_recall"]),
        "runs_with_candidate_signals": sum(1 for case in cases if case["candidate_signal_count"] > 0),
        "average_candidate_signals": average([float(case["candidate_signal_count"]) for case in cases]),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--label", default="strict-kind-production")
    parser.add_argument("--db", type=Path, default=Path.home() / ".yaaml" / "yaaml.db")
    parser.add_argument("--embedding-model", default="text-embedding-3-small")
    parser.add_argument("--cluster-threshold", type=float, default=0.84)
    parser.add_argument("--query-cache", type=Path)
    parser.add_argument("--embedding-batch-size", type=int, default=32)
    parser.add_argument("--recall-live-turn-window", type=int, default=3)
    parser.add_argument("--recall-query-max-chars", type=int, default=12_000)
    parser.add_argument("--tool-output-truncation-chars", type=int, default=500)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    recall_paths = sorted(args.input_dir.glob(f"{args.label}.recall-*.json"))
    oracle_paths = [args.input_dir / f"oracle-{int(path.stem.split('-')[-1])}.json" for path in recall_paths]
    queries = load_anchor_queries(
        args.db,
        oracle_paths,
        args.recall_live_turn_window,
        args.recall_query_max_chars,
        args.tool_output_truncation_chars,
    )
    cache_path = args.query_cache or args.out_dir / f"{args.embedding_model}.query-embeddings.json"
    query_by_run = query_embeddings(
        queries,
        args.embedding_model,
        cache_path,
        max(1, args.embedding_batch_size),
    )
    clusters_by_memory = load_eval_clusters(args.db, args.embedding_model, args.cluster_threshold)
    policies = [
        "baseline",
        "cluster_boost",
        "cluster_demote",
        "cluster_margin",
        "cluster_gate",
        "cluster_count_weighted",
    ]
    summaries = []
    for policy in policies:
        cases = []
        for recall_path in recall_paths:
            run_id = int(recall_path.stem.split("-")[-1])
            recall = json.loads(recall_path.read_text())
            candidates = recall.get("ranking") or []
            signals = {
                memory_id(candidate): cluster_signal(
                    memory_id(candidate),
                    query_by_run.get(run_id),
                    clusters_by_memory,
                )
                for candidate in candidates
            }
            active_signal_count = sum(
                1
                for signal in signals.values()
                if signal.positive_similarity is not None or signal.negative_similarity is not None
            )
            selected_ids = (
                selected_from_recall(candidates)
                if policy == "baseline"
                else strict_kind_select(adjusted_candidates(candidates, signals, policy))
            )
            scores = load_scores(args.input_dir / f"oracle-{run_id}.json")
            cases.append(case_metrics(run_id, selected_ids, scores, active_signal_count))
        summary = summarize(policy, cases)
        (args.out_dir / f"{policy}.summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        with (args.out_dir / f"{policy}.details.jsonl").open("w") as details:
            for case in cases:
                details.write(json.dumps(case) + "\n")
        summaries.append(summary)

    (args.out_dir / "cluster-rerank.summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
    print(json.dumps(summaries, indent=2))


if __name__ == "__main__":
    main()
