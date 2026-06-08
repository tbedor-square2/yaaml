"""Tests for ClaudeCodeParser and CodexParser using the harness writers."""

from __future__ import annotations

import json
from pathlib import Path

from yaaml.harness import (
    ClaudeCodeSessionWriter,
    CodexSessionWriter,
    FakeToolCall,
    FakeTurn,
)
from yaaml.parsers import ClaudeCodeParser, CodexParser, ParsedTurn, normalize_project_id


def _feed_file(parser: ClaudeCodeParser | CodexParser, path: Path) -> list[ParsedTurn]:
    turns = []
    for line in path.read_text(encoding="utf-8").splitlines():
        result = parser.feed(line)
        if result is not None:
            turns.append(result)
    return turns


def test_normalize_project_id_preserves_filesystem_root():
    assert normalize_project_id("/") == "/"


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


def test_tool_output_is_not_truncated_in_raw_turn(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    long_output = "x" * 2000
    writer.write_turn(
        FakeTurn(
            user="Read the large output",
            assistant="Done",
            tool_calls=[FakeToolCall(name="Read", input={"path": "/large"}, output=long_output)],
        )
    )

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = _feed_file(parser, writer.file_path)

    assert turns[0].tool_calls[0]["output_summary"] == long_output


def test_claude_non_text_assistant_block_is_not_turn_boundary(tmp_claude_dir, project_cwd):
    path = tmp_claude_dir / "session.jsonl"
    records = [
        {
            "type": "user",
            "message": {"role": "user", "content": "Think first"},
            "timestamp": "2026-06-08T00:00:00Z",
            "cwd": project_cwd,
        },
        {
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "thinking", "thinking": "internal"}],
            },
            "timestamp": "2026-06-08T00:00:01Z",
        },
        {
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "Final answer"}],
            },
            "timestamp": "2026-06-08T00:00:02Z",
        },
    ]
    path.write_text("\n".join(json.dumps(record) for record in records) + "\n")

    turns = _feed_file(ClaudeCodeParser(str(path)), path)

    assert len(turns) == 1
    assert turns[0].assistant_content == "Final answer"


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


def test_codex_parser_real_envelope_shape(tmp_path, project_cwd):
    path = tmp_path / "rollout-real.jsonl"
    records = [
        {
            "timestamp": "2026-06-08T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "session-1",
                "timestamp": "2026-06-08T00:00:00Z",
                "cwd": project_cwd,
                "git": {"branch": "feature"},
            },
        },
        {
            "timestamp": "2026-06-08T00:00:01Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Fix the parser"},
        },
        {
            "timestamp": "2026-06-08T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": '{"cmd":"pytest"}',
                "call_id": "call-1",
            },
        },
        {
            "timestamp": "2026-06-08T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "25 passed",
            },
        },
        {
            "timestamp": "2026-06-08T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "*** Begin Patch",
                "call_id": "call-2",
            },
        },
        {
            "timestamp": "2026-06-08T00:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-2",
                "output": "Done!",
            },
        },
        {
            "timestamp": "2026-06-08T00:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Fixed."}],
                "phase": "final_answer",
            },
        },
        {
            "timestamp": "2026-06-08T00:00:07Z",
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": "Fixed."},
        },
    ]
    path.write_text("\n".join(json.dumps(record) for record in records) + "\n")

    parser = CodexParser(str(path))
    turns = _feed_file(parser, path)

    assert parser.session_meta is not None
    assert parser.session_meta.session_id == "session-1"
    assert parser.session_meta.project_id == project_cwd
    assert len(turns) == 1
    assert turns[0].user_content == "Fix the parser"
    assert turns[0].assistant_content == "Fixed."
    assert turns[0].tool_calls[0]["output_summary"] == "25 passed"
    assert turns[0].tool_calls[1]["name"] == "apply_patch"
    assert turns[0].tool_calls[1]["input_summary"] == "*** Begin Patch"
