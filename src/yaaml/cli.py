"""YAAML command-line interface."""

from __future__ import annotations

import json
import logging
import os
from datetime import UTC, datetime
from pathlib import Path

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


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


# ---------------------------------------------------------------------------
# yaaml daemon
# ---------------------------------------------------------------------------


def _check_api_key() -> None:
    """Warn if neither ANTHROPIC_API_KEY nor OPENAI_API_KEY is set."""
    has_key = os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("OPENAI_API_KEY")
    if not has_key:
        console.print(
            "[yellow]Warning:[/yellow] Neither ANTHROPIC_API_KEY nor OPENAI_API_KEY is set. "
            "Memory creation and consolidation require one of these to be exported."
        )


@app.command()
def daemon() -> None:
    """Start the YAAML background daemon."""
    import asyncio

    from .config import load_config
    from .daemon import YAAMLDaemon

    _check_api_key()
    config = load_config(None)
    d = YAAMLDaemon(config)
    asyncio.run(d.run())


# ---------------------------------------------------------------------------
# yaaml init
# ---------------------------------------------------------------------------


@app.command()
def init() -> None:
    """Initialize YAAML: create config dirs, install skills, show setup instructions."""
    import asyncio
    import platform

    from .config import load_config
    from .db import init_db

    _check_api_key()
    config = load_config(None)

    # Create ~/.yaaml/
    yaaml_dir = Path("~/.yaaml").expanduser()
    yaaml_dir.mkdir(parents=True, exist_ok=True)
    console.print(f"[green]✓[/green] Created {yaaml_dir}")

    # Initialize DB
    init_db(config.db_path)
    console.print(f"[green]✓[/green] Initialized database at {config.db_path}")

    # Install skills to ~/.claude/skills/
    skills_dir = Path("~/.claude/skills").expanduser()
    skills_dir.mkdir(parents=True, exist_ok=True)
    src_skills = Path(__file__).parent / "skills"
    if src_skills.exists():
        for skill_file in src_skills.glob("*.md"):
            dest = skills_dir / skill_file.name
            dest.write_text(skill_file.read_text(encoding="utf-8"), encoding="utf-8")
            console.print(f"[green]✓[/green] Installed skill {dest.name}")
    else:
        console.print("[yellow]Skills directory not found — skipping.[/yellow]")

    # Print CLAUDE.md snippet
    console.print(
        "\n[bold]Add this to your project CLAUDE.md:[/bold]\n"
        "\n## YAAML Memory Layer\n"
        "\nAt the start of each session, run `yaaml recall --project $PWD`"
        " and read `.yaaml/recall.md`.\n"
        "It contains memories from previous sessions relevant to this project.\n"
    )

    # Offer Stop hook
    if typer.confirm("Configure Claude Code Stop hook for turn-boundary detection?", default=True):
        _install_stop_hook()

    # Offer backlog ingestion
    if typer.confirm(
        "Ingest existing Claude Code / Codex transcripts and create memories now?",
        default=True,
    ):
        has_key = bool(os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("OPENAI_API_KEY"))
        if not has_key:
            console.print(
                "[yellow]No LLM API key found — transcripts will be stored but "
                "memories will not be created until ANTHROPIC_API_KEY or "
                "OPENAI_API_KEY is exported.[/yellow]"
            )

        from .embeddings import EmbeddingStore
        from .memory import MemoryManager
        from .parsers import ParsedTurn
        from .watcher import CLAUDE_WATCH_PATH, CODEX_WATCH_PATH, FileWatcher

        async def _ingest() -> None:
            db = init_db(config.db_path)
            store = EmbeddingStore(config.chroma_path, config.embedding_model)
            memory_mgr = MemoryManager(db, store, config)
            turn_count = 0

            async def _on_turn(turn: ParsedTurn) -> None:
                nonlocal turn_count
                turn_count += 1
                await memory_mgr.on_turn(turn)

            watcher = FileWatcher(db, config, _on_turn)
            files_processed = 0

            for watch_dir in (CLAUDE_WATCH_PATH, CODEX_WATCH_PATH):
                if not watch_dir.exists():
                    continue
                for jsonl in sorted(watch_dir.rglob("*.jsonl")):
                    try:
                        await watcher.process_file(jsonl)
                        files_processed += 1
                    except Exception as exc:
                        console.print(f"[red]Error ingesting {jsonl.name}: {exc}[/red]")

            # After all files are read, flush any remaining turns into memories
            if has_key:
                for project_id in memory_mgr.get_projects_with_pending_turns():
                    try:
                        ids = await memory_mgr.create_memories_for_project(project_id)
                        if ids:
                            console.print(
                                f"[dim]  {len(ids)} memory(s) created for "
                                f"{Path(project_id).name}[/dim]"
                            )
                    except Exception as exc:
                        console.print(f"[red]Memory creation failed for {project_id}: {exc}[/red]")

            mem_count = db.execute("SELECT COUNT(*) FROM memories WHERE is_active = 1").fetchone()[
                0
            ]
            console.print(
                f"[green]✓[/green] Ingested {files_processed} file(s), "
                f"{turn_count} turn(s) → {mem_count} memor{'y' if mem_count == 1 else 'ies'}"
            )

        asyncio.run(_ingest())

    # Offer service autostart
    if typer.confirm("Install YAAML daemon as a background service (autostart)?", default=True):
        _install_service(platform.system())

    # Next steps
    console.print("\n[bold green]✓ YAAML initialized.[/bold green]\n")
    console.print("[bold]Next steps:[/bold]")
    console.print("  1. No service installed? Run [cyan]yaaml daemon[/cyan] to start observing")
    console.print("  2. Paste the CLAUDE.md snippet above into your project")
    console.print("  3. Run [cyan]yaaml status[/cyan] to confirm the daemon is running\n")


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
    console.print(f"[green]✓[/green] Stop hook installed in {settings_path}")


def _install_service(system: str) -> None:
    """Install the YAAML daemon as an OS-level autostart service."""
    import shutil

    yaaml_bin = shutil.which("yaaml")
    if not yaaml_bin:
        console.print(
            "[yellow]Could not locate the yaaml binary — skipping service install. "
            "Run `yaaml daemon` manually.[/yellow]"
        )
        return

    if system == "Darwin":
        _install_launchd(yaaml_bin)
    elif system == "Linux":
        _install_systemd(yaaml_bin)
    else:
        console.print(
            f"[yellow]Autostart not supported on {system}. Run `yaaml daemon` manually.[/yellow]"
        )


def _install_launchd(yaaml_bin: str) -> None:
    """Install a launchd user agent on macOS."""
    label = "com.yaaml.daemon"
    plist_dir = Path("~/Library/LaunchAgents").expanduser()
    plist_dir.mkdir(parents=True, exist_ok=True)
    plist_path = plist_dir / f"{label}.plist"

    log_dir = Path("~/.yaaml").expanduser()
    plist_content = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{yaaml_bin}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/daemon-stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/daemon-stderr.log</string>
</dict>
</plist>
"""
    plist_path.write_text(plist_content, encoding="utf-8")
    console.print(f"[green]✓[/green] launchd plist written to {plist_path}")
    console.print(
        f"  Load now with: [cyan]launchctl load {plist_path}[/cyan]\n"
        "  (It will also load automatically at next login.)"
    )


def _install_systemd(yaaml_bin: str) -> None:
    """Install a systemd user service on Linux."""
    service_dir = Path("~/.config/systemd/user").expanduser()
    service_dir.mkdir(parents=True, exist_ok=True)
    service_path = service_dir / "yaaml.service"

    service_content = f"""[Unit]
Description=YAAML memory layer daemon
After=network.target

[Service]
ExecStart={yaaml_bin} daemon
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"""
    service_path.write_text(service_content, encoding="utf-8")
    console.print(f"[green]✓[/green] systemd unit written to {service_path}")
    console.print(
        "  Enable and start with:\n"
        "    [cyan]systemctl --user daemon-reload[/cyan]\n"
        "    [cyan]systemctl --user enable --now yaaml[/cyan]"
    )


# ---------------------------------------------------------------------------
# yaaml recall
# ---------------------------------------------------------------------------


@app.command()
def recall(
    query: str | None = typer.Option(None, "--query", "-q", help="Explicit recall query text."),
    project: Path | None = typer.Option(
        None, "--project", "-p", help="Project directory.", exists=False
    ),
) -> None:
    """Run recall and write the recall file for a project."""
    from .config import load_config
    from .db import get_db
    from .embeddings import EmbeddingStore
    from .parsers import normalize_project_id
    from .recall import RecallManager

    project_dir = project or Path.cwd()
    config = load_config(project_dir)
    db = get_db(config.db_path)
    store = EmbeddingStore(config.chroma_path, config.embedding_model)
    recall_mgr = RecallManager(db, store, config)

    project_id = normalize_project_id(str(project_dir))
    query_text = query or f"Project: {project_id}"

    memories = recall_mgr.query(query_text, project_id)
    recall_mgr.write_recall_file(memories, project_id, query_source=query or "cli")
    recall_mgr.update_recall_state(project_id, [m["id"] for m in memories])

    recall_file = Path(project_id) / ".yaaml" / "recall.md"
    console.print(
        f"[green]Recall complete.[/green] {len(memories)} memories written to {recall_file}"
    )


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
        last_recall = db.execute("SELECT MAX(last_recall_at) FROM recall_state").fetchone()[0]
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
    project: Path | None = typer.Option(
        None, "--project", "-p", help="Filter by project directory."
    ),
    since: str | None = typer.Option(None, "--since", help="ISO date filter (e.g. 2025-01-01)."),
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
    params: list[str] = []

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
            body_preview = body[:200] + ("…" if len(body) > 200 else "")
            table.add_row(short_id, title, short_project, created_short, body_preview)
        else:
            table.add_row(short_id, title, short_project, created_short)

    console.print(table)


# ---------------------------------------------------------------------------
# yaaml path
# ---------------------------------------------------------------------------


@app.command()
def path(
    project: Path | None = typer.Option(None, "--project", "-p", help="Project directory."),
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

    async def _run() -> None:
        await watcher.process_file(file)

    asyncio.run(_run())
    console.print(f"[green]Ingested[/green] {file}")


# ---------------------------------------------------------------------------
# yaaml simulate
# ---------------------------------------------------------------------------


@app.command()
def simulate(
    agent: str = typer.Option("claude-code", "--agent", help="Agent type: claude-code or codex"),
    turns: int = typer.Option(5, "--turns", "-n", help="Number of turns to generate"),
    project: Path | None = typer.Option(None, "--project", "-p", help="Project cwd"),
    delay: float = typer.Option(0.0, "--delay", "-d", help="Seconds between turns"),
    output_dir: Path | None = typer.Option(
        None, "--output-dir", "-o", help="Where to write the session file"
    ),
) -> None:
    """Generate a synthetic agent session for testing YAAML."""
    import time

    from .harness import (
        ClaudeCodeSessionWriter,
        CodexSessionWriter,
        FakeTurn,
        LoremGenerator,
        generate_session,
    )

    project_cwd = str(project or Path.cwd())
    out_dir = output_dir or Path.cwd()
    out_dir.mkdir(parents=True, exist_ok=True)

    lorem = LoremGenerator()

    if agent == "claude-code":
        writer: ClaudeCodeSessionWriter | CodexSessionWriter = ClaudeCodeSessionWriter(
            out_dir, project_cwd
        )
    elif agent == "codex":
        writer = CodexSessionWriter(out_dir, project_cwd)
    else:
        console.print(f"[red]Unknown agent type: {agent}. Use 'claude-code' or 'codex'.[/red]")
        raise typer.Exit(code=1)

    console.print(f"Writing {turns} turns to {writer.file_path}")

    if delay > 0:
        for i in range(turns):
            turn = FakeTurn(
                user=lorem.paragraph(sentences=2),
                assistant=lorem.paragraph(sentences=3),
            )
            writer.write_turn(turn)
            console.print(f"  Turn {i + 1}/{turns} written")
            if i < turns - 1:
                time.sleep(delay)
    else:
        generate_session(writer, n_turns=turns, lorem=lorem)

    console.print(f"[green]Done.[/green] Session file: {writer.file_path}")
