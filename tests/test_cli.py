"""CLI behavior tests."""

import json
from datetime import UTC, datetime
from pathlib import Path

from typer.testing import CliRunner

from yaaml.cli import app
from yaaml.db import init_db


def test_transcript_prints_raw_turns(tmp_path: Path, project_cwd: str) -> None:
    db_path = tmp_path / "yaaml.db"
    project = Path(project_cwd)
    config_dir = project / ".yaaml"
    config_dir.mkdir()
    (config_dir / "config.toml").write_text(f'db_path = "{db_path}"\n')

    db = init_db(db_path)
    now = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    db.execute(
        """
        INSERT INTO sessions
          (id, agent_type, project_id, transcript_file_path, started_at, last_seen_at)
        VALUES ('session-1', 'codex', ?, '/tmp/session.jsonl', ?, ?)
        """,
        (project_cwd, now, now),
    )
    db.execute(
        """
        INSERT INTO turns
          (id, session_id, role, content_json, observed_at, turn_group_id)
        VALUES ('turn-1', 'session-1', 'user', ?, ?, 'group-1')
        """,
        (json.dumps({"text": "Keep this exact transcript"}), now),
    )
    db.commit()

    result = CliRunner().invoke(app, ["transcript", "--project", project_cwd])

    assert result.exit_code == 0
    assert "Session `session-1`" in result.stdout
    assert "Keep this exact transcript" in result.stdout


def test_path_uses_project_recall_configuration(project_cwd: str) -> None:
    project = Path(project_cwd)
    config_dir = project / ".yaaml"
    config_dir.mkdir()
    (config_dir / "config.toml").write_text('recall_file_path = ".memory/current.md"\n')

    result = CliRunner().invoke(app, ["path", "--project", project_cwd])

    assert result.exit_code == 0
    assert result.stdout.strip() == str(project / ".memory" / "current.md")
