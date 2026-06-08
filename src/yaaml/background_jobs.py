"""Small durable queue helpers for asynchronous LLM work."""

import json
import sqlite3
from datetime import UTC, datetime, timedelta
from typing import Any


def _utcnow() -> datetime:
    return datetime.now(UTC)


def _timestamp(value: datetime | None = None) -> str:
    return (value or _utcnow()).isoformat().replace("+00:00", "Z")


def enqueue_job(
    db: sqlite3.Connection,
    job_id: str,
    task_type: str,
    payload: dict[str, Any],
) -> None:
    """Persist a job unless an equivalent job is already queued."""
    now = _timestamp()
    db.execute(
        """
        INSERT OR IGNORE INTO background_jobs
          (id, task_type, payload_json, attempts, next_attempt_at, last_error, created_at)
        VALUES (?, ?, ?, 0, ?, NULL, ?)
        """,
        (job_id, task_type, json.dumps(payload), now, now),
    )


def due_jobs(
    db: sqlite3.Connection,
    task_type: str,
    limit: int = 10,
) -> list[tuple[str, dict[str, Any]]]:
    """Return due jobs of one type in retry order."""
    rows = db.execute(
        """
        SELECT id, payload_json
        FROM background_jobs
        WHERE task_type = ? AND next_attempt_at <= ?
        ORDER BY next_attempt_at, created_at
        LIMIT ?
        """,
        (task_type, _timestamp(), limit),
    ).fetchall()
    jobs = []
    for row in rows:
        try:
            payload = json.loads(row[1])
        except json.JSONDecodeError:
            payload = {}
        jobs.append((str(row[0]), payload if isinstance(payload, dict) else {}))
    return jobs


def complete_job(db: sqlite3.Connection, job_id: str) -> None:
    """Remove a completed job."""
    db.execute("DELETE FROM background_jobs WHERE id = ?", (job_id,))
    db.commit()


def fail_job(db: sqlite3.Connection, job_id: str, error: Exception) -> None:
    """Record a failure and schedule exponential backoff."""
    row = db.execute(
        "SELECT attempts FROM background_jobs WHERE id = ?",
        (job_id,),
    ).fetchone()
    if row is None:
        return
    attempts = int(row[0]) + 1
    delay_seconds = min(300, 2 ** min(attempts, 8))
    next_attempt = _timestamp(_utcnow() + timedelta(seconds=delay_seconds))
    db.execute(
        """
        UPDATE background_jobs
        SET attempts = ?, next_attempt_at = ?, last_error = ?
        WHERE id = ?
        """,
        (attempts, next_attempt, str(error), job_id),
    )
    db.commit()
