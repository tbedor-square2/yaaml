"""Tests for FileWatcher.process_file using harness-generated sessions."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

import yaaml.watcher as watcher_mod
from yaaml.config import Config
from yaaml.db import init_db
from yaaml.harness import (
    ClaudeCodeSessionWriter,
    CodexSessionWriter,
    FakeToolCall,
    FakeTurn,
)
from yaaml.parsers import ParsedTurn
from yaaml.watcher import FileWatcher


async def _noop(turn: ParsedTurn) -> None:
    pass


def _make_watcher(db: sqlite3.Connection, claude_path: Path, codex_path: Path) -> FileWatcher:
    # Patch the module-level watch path constants so the watcher identifies
    # files in tmp directories correctly as claude-code or codex.
    watcher_mod.CLAUDE_WATCH_PATH = claude_path
    watcher_mod.CODEX_WATCH_PATH = codex_path

    config = Config()
    return FileWatcher(db, config, _noop)


def _turn_count(db: sqlite3.Connection) -> int:
    return db.execute("SELECT COUNT(*) FROM turns").fetchone()[0]


def _cursor_offset(db: sqlite3.Connection, path: str) -> int:
    row = db.execute(
        "SELECT last_byte_offset FROM file_cursors WHERE file_path = ?", (path,)
    ).fetchone()
    return row[0] if row else 0


# ---------------------------------------------------------------------------
# Claude Code watcher tests
# ---------------------------------------------------------------------------


async def test_process_claude_code_file_inserts_turns(tmp_path, project_cwd, tmp_claude_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello", assistant="World"))
    writer.write_turn(FakeTurn(user="Question", assistant="Answer"))

    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)

    # Each turn emits user + assistant rows → 2 turns × 2 roles = 4 rows
    assert _turn_count(db) >= 2


async def test_process_claude_code_file_with_tool_calls(tmp_path, project_cwd, tmp_claude_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    tool_calls = [
        FakeToolCall(name="Read", input={"path": "/foo"}, output="content"),
    ]
    writer.write_turn(FakeTurn(user="Read file", assistant="Done", tool_calls=tool_calls))

    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)

    # user + assistant + 1 tool = 3 rows minimum
    assert _turn_count(db) >= 2


async def test_cursor_advances_after_processing(tmp_path, project_cwd, tmp_claude_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello", assistant="Hi"))

    file_size = writer.file_path.stat().st_size
    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)

    offset = _cursor_offset(db, str(writer.file_path))
    assert offset == file_size


async def test_reprocessing_same_file_is_noop(tmp_path, project_cwd, tmp_claude_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello", assistant="Hi"))

    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)
    count_after_first = _turn_count(db)

    # Process again — should be a no-op (cursor at end of file)
    await watcher.process_file(writer.file_path)
    count_after_second = _turn_count(db)

    assert count_after_first == count_after_second


# ---------------------------------------------------------------------------
# Codex watcher tests
# ---------------------------------------------------------------------------


async def test_process_codex_file_inserts_turns(tmp_path, project_cwd, tmp_codex_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello codex", assistant="Hi from codex"))
    writer.write_turn(FakeTurn(user="Q2", assistant="A2"))

    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)

    assert _turn_count(db) >= 2


async def test_codex_cursor_advances(tmp_path, project_cwd, tmp_codex_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello", assistant="Hi"))

    file_size = writer.file_path.stat().st_size
    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)

    offset = _cursor_offset(db, str(writer.file_path))
    assert offset == file_size


async def test_codex_reprocessing_is_noop(tmp_path, project_cwd, tmp_codex_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    writer.write_turn(FakeTurn(user="Hello", assistant="Hi"))

    watcher = _make_watcher(db, claude_root, codex_root)
    await watcher.process_file(writer.file_path)
    count_after_first = _turn_count(db)

    await watcher.process_file(writer.file_path)
    count_after_second = _turn_count(db)

    assert count_after_first == count_after_second


async def test_codex_restart_restores_in_progress_turn(tmp_path, project_cwd, tmp_codex_dir):
    db = init_db(tmp_path / "test.db")
    claude_root = tmp_path / "claude" / "projects"
    claude_root.mkdir(parents=True, exist_ok=True)
    codex_root = tmp_path / "codex"
    codex_root.mkdir(parents=True, exist_ok=True)

    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    with writer.file_path.open("a", encoding="utf-8") as fh:
        fh.write(
            json.dumps(
                {
                    "timestamp": "2026-06-08T00:00:01Z",
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": "Interrupted turn"},
                }
            )
            + "\n"
        )

    first_watcher = _make_watcher(db, claude_root, codex_root)
    await first_watcher.process_file(writer.file_path)
    assert _turn_count(db) == 0

    with writer.file_path.open("a", encoding="utf-8") as fh:
        for record in (
            {
                "timestamp": "2026-06-08T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Recovered"}],
                },
            },
            {
                "timestamp": "2026-06-08T00:00:03Z",
                "type": "event_msg",
                "payload": {"type": "task_complete"},
            },
        ):
            fh.write(json.dumps(record) + "\n")

    restarted_watcher = _make_watcher(db, claude_root, codex_root)
    await restarted_watcher.process_file(writer.file_path)

    rows = db.execute("SELECT role, content_json FROM turns ORDER BY rowid").fetchall()
    assert [row[0] for row in rows] == ["user", "assistant"]
    assert json.loads(rows[0][1])["text"] == "Interrupted turn"
    assert json.loads(rows[1][1])["text"] == "Recovered"
