"""YAAML daemon — ties together watcher, memory, recall, and consolidation."""

import asyncio
import contextlib
import logging
import logging.handlers
import os
import signal
import sqlite3
from pathlib import Path

from .config import Config
from .consolidation import ConsolidationManager
from .db import init_db
from .embeddings import EmbeddingStore
from .memory import MemoryManager
from .parsers import ParsedTurn
from .recall import RecallManager
from .watcher import CLAUDE_WATCH_PATH, CODEX_WATCH_PATH, FileWatcher

logger = logging.getLogger(__name__)

PID_FILE = Path("~/.yaaml/daemon.pid").expanduser()
LOG_FILE = Path("~/.yaaml/yaaml.log").expanduser()


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

        # Set up graceful shutdown on SIGINT / SIGTERM
        loop = asyncio.get_event_loop()

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

        # Process any backlogs before starting the live watcher
        await self._process_backlog(db, watcher)

        # Start the consolidation dark-period timer initially
        self._reset_consolidation_timer(consolidation_manager)

        try:
            await watcher.start()
        except asyncio.CancelledError:
            logger.info("Watcher cancelled — daemon shutting down.")

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
    ) -> None:
        """Find JSONL files that haven't been processed yet and ingest them."""
        logger.info("Checking for backlog files.")

        paths: list[Path] = []

        for watch_dir in (CLAUDE_WATCH_PATH, CODEX_WATCH_PATH):
            if not watch_dir.exists():
                continue
            for jsonl in watch_dir.rglob("*.jsonl"):
                row = db.execute(
                    "SELECT last_byte_offset FROM file_cursors WHERE file_path = ?",
                    (str(jsonl),),
                ).fetchone()
                # Only process files with no cursor (brand new to the daemon)
                if row is None:
                    paths.append(jsonl)

        if paths:
            logger.info("Found %d backlog files to ingest.", len(paths))
        for path in paths:
            try:
                await watcher.process_file(path)
            except Exception as exc:
                logger.error("Backlog processing failed for %s: %s", path, exc)

    def _cleanup(self) -> None:
        """Remove PID file on exit."""
        try:
            if PID_FILE.exists():
                PID_FILE.unlink()
                logger.info("PID file removed.")
        except OSError as exc:
            logger.warning("Could not remove PID file: %s", exc)
