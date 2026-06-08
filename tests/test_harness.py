"""Tests that harness writers produce valid JSONL."""

from __future__ import annotations

import json
from pathlib import Path

from yaaml.harness import (
    ClaudeCodeSessionWriter,
    CodexSessionWriter,
    LoremGenerator,
    generate_session,
)
from yaaml.parsers import ClaudeCodeParser, CodexParser


def _read_jsonl(path: Path) -> list[dict]:
    lines = path.read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if line.strip()]


def test_claude_code_writer_creates_jsonl(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    assert not writer.file_path.exists()  # nothing written yet

    lorem = LoremGenerator()
    generate_session(writer, n_turns=2, lorem=lorem)

    assert writer.file_path.exists()
    assert writer.file_path.suffix == ".jsonl"
    rows = _read_jsonl(writer.file_path)
    assert len(rows) > 0
    # Every row must be valid JSON with a "type" key
    for row in rows:
        assert "type" in row


def test_codex_writer_creates_jsonl_with_session_meta(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    assert writer.file_path.exists()  # SessionMeta written on init

    rows = _read_jsonl(writer.file_path)
    assert rows[0]["type"] == "session_meta"
    assert rows[0]["payload"]["cwd"] == project_cwd

    lorem = LoremGenerator()
    generate_session(writer, n_turns=1, lorem=lorem)

    rows = _read_jsonl(writer.file_path)
    assert rows[0]["type"] == "session_meta"
    assert len(rows) > 1


def test_generate_session_produces_parseable_claude_code_turns(tmp_claude_dir, project_cwd):
    writer = ClaudeCodeSessionWriter(tmp_claude_dir, project_cwd)
    lorem = LoremGenerator(seed=1)
    n_turns = 3
    generate_session(writer, n_turns=n_turns, lorem=lorem)

    parser = ClaudeCodeParser(str(writer.file_path))
    turns = []
    for line in writer.file_path.read_text(encoding="utf-8").splitlines():
        result = parser.feed(line)
        if result is not None:
            turns.append(result)

    assert len(turns) == n_turns


def test_generate_session_produces_parseable_codex_turns(tmp_codex_dir, project_cwd):
    writer = CodexSessionWriter(tmp_codex_dir, project_cwd)
    lorem = LoremGenerator(seed=2)
    n_turns = 3
    generate_session(writer, n_turns=n_turns, lorem=lorem)

    parser = CodexParser(str(writer.file_path))
    turns = []
    for line in writer.file_path.read_text(encoding="utf-8").splitlines():
        result = parser.feed(line)
        if result is not None:
            turns.append(result)

    assert len(turns) == n_turns


def test_lorem_generator_sentence():
    lorem = LoremGenerator(seed=0)
    s = lorem.sentence()
    assert isinstance(s, str)
    assert s.endswith(".")
    assert s[0].isupper()


def test_lorem_generator_paragraph():
    lorem = LoremGenerator(seed=0)
    p = lorem.paragraph(sentences=3)
    assert isinstance(p, str)
    # 3 sentences → 3 periods
    assert p.count(".") == 3
