"""Filesystem watcher for Claude Code and Codex JSONL transcript files."""

import asyncio
import json
import logging
import os
import sqlite3
from collections.abc import Awaitable, Callable
from datetime import UTC, datetime
from pathlib import Path

from watchdog.events import FileSystemEvent, FileSystemEventHandler
from watchdog.observers import Observer

from .config import Config
from .parsers import ClaudeCodeParser, CodexParser, ParsedTurn, SessionMeta

logger = logging.getLogger(__name__)


def _watch_path(env_var: str, default: str) -> Path:
    override = os.environ.get(env_var)
    return Path(override).expanduser() if override else Path(default).expanduser()


CLAUDE_WATCH_PATH = _watch_path("YAAML_CLAUDE_WATCH_PATH", "~/.claude/projects/")
CODEX_WATCH_PATH = _watch_path("YAAML_CODEX_WATCH_PATH", "~/.codex/sessions/")


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def _persist_session(db: sqlite3.Connection, meta: SessionMeta) -> None:
    """Insert session into DB if not already present."""
    existing = db.execute("SELECT id FROM sessions WHERE id = ?", (meta.session_id,)).fetchone()
    if existing:
        db.execute(
            "UPDATE sessions SET last_seen_at = ? WHERE id = ?",
            (_utcnow(), meta.session_id),
        )
    else:
        db.execute(
            """
            INSERT INTO sessions
              (id, agent_type, project_id, transcript_file_path, started_at, last_seen_at)
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            (
                meta.session_id,
                meta.agent_type,
                meta.project_id,
                meta.transcript_file_path,
                meta.started_at,
                _utcnow(),
            ),
        )
    db.commit()


def _persist_turn(db: sqlite3.Connection, turn: ParsedTurn) -> None:
    """Insert DB rows for each role in a completed turn."""
    import uuid

    now = _utcnow()

    def _ins(role: str, content_dict: dict[str, object]) -> None:
        db.execute(
            "INSERT OR IGNORE INTO turns (id, session_id, role, content_json, observed_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (str(uuid.uuid4()), turn.session_id, role, json.dumps(content_dict), now),
        )

    if turn.user_content:
        _ins("user", {"text": turn.user_content})
    if turn.assistant_content:
        _ins("assistant", {"text": turn.assistant_content})
    for tc in turn.tool_calls:
        _ins("tool", tc)

    db.commit()


def _load_cursor(db: sqlite3.Connection, file_path: str) -> int:
    """Return last byte offset for a file, defaulting to 0."""
    row = db.execute(
        "SELECT last_byte_offset FROM file_cursors WHERE file_path = ?",
        (file_path,),
    ).fetchone()
    return int(row[0]) if row else 0


def _save_cursor(db: sqlite3.Connection, file_path: str, offset: int) -> None:
    """Persist the byte cursor for a file."""
    now = _utcnow()
    db.execute(
        """
        INSERT INTO file_cursors (file_path, last_byte_offset, last_processed_at)
        VALUES (?, ?, ?)
        ON CONFLICT(file_path) DO UPDATE SET
          last_byte_offset = excluded.last_byte_offset,
          last_processed_at = excluded.last_processed_at
        """,
        (file_path, offset, now),
    )
    db.commit()


class _QueueHandler(FileSystemEventHandler):
    """Watchdog event handler that enqueues events into an asyncio Queue."""

    def __init__(self, queue: asyncio.Queue[FileSystemEvent]) -> None:
        self._queue = queue
        self._loop: asyncio.AbstractEventLoop | None = None

    def set_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        self._loop = loop

    def _enqueue(self, event: FileSystemEvent) -> None:
        if self._loop and not self._loop.is_closed():
            self._loop.call_soon_threadsafe(self._queue.put_nowait, event)

    def on_created(self, event: FileSystemEvent) -> None:
        self._enqueue(event)

    def on_modified(self, event: FileSystemEvent) -> None:
        self._enqueue(event)


class FileWatcher:
    """Watches Claude Code and Codex JSONL directories and feeds turns to a callback."""

    def __init__(
        self,
        db: sqlite3.Connection,
        config: Config,
        on_turn: Callable[[ParsedTurn], Awaitable[None]],
    ) -> None:
        self._db = db
        self._config = config
        self._on_turn = on_turn
        self._queue: asyncio.Queue[FileSystemEvent] = asyncio.Queue()
        # Per-file parser cache
        self._parsers: dict[str, ClaudeCodeParser | CodexParser] = {}

    async def start(self) -> None:
        """Start watching directories and process events forever."""
        loop = asyncio.get_running_loop()
        handler = _QueueHandler(self._queue)
        handler.set_loop(loop)

        observer = Observer()

        for watch_path in (CLAUDE_WATCH_PATH, CODEX_WATCH_PATH):
            if watch_path.exists():
                observer.schedule(handler, str(watch_path), recursive=True)
                logger.info("Watching %s", watch_path)
            else:
                logger.info("Watch path does not exist, skipping: %s", watch_path)

        observer.start()
        logger.info("FileWatcher started.")

        try:
            while True:
                event = await self._queue.get()
                await self._handle_event(event)
        finally:
            observer.stop()
            observer.join()

    async def _handle_event(self, event: FileSystemEvent) -> None:
        """Dispatch a watchdog event."""
        src = getattr(event, "src_path", None)
        if not src:
            return
        path = Path(src)
        if path.suffix != ".jsonl":
            return
        if not path.is_file():
            return
        try:
            await self.process_file(path)
        except Exception as exc:
            logger.error("Error processing %s: %s", path, exc)

    async def process_file(self, file_path: Path) -> None:
        """Read new lines from a JSONL file and feed them to the appropriate parser.

        Args:
            file_path: Path to the JSONL transcript file.
        """
        str_path = str(file_path)

        # Determine agent type from path
        is_codex = (
            CODEX_WATCH_PATH.resolve() in list(file_path.resolve().parents)
            or str(CODEX_WATCH_PATH) in str_path
        )

        # Get or create parser
        if str_path not in self._parsers:
            if is_codex:
                self._parsers[str_path] = CodexParser(str_path)
            else:
                self._parsers[str_path] = ClaudeCodeParser(str_path)

        parser = self._parsers[str_path]
        offset = _load_cursor(self._db, str_path)

        try:
            with open(file_path, "rb") as fh:
                fh.seek(offset)
                new_bytes = fh.read()
        except OSError as exc:
            logger.warning("Cannot read %s: %s", file_path, exc)
            return

        if not new_bytes:
            return

        text = new_bytes.decode("utf-8", errors="replace")
        lines = text.splitlines(keepends=True)

        bytes_processed = 0
        for line in lines:
            line_bytes = line.encode("utf-8", errors="replace")
            try:
                turn = parser.feed(line.rstrip("\n"))
            except Exception as exc:
                logger.warning("Parser error on %s: %s", file_path, exc)
                bytes_processed += len(line_bytes)
                _save_cursor(self._db, str_path, offset + bytes_processed)
                continue

            bytes_processed += len(line_bytes)

            # Persist session meta as soon as it's available
            if parser.session_meta is not None:
                try:
                    _persist_session(self._db, parser.session_meta)
                except Exception as exc:
                    logger.warning("Could not persist session meta: %s", exc)

            if turn is not None:
                try:
                    _persist_turn(self._db, turn)
                    await self._on_turn(turn)
                except Exception as exc:
                    logger.error("on_turn callback failed for %s: %s", file_path, exc)

            _save_cursor(self._db, str_path, offset + bytes_processed)
