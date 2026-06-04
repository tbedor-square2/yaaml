"""YAAML command-line interface."""

from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(
    name="yaaml",
    help="YAAML — Yet Another Agent Memory Layer",
    add_completion=False,
)
console = Console()

memories_app = typer.Typer(name="memories", help="Manage stored memories.")
app.add_typer(memories_app, name="memories")

logger = logging.getLogger(__name__)


def _get_config(project_dir: Path | None = None):
    from .config import load_config

    return load_config(project_dir)


def _utcnow() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


# ---------------------------------------------------------------------------
# yaaml daemon
# ---------------------------------------------------------------------------


@app.command()
def daemon() -> None:
    """Start the YAAML background daemon."""
    import asyncio

    from .config import load_config
    from .daemon import YAAMLDaemon

    config = load_config(None)
    d = YAAMLDaemon(config)
    asyncio.run(d.run())


# ---------------------------------------------------------------------------
# yaaml init
# ---------------------------------------------------------------------------


@app.command()
def init() -> None:
    """Initialize YAAML: create config dirs, install skills, show setup instructions."""
    from .config import load_config

    config = load_config(None)

    # Create ~/.yaaml/
    yaaml_dir = Path("~/.yaaml").expanduser()
    yaaml_dir.mkdir(parents=True, exist_ok=True)
    console.print(f"[green]Created[/green] {yaaml_dir}")

    # Initialize DB
    from .db import init_db

    init_db(config.db_path)
    console.print(f"[green]Initialized database[/green] at {config.db_path}")

    # Install skills to ~/.claude/skills/
    skills_dir = Path("~/.claude/skills").expanduser()
    skills_dir.mkdir(parents=True, exist_ok=True)

    # Skills are bundled inside the package under yaaml/skills/
    src_skills = Path(__file__).parent / "skills"
    if src_skills.exists():
        for skill_file in src_skills.glob("*.md"):
            dest = skills_dir / skill_file.name
            dest.write_text(skill_file.read_text(encoding="utf-8"), encoding="utf-8")
            console.print(f"[green]Installed skill[/green] {dest}")
    else:
        console.print("[yellow]Skills directory not found — skipping skill installation.[/yellow]")

    # Print CLAUDE.md snippet
    claude_snippet = (
        "\n## YAAML Memory Layer\n\n"
        "At the start of each session, run: `yaaml recall --project $PWD`\n"
        "Then read the recall file at: `$(yaaml path)`\n\n"
        "This file contains memories from previous sessions relevant to this project.\n"
    )
    console.print("\n[bold]Add this to your CLAUDE.md:[/bold]")
    console.print(claude_snippet)

    # Offer to configure Stop hook
    configure_hook = typer.confirm(
        "Configure a Claude Code Stop hook to trigger recall after each session?",
        default=False,
    )
    if configure_hook:
        _install_stop_hook()

    # Ask about backlog ingestion
    ingest_backlog = typer.confirm(
        "Ingest existing Claude Code / Codex transcripts now?",
        default=False,
    )
    if ingest_backlog:
        import asyncio

        from .daemon import YAAMLDaemon
        from .db import init_db
        from .embeddings import EmbeddingStore
        from .memory import MemoryManager
        from .parsers import ParsedTurn
        from .recall import RecallManager
        from .watcher import CLAUDE_WATCH_PATH, CODEX_WATCH_PATH, FileWatcher

        async def _ingest():
            db = init_db(config.db_path)
            store = EmbeddingStore(config.chroma_path, config.embedding_model)

            async def _noop(turn: ParsedTurn) -> None:
                pass

            watcher = FileWatcher(db, config, _noop)

            for watch_dir in (CLAUDE_WATCH_PATH, CODEX_WATCH_PATH):
                if not watch_dir.exists():
                    continue
                for jsonl in watch_dir.rglob("*.jsonl"):
                    console.print(f"Ingesting {jsonl} …")
                    try:
                        await watcher.process_file(jsonl)
                    except Exception as exc:
                        console.print(f"[red]Error ingesting {jsonl}: {exc}[/red]")

        asyncio.run(_ingest())
        console.print("[green]Backlog ingestion complete.[/green]")

    console.print("\n[bold green]YAAML initialized successfully![/bold green]")


def _install_stop_hook() -> None:
    """Add a yaaml recall Stop hook to ~/.claude/settings.json."""
    settings_path = Path("~/.claude/settings.json").expanduser()
    if settings_path.exists():
        try:
            data = json.loads(settings_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            data = {}
    else:
        data = {}

    hooks = data.setdefault("hooks", {})
    stop_hooks = hooks.setdefault("Stop", [])

    hook_entry = {
        "type": "command",
        "command": "yaaml recall --project $CLAUDE_PROJECT_DIR",
    }

    # Check if already present
    for h in stop_hooks:
        if isinstance(h, dict) and h.get("command", "").startswith("yaaml recall"):
            console.print("[yellow]Stop hook already configured.[/yellow]")
            return

    stop_hooks.append(hook_entry)
    settings_path.parent.mkdir(parents=True, exist_ok=True)
    settings_path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    console.print(f"[green]Stop hook installed in[/green] {settings_path}")


# ---------------------------------------------------------------------------
# yaaml recall
# ---------------------------------------------------------------------------


@app.command()
def recall(
    query: Optional[str] = typer.Option(None, "--query", "-q", help="Explicit recall query text."),
    project: Optional[Path] = typer.Option(
        None, "--project", "-p", help="Project directory.", exists=False
    ),
) -> None:
    """Run recall and write the recall file for a project."""
    from .config import load_config
    from .db import get_db
    from .embeddings import EmbeddingStore
    from .recall import RecallManager

    project_dir = project or Path.cwd()
    config = load_config(project_dir)
    db = get_db(config.db_path)
    store = EmbeddingStore(config.chroma_path, config.embedding_model)
    recall_mgr = RecallManager(db, store, config)

    from .parsers import normalize_project_id

    project_id = normalize_project_id(str(project_dir))
    query_text = query or f"Project: {project_id}"

    memories = recall_mgr.query(query_text, project_id)
    recall_mgr.write_recall_file(memories, project_id, query_source=query or "cli")
    recall_mgr._update_recall_state(project_id, [m["id"] for m in memories])

    recall_file = Path(project_id) / ".yaaml" / "recall.md"
    console.print(f"[green]Recall complete.[/green] {len(memories)} memories written to {recall_file}")


# ---------------------------------------------------------------------------
# yaaml status
# ---------------------------------------------------------------------------


@app.command()
def status() -> None:
    """Show YAAML status: memory count, last creation, last recall, daemon PID."""
    from .config import load_config
    from .daemon import PID_FILE
    from .db import get_db

    config = load_config(None)

    try:
        db = get_db(config.db_path)
        total = db.execute("SELECT COUNT(*) FROM memories WHERE is_active = 1").fetchone()[0]
        last_mem = db.execute(
            "SELECT MAX(created_at) FROM memories WHERE is_active = 1"
        ).fetchone()[0]
        last_recall = db.execute(
            "SELECT MAX(last_recall_at) FROM recall_state"
        ).fetchone()[0]
    except Exception:
        total = 0
        last_mem = None
        last_recall = None

    pid_str = "not running"
    if PID_FILE.exists():
        try:
            pid = int(PID_FILE.read_text().strip())
            # Check if process is alive
            os.kill(pid, 0)
            pid_str = f"running (pid={pid})"
        except (OSError, ValueError):
            pid_str = "stale PID file"

    table = Table(title="YAAML Status")
    table.add_column("Key", style="cyan")
    table.add_column("Value")

    table.add_row("Active memories", str(total))
    table.add_row("Last memory created", last_mem or "—")
    table.add_row("Last recall", last_recall or "—")
    table.add_row("Daemon", pid_str)
    table.add_row("DB path", str(config.db_path))
    table.add_row("Chroma path", str(config.chroma_path))

    console.print(table)


# ---------------------------------------------------------------------------
# yaaml memories list
# ---------------------------------------------------------------------------


@memories_app.command("list")
def memories_list(
    project: Optional[Path] = typer.Option(None, "--project", "-p", help="Filter by project directory."),
    since: Optional[str] = typer.Option(None, "--since", help="ISO date filter (e.g. 2025-01-01)."),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Show memory bodies."),
) -> None:
    """List stored memories in a table."""
    from .config import load_config
    from .db import get_db
    from .parsers import normalize_project_id

    project_dir = project
    config = load_config(project_dir)
    db = get_db(config.db_path)

    query = "SELECT id, title, body, project_id, created_at FROM memories WHERE is_active = 1"
    params: list = []

    if project_dir:
        project_id = normalize_project_id(str(project_dir))
        query += " AND project_id = ?"
        params.append(project_id)

    if since:
        query += " AND created_at >= ?"
        params.append(since)

    query += " ORDER BY created_at DESC"

    rows = db.execute(query, params).fetchall()

    table = Table(title=f"Memories ({len(rows)} total)", show_lines=verbose)
    table.add_column("ID", style="dim", max_width=8)
    table.add_column("Title", style="bold")
    table.add_column("Project", style="cyan", max_width=30)
    table.add_column("Created", style="green")
    if verbose:
        table.add_column("Body")

    for row in rows:
        mem_id, title, body, project_id, created_at = row
        short_id = mem_id[:8]
        short_project = project_id.split("/")[-1] if project_id else "—"
        created_short = created_at[:10] if created_at else "—"

        if verbose:
            table.add_row(short_id, title, short_project, created_short, body[:200] + ("…" if len(body) > 200 else ""))
        else:
            table.add_row(short_id, title, short_project, created_short)

    console.print(table)


# ---------------------------------------------------------------------------
# yaaml path
# ---------------------------------------------------------------------------


@app.command()
def path(
    project: Optional[Path] = typer.Option(None, "--project", "-p", help="Project directory."),
) -> None:
    """Print the recall file path for the current (or specified) project."""
    from .parsers import normalize_project_id

    project_dir = project or Path.cwd()
    project_id = normalize_project_id(str(project_dir))
    recall_file = Path(project_id) / ".yaaml" / "recall.md"
    typer.echo(str(recall_file))


# ---------------------------------------------------------------------------
# yaaml ingest
# ---------------------------------------------------------------------------


@app.command()
def ingest(
    file: Path = typer.Argument(..., help="JSONL transcript file to ingest.", exists=True),
) -> None:
    """Ingest a JSONL transcript file into YAAML."""
    import asyncio

    from .config import load_config
    from .db import get_db
    from .embeddings import EmbeddingStore
    from .memory import MemoryManager
    from .parsers import ParsedTurn
    from .recall import RecallManager
    from .watcher import FileWatcher

    config = load_config(None)
    db = get_db(config.db_path)
    store = EmbeddingStore(config.chroma_path, config.embedding_model)
    memory_mgr = MemoryManager(db, store, config)
    recall_mgr = RecallManager(db, store, config)

    async def on_turn(turn: ParsedTurn) -> None:
        await memory_mgr.on_turn(turn)
        await recall_mgr.on_turn(turn)

    watcher = FileWatcher(db, config, on_turn)

    async def _run():
        await watcher.process_file(file)

    asyncio.run(_run())
    console.print(f"[green]Ingested[/green] {file}")
