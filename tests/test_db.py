"""Tests for init_db and schema migrations."""

from __future__ import annotations

from pathlib import Path

from yaaml.db import init_db


def test_init_db_creates_schema_version(tmp_path: Path) -> None:
    db_path = tmp_path / "test.db"
    conn = init_db(db_path)

    row = conn.execute("SELECT version FROM schema_version").fetchone()
    assert row is not None
    assert row[0] == 4


def test_init_db_creates_all_tables(tmp_path: Path) -> None:
    db_path = tmp_path / "test.db"
    conn = init_db(db_path)

    tables = {
        r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()
    }

    expected = {
        "schema_version",
        "sessions",
        "turns",
        "memories",
        "file_cursors",
        "recall_state",
        "embedding_jobs",
        "background_jobs",
    }
    assert expected <= tables


def test_init_db_idempotent(tmp_path: Path) -> None:
    db_path = tmp_path / "test.db"
    init_db(db_path)
    conn2 = init_db(db_path)

    # Should still have schema_version = 4
    row = conn2.execute("SELECT version FROM schema_version").fetchone()
    assert row[0] == 4

    # Should still have all expected tables
    tables = {
        r[0] for r in conn2.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()
    }
    expected = {
        "schema_version",
        "sessions",
        "turns",
        "memories",
        "file_cursors",
        "recall_state",
        "embedding_jobs",
        "background_jobs",
    }
    assert expected <= tables
