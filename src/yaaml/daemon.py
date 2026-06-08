"""YAAML daemon — ties together watcher, memory, recall, and consolidation."""

import asyncio
import contextlib
import json
import logging
import logging.handlers
import os
import signal
import sqlite3
from pathlib import Path

from .config import Config
from .consolidation import ConsolidationManager
from .db import init_db
from .embedding_jobs import process_due_embedding_jobs
from .embeddings import EmbeddingStore
from .memory import MemoryManager
from .parsers import ParsedTurn
from .recall import RecallManager
from .watcher import CLAUDE_WATCH_PATH, CODEX_WATCH_PATH, FileWatcher

logger = logging.getLogger(__name__)

PID_FILE = Path("~/.yaaml/daemon.pid").expanduser()
LOG_FILE = Path("~/.yaaml/yaaml.log").expanduser()
SOCKET_FILE = Path("~/.yaaml/daemon.sock").expanduser()


def setup_logging() -> None:
    """Configure rotating file handler + stderr for the daemon."""
    LOG_FILE.parent.mkdir(parents=True, exist_ok=True)
    handler = logging.handlers.RotatingFileHandler(
        LOG_FILE,
        maxBytes=10 * 1024 * 1024,  # 10 MB
        backupCount=5,
        encoding="utf-8",
    )
    formatter = logging.Formatter(
        "%(asctime)s %(levelname)-8s %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%SZ",
    )
    handler.setFormatter(formatter)

    root = logging.getLogger()
    root.setLevel(logging.INFO)
    root.addHandler(handler)

    # Also emit to stderr so `yaaml daemon` is usable interactively
    stderr_handler = logging.StreamHandler()
    stderr_handler.setFormatter(formatter)
    root.addHandler(stderr_handler)


class YAAMLDaemon:
    """Long-running background daemon for YAAML."""

    def __init__(self, config: Config) -> None:
        self._config = config
        self._consolidation_task: asyncio.Task[None] | None = None

    async def run(self) -> None:
        """Initialize everything and run forever."""
        setup_logging()

        # Refuse to start if another instance is already running.
        if PID_FILE.exists():
            try:
                pid = int(PID_FILE.read_text().strip())
                os.kill(pid, 0)  # signal 0 = liveness check
                logger.error(
                    "YAAML daemon already running (pid=%d). "
                    "Stop it first or remove %s if it is stale.",
                    pid,
                    PID_FILE,
                )
                return
            except (ValueError, ProcessLookupError):
                pass  # stale PID file — safe to overwrite

        logger.info("YAAML daemon starting (pid=%d).", os.getpid())

        # Write PID file
        PID_FILE.parent.mkdir(parents=True, exist_ok=True)
        PID_FILE.write_text(str(os.getpid()), encoding="utf-8")

        try:
            await self._main()
        finally:
            self._cleanup()

    async def _main(self) -> None:
        config = self._config

        db = init_db(config.db_path)
        store = EmbeddingStore(config.chroma_path, config.embedding_model)
        memory_manager = MemoryManager(db, store, config)
        recall_manager = RecallManager(db, store, config)
        consolidation_manager = ConsolidationManager(db, store, config)

        async def retry_embeddings() -> None:
            while True:
                try:
                    process_due_embedding_jobs(db, store)
                    await memory_manager.process_due_jobs()
                    await consolidation_manager.process_due_jobs()
                except Exception as exc:
                    logger.error("Background retry worker failed: %s", exc)
                await asyncio.sleep(1)

        # Set up graceful shutdown on SIGINT / SIGTERM
        loop = asyncio.get_running_loop()

        def _handle_signal() -> None:
            logger.info("Shutdown signal received.")
            for task in asyncio.all_tasks(loop):
                task.cancel()

        for sig in (signal.SIGINT, signal.SIGTERM):
            with contextlib.suppress(NotImplementedError, RuntimeError):
                loop.add_signal_handler(sig, _handle_signal)

        async def on_turn(turn: ParsedTurn) -> None:
            await memory_manager.on_turn(turn)
            await recall_manager.on_turn(turn)
            self._reset_consolidation_timer(consolidation_manager)

        watcher = FileWatcher(db, config, on_turn)

        async def handle_signal(
            reader: asyncio.StreamReader,
            writer: asyncio.StreamWriter,
        ) -> None:
            try:
                raw = await reader.read(1024 * 1024)
                payload = json.loads(raw) if raw else {}
                transcript_path = payload.get("transcript_path")
                if transcript_path:
                    await watcher.process_file(Path(str(transcript_path)))
                writer.write(b"ok\n")
                await writer.drain()
            except Exception as exc:
                logger.warning("Invalid Stop-hook signal: %s", exc)
                writer.write(b"error\n")
                await writer.drain()
            finally:
                writer.close()
                await writer.wait_closed()

        # Process any backlogs before starting the live watcher
        await self._process_backlog(db, watcher, memory_manager)

        with contextlib.suppress(FileNotFoundError):
            SOCKET_FILE.unlink()
        signal_server = await asyncio.start_unix_server(handle_signal, path=str(SOCKET_FILE))
        embedding_retry_task = asyncio.create_task(retry_embeddings())

        # Start the consolidation dark-period timer initially
        self._reset_consolidation_timer(consolidation_manager)

        try:
            await watcher.start()
        except asyncio.CancelledError:
            logger.info("Watcher cancelled — daemon shutting down.")
        finally:
            signal_server.close()
            await signal_server.wait_closed()
            with contextlib.suppress(FileNotFoundError):
                SOCKET_FILE.unlink()
            embedding_retry_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await embedding_retry_task

    def _reset_consolidation_timer(self, consolidation_manager: ConsolidationManager) -> None:
        """Cancel existing timer and start a new dark-period countdown."""
        if self._consolidation_task and not self._consolidation_task.done():
            self._consolidation_task.cancel()

        async def _fire() -> None:
            try:
                await asyncio.sleep(self._config.consolidation_dark_period_seconds)
                logger.info("Dark period elapsed — running consolidation.")
                await consolidation_manager.run()
            except asyncio.CancelledError:
                pass
            except Exception as exc:
                logger.error("Consolidation run failed: %s", exc)

        self._consolidation_task = asyncio.create_task(_fire())

    async def _process_backlog(
        self,
        db: sqlite3.Connection,
        watcher: FileWatcher,
        memory_manager: MemoryManager,
    ) -> None:
        """Ingest new JSONL lines, then create memories for any pending turns.

        Two-phase approach:
        1. File phase — process files with unread bytes.
        2. Memory phase — for every project that has stored turns not yet in any
           memory (e.g. turns ingested by `yaaml init`), call create_memories_for_project.
        """
        # Phase 1: ingest files the daemon has never seen
        logger.info("Checking for unread backlog files.")
        paths: list[Path] = []
        for watch_dir in (CLAUDE_WATCH_PATH, CODEX_WATCH_PATH):
            if not watch_dir.exists():
                continue
            for jsonl in watch_dir.rglob("*.jsonl"):
                row = db.execute(
                    "SELECT last_byte_offset FROM file_cursors WHERE file_path = ?",
                    (str(jsonl),),
                ).fetchone()
                if row is None or jsonl.stat().st_size > int(row[0]):
                    paths.append(jsonl)

        if paths:
            logger.info("Found %d unread backlog files.", len(paths))
        for path in paths:
            try:
                await watcher.process_file(path)
            except Exception as exc:
                logger.error("Backlog file processing failed for %s: %s", path, exc)

        # Phase 2: create memories for turns already in DB but not yet summarised
        pending = memory_manager.get_projects_with_pending_turns()
        if pending:
            logger.info("Creating memories for %d project(s) with pending turns.", len(pending))
        for project_id in pending:
            try:
                ids = await memory_manager.create_memories_for_project(project_id)
                if ids:
                    logger.info("Created %d memory(s) for %s during startup.", len(ids), project_id)
            except Exception as exc:
                logger.error("Startup memory creation failed for %s: %s", project_id, exc)

    def _cleanup(self) -> None:
        """Remove PID file on exit."""
        try:
            if PID_FILE.exists():
                PID_FILE.unlink()
                logger.info("PID file removed.")
        except OSError as exc:
            logger.warning("Could not remove PID file: %s", exc)
