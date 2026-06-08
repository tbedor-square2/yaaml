"""Tests for durable LLM job backoff state."""

from pathlib import Path

from yaaml.background_jobs import complete_job, due_jobs, enqueue_job, fail_job
from yaaml.db import init_db


def test_background_job_failure_is_durable(tmp_path: Path) -> None:
    db = init_db(tmp_path / "test.db")
    enqueue_job(db, "memory:/project", "memory_formulation", {"project_id": "/project"})
    db.commit()

    jobs = due_jobs(db, "memory_formulation")
    assert jobs == [("memory:/project", {"project_id": "/project"})]

    fail_job(db, "memory:/project", RuntimeError("provider unavailable"))
    row = db.execute(
        "SELECT attempts, last_error FROM background_jobs WHERE id = 'memory:/project'"
    ).fetchone()
    assert row[0] == 1
    assert row[1] == "provider unavailable"
    assert due_jobs(db, "memory_formulation") == []

    complete_job(db, "memory:/project")
    assert db.execute("SELECT COUNT(*) FROM background_jobs").fetchone()[0] == 0
