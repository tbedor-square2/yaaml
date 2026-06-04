"""Shared pytest fixtures for YAAML tests."""

from __future__ import annotations

from datetime import date
from pathlib import Path

import pytest


@pytest.fixture
def tmp_claude_dir(tmp_path: Path) -> Path:
    d = tmp_path / "claude" / "projects" / "test-project"
    d.mkdir(parents=True)
    return d


@pytest.fixture
def tmp_codex_dir(tmp_path: Path) -> Path:
    d = tmp_path / "codex" / "sessions" / str(date.today()).replace("-", "/")
    d.mkdir(parents=True)
    return d


@pytest.fixture
def project_cwd(tmp_path: Path) -> str:
    p = tmp_path / "myproject"
    p.mkdir()
    return str(p)
