"""Tests for ClaudeCodeParser and CodexParser using the harness writers."""

from __future__ import annotations

from pathlib import Path

from yaaml.harness import (
    ClaudeCodeSessionWriter,
    CodexSessionWriter,
    FakeToolCall,
    FakeTurn,
)
from yaaml.parsers import ClaudeCodeParser, CodexParser, ParsedTurn


def _feed_file(parser: ClaudeCodeParser | CodexParser, path: Path) -> list[ParsedTurn]:
    turns = []
    for line in path.read_text(encoding="utf-8").splitlines():
        result = parser.feed(line)
        if result is not None:
            turns.append(result)
    return turns


# ---------------------------------------------------------------------------
# ClaudeCodeParser tests
# ---------------------------------------------------------------------------


def test_claude_code_parser_no_tool_calls(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    turn = FakeTurn(user="Hello world", assistant="Hi there")
    writer.write_turn(turn)

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    assert turns[0].user_content == "Hello world"
    assert turns[0].assistant_content == "Hi there"
    assert turns[0].tool_calls == []


def test_claude_code_parser_with_tool_calls(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    tool_calls = [
        FakeToolCall(name="Read", input={"path": "/foo.py"}, output="file content"),
        FakeToolCall(name="Bash", input={"cmd": "ls"}, output="dir listing"),
    ]
    turn = FakeTurn(user="Do something", assistant="Done", tool_calls=tool_calls)
    writer.write_turn(turn)

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    assert len(turns[0].tool_calls) == 2
    names = [tc["name"] for tc in turns[0].tool_calls]
    assert "Read" in names
    assert "Bash" in names


def test_claude_code_parser_multiple_turns(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    for i in range(3):
        writer.write_turn(FakeTurn(user=f"user {i}", assistant=f"assistant {i}"))

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 3


def test_claude_code_parser_populated_fields(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd, git_branch="feat/test")
    writer.write_turn(FakeTurn(user="Hello", assistant="World"))

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    t = turns[0]
    assert t.agent_type == "claude-code"
    assert t.session_id == writer.file_path.stem
    assert t.git_branch == "feat/test"
    # project_id should be derived from cwd
    assert project_cwd in t.project_id or t.project_id != ""


# ---------------------------------------------------------------------------
# CodexParser tests
# ---------------------------------------------------------------------------


def test_codex_parser_no_tool_calls(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    turn = FakeTurn(user="Hello codex", assistant="Hi from codex")
    writer.write_turn(turn)

    parser = CodexParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    assert turns[0].user_content == "Hello codex"
    assert turns[0].assistant_content == "Hi from codex"
    assert turns[0].tool_calls == []


def test_codex_parser_with_tool_calls(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    tool_calls = [
        FakeToolCall(name="Grep", input={"pattern": "foo"}, output="match found"),
        FakeToolCall(name="Write", input={"path": "/bar.py"}, output="ok"),
    ]
    turn = FakeTurn(user="Search and write", assistant="Done", tool_calls=tool_calls)
    writer.write_turn(turn)

    parser = CodexParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    assert len(turns[0].tool_calls) == 2
    names = [tc["name"] for tc in turns[0].tool_calls]
    assert "Grep" in names
    assert "Write" in names


def test_codex_parser_multiple_turns(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    for i in range(3):
        writer.write_turn(FakeTurn(user=f"user {i}", assistant=f"assistant {i}"))

    parser = CodexParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 3


def test_codex_parser_populated_fields(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd, git_branch="main")
    writer.write_turn(FakeTurn(user="Hello", assistant="World"))

    parser = CodexParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert len(turns) == 1
    t = turns[0]
    assert t.agent_type == "codex"
    assert t.git_branch == "main"
    assert project_cwd in t.project_id or t.project_id != ""


def test_codex_parser_session_meta(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd, git_branch="dev")
    writer.write_turn(FakeTurn(user="Hi", assistant="Hello"))

    parser = CodexParser(str(writer.file_path))
    _feed_file(parser, writer.file_path)

    assert parser.session_meta is not None
    assert parser.session_meta.agent_type == "codex"
    assert parser.session_meta.git_branch == "dev"
