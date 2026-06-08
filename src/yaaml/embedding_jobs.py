"""Durable retry queue for memory embedding operations."""

import json
import logging
import sqlite3
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .embeddings import EmbeddingStore

logger = logging.getLogger(__name__)


def _utcnow() -> datetime:
    return datetime.now(UTC)


def _timestamp(value: datetime | None = None) -> str:
    return (value or _utcnow()).isoformat().replace("+00:00", "Z")


def enqueue_embedding(
    db: sqlite3.Connection,
    memory_id: str,
    source_memory_ids: list[str] | None = None,
) -> None:
    """Create a durable embedding job if one does not already exist."""
    now = _timestamp()
    db.execute(
        """
        INSERT OR IGNORE INTO embedding_jobs
          (memory_id, source_memory_ids, attempts, next_attempt_at, last_error, created_at)
        VALUES (?, ?, 0, ?, NULL, ?)
        """,
        (memory_id, json.dumps(source_memory_ids or []), now, now),
    )


def process_embedding_job(
    db: sqlite3.Connection,
    store: "EmbeddingStore",
    memory_id: str,
) -> bool:
    """Attempt one job, recording exponential backoff on failure."""
    row = db.execute(
        """
        SELECT j.source_memory_ids, j.attempts,
               m.title, m.body, m.project_id, m.session_id
        FROM embedding_jobs j
        JOIN memories m ON m.id = j.memory_id
        WHERE j.memory_id = ?
        """,
        (memory_id,),
    ).fetchone()
    if row is None:
        return True

    try:
        source_ids = json.loads(row[0])
        if not isinstance(source_ids, list):
            source_ids = []

        store.upsert(
            memory_id,
            f"{row[2]}\n\n{row[3]}",
            {"project_id": row[4], "session_id": row[5] or ""},
        )

        # For consolidation, remove old vectors before switching DB visibility.
        # Re-running after a partial failure is safe because deletes are idempotent.
        for source_id in source_ids:
            store.delete(str(source_id))

        if source_ids:
            placeholders = ",".join("?" for _ in source_ids)
            db.execute(
                f"UPDATE memories SET is_active = 0, consolidated_into = ? "
                f"WHERE id IN ({placeholders})",
                (memory_id, *source_ids),
            )
            db.execute("UPDATE memories SET is_active = 1 WHERE id = ?", (memory_id,))

        db.execute("DELETE FROM embedding_jobs WHERE memory_id = ?", (memory_id,))
        db.commit()
        return True
    except Exception as exc:
        db.rollback()
        attempts = int(row[1]) + 1
        delay_seconds = min(300, 2 ** min(attempts, 8))
        next_attempt = _timestamp(_utcnow() + timedelta(seconds=delay_seconds))
        db.execute(
            """
            UPDATE embedding_jobs
            SET attempts = ?, next_attempt_at = ?, last_error = ?
            WHERE memory_id = ?
            """,
            (attempts, next_attempt, str(exc), memory_id),
        )
        db.commit()
        logger.error(
            "Embedding job for %s failed (attempt %d, retry in %ds): %s",
            memory_id,
            attempts,
            delay_seconds,
            exc,
        )
        return False


def process_due_embedding_jobs(
    db: sqlite3.Connection,
    store: "EmbeddingStore",
    limit: int = 20,
) -> int:
    """Process currently due jobs and return the number attempted."""
    rows = db.execute(
        """
        SELECT memory_id
        FROM embedding_jobs
        WHERE next_attempt_at <= ?
        ORDER BY next_attempt_at, created_at
        LIMIT ?
        """,
        (_timestamp(), limit),
    ).fetchall()
    for row in rows:
        process_embedding_job(db, store, str(row[0]))
    return len(rows)
