"""Tests for durable embedding retries."""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

from yaaml.db import init_db
from yaaml.embedding_jobs import (
    enqueue_embedding,
    process_due_embedding_jobs,
    process_embedding_job,
)


class FakeStore:
    def __init__(self, failures: int = 0) -> None:
        self.failures = failures
        self.upserted: list[str] = []
        self.deleted: list[str] = []

    def upsert(self, memory_id: str, text: str, metadata: dict[str, object]) -> None:
        del text, metadata
        if self.failures:
            self.failures -= 1
            raise RuntimeError("embedding unavailable")
        self.upserted.append(memory_id)

    def delete(self, memory_id: str) -> None:
        self.deleted.append(memory_id)


def _insert_memory(db, memory_id: str, *, active: int = 1) -> None:
    now = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    db.execute(
        """
        INSERT INTO memories
          (id, title, body, source_turn_ids, created_at, updated_at,
           is_active, session_id, project_id, consolidated_into)
        VALUES (?, 'Title', 'Body', '[]', ?, ?, ?, NULL, '/project', NULL)
        """,
        (memory_id, now, now, active),
    )


def test_failed_embedding_remains_queued(tmp_path: Path) -> None:
    db = init_db(tmp_path / "test.db")
    _insert_memory(db, "memory-1")
    enqueue_embedding(db, "memory-1")
    db.commit()

    store = FakeStore(failures=1)
    assert not process_embedding_job(db, store, "memory-1")

    job = db.execute(
        "SELECT attempts, last_error FROM embedding_jobs WHERE memory_id = 'memory-1'"
    ).fetchone()
    assert job[0] == 1
    assert "embedding unavailable" in job[1]

    db.execute(
        "UPDATE embedding_jobs SET next_attempt_at = '1970-01-01' WHERE memory_id = 'memory-1'"
    )
    db.commit()
    assert process_due_embedding_jobs(db, store) == 1
    assert store.upserted == ["memory-1"]
    assert db.execute("SELECT COUNT(*) FROM embedding_jobs").fetchone()[0] == 0


def test_consolidation_visibility_switches_after_embedding(tmp_path: Path) -> None:
    db = init_db(tmp_path / "test.db")
    _insert_memory(db, "source-1")
    _insert_memory(db, "source-2")
    _insert_memory(db, "merged", active=0)
    db.execute(
        "UPDATE memories SET consolidated_into = 'merged' WHERE id IN ('source-1', 'source-2')"
    )
    enqueue_embedding(db, "merged", ["source-1", "source-2"])
    db.commit()

    store = FakeStore()
    assert process_embedding_job(db, store, "merged")

    states = {
        row[0]: row[1]
        for row in db.execute("SELECT id, is_active FROM memories ORDER BY id").fetchall()
    }
    assert states == {"merged": 1, "source-1": 0, "source-2": 0}
    assert store.deleted == ["source-1", "source-2"]
