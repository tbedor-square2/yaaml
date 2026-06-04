"""Memory creation and management for YAAML."""

import json
import logging
import sqlite3
import uuid
from collections import defaultdict
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from .config import Config
from .parsers import ParsedTurn

if TYPE_CHECKING:
    from .embeddings import EmbeddingStore

logger = logging.getLogger(__name__)


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


class MemoryManager:
    """Creates and persists memories from observed conversation turns."""

    def __init__(
        self,
        db: sqlite3.Connection,
        store: "EmbeddingStore",
        config: Config,
    ) -> None:
        self._db = db
        self._store = store
        self._config = config
        # Per-project turn counters (reset after each memory creation batch)
        self._turn_counts: dict[str, int] = defaultdict(int)

    async def on_turn(self, turn: ParsedTurn) -> None:
        """Called after each completed turn.

        Increments the project's turn counter; triggers memory creation when the
        configured threshold is reached.

        Args:
            turn: The completed ParsedTurn.
        """
        project_id = turn.project_id
        self._turn_counts[project_id] += 1

        if self._turn_counts[project_id] >= self._config.turns_between_memory:
            self._turn_counts[project_id] = 0
            try:
                await self.create_memories_for_project(project_id)
            except Exception as exc:
                logger.error("create_memories_for_project failed for %s: %s", project_id, exc)

    async def create_memories_for_project(self, project_id: str) -> list[str]:
        """Summarize unprocessed turns into memories and persist them.

        Args:
            project_id: The normalized project directory path.

        Returns:
            List of newly created memory IDs.
        """
        # Import here to avoid circular import at module load time
        from . import llm as llm_module

        turns = self.get_turns_since_last_memory(project_id)
        if not turns:
            logger.debug("No new turns for project %s, skipping memory creation.", project_id)
            return []

        logger.info("Creating memory for project %s from %d turns.", project_id, len(turns))

        try:
            title, body = await llm_module.summarize_turns(turns, None, self._config)
        except Exception as exc:
            logger.error("LLM summarization failed for %s: %s", project_id, exc)
            return []

        now = _utcnow()
        memory_id = str(uuid.uuid4())

        # Collect turn IDs that contributed to this memory
        source_turn_ids = self._get_turn_ids_for_project_since_last_memory(project_id)

        session_id = turns[-1].session_id if turns else None

        self._db.execute(
            """
            INSERT INTO memories
              (id, title, body, source_turn_ids, created_at, updated_at,
               is_active, session_id, project_id, consolidated_into)
            VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, NULL)
            """,
            (
                memory_id,
                title,
                body,
                json.dumps(source_turn_ids),
                now,
                now,
                session_id,
                project_id,
            ),
        )
        self._db.commit()

        # Index in vector store
        try:
            self._store.upsert(
                memory_id,
                f"{title}\n\n{body}",
                {"project_id": project_id, "session_id": session_id or ""},
            )
        except Exception as exc:
            logger.error("Failed to upsert embedding for memory %s: %s", memory_id, exc)

        logger.info("Created memory %s: %s", memory_id, title)
        return [memory_id]

    def get_turns_since_last_memory(self, project_id: str) -> list[ParsedTurn]:
        """Return turns not yet incorporated into any memory for this project.

        Fetches all turns for the project whose observed_at is later than the
        most recently created memory's created_at (or all turns if no memory yet).

        Args:
            project_id: The normalized project directory path.

        Returns:
            List of ParsedTurn objects reconstructed from DB rows.
        """
        # Find the latest memory creation time for this project
        row = self._db.execute(
            "SELECT MAX(created_at) FROM memories WHERE project_id = ? AND is_active = 1",
            (project_id,),
        ).fetchone()
        last_memory_at = row[0] if row and row[0] else None

        if last_memory_at:
            cursor = self._db.execute(
                """
                SELECT t.id, t.session_id, t.role, t.content_json, t.observed_at
                FROM turns t
                JOIN sessions s ON s.id = t.session_id
                WHERE s.project_id = ? AND t.observed_at > ?
                ORDER BY t.observed_at ASC
                """,
                (project_id, last_memory_at),
            )
        else:
            cursor = self._db.execute(
                """
                SELECT t.id, t.session_id, t.role, t.content_json, t.observed_at
                FROM turns t
                JOIN sessions s ON s.id = t.session_id
                WHERE s.project_id = ?
                ORDER BY t.observed_at ASC
                """,
                (project_id,),
            )

        rows = cursor.fetchall()
        return self._rows_to_parsed_turns(rows, project_id)

    def _get_turn_ids_for_project_since_last_memory(self, project_id: str) -> list[str]:
        """Return raw turn IDs since last memory — used for source_turn_ids field."""
        row = self._db.execute(
            "SELECT MAX(created_at) FROM memories WHERE project_id = ? AND is_active = 1",
            (project_id,),
        ).fetchone()
        last_memory_at = row[0] if row and row[0] else None

        if last_memory_at:
            cursor = self._db.execute(
                """
                SELECT t.id FROM turns t
                JOIN sessions s ON s.id = t.session_id
                WHERE s.project_id = ? AND t.observed_at > ?
                ORDER BY t.observed_at ASC
                """,
                (project_id, last_memory_at),
            )
        else:
            cursor = self._db.execute(
                """
                SELECT t.id FROM turns t
                JOIN sessions s ON s.id = t.session_id
                WHERE s.project_id = ?
                ORDER BY t.observed_at ASC
                """,
                (project_id,),
            )
        return [r[0] for r in cursor.fetchall()]

    def _rows_to_parsed_turns(self, rows: Any, project_id: str) -> list[ParsedTurn]:
        """Reconstruct ParsedTurn objects from DB rows grouped by session/timestamp."""
        # Group rows by (session_id, observed_at) to reconstruct turns
        # Each turn has user + assistant + tool rows stored separately
        # We return a simplified ParsedTurn per unique timestamp group

        # Build a map: (session_id, observed_at) -> {role -> content, session_id}
        turn_map: dict[tuple[str, str], dict[str, Any]] = {}
        for row in rows:
            session_id = row[1]
            role = row[2]
            content_json = row[3]
            observed_at = row[4]

            key = (session_id, observed_at)
            if key not in turn_map:
                turn_map[key] = {
                    "session_id": session_id,
                    "observed_at": observed_at,
                    "user_content": "",
                    "assistant_content": "",
                    "tool_calls": [],
                }
            try:
                content = json.loads(content_json)
            except json.JSONDecodeError:
                content = {}

            if role == "user":
                turn_map[key]["user_content"] = content.get("text", str(content))
            elif role == "assistant":
                turn_map[key]["assistant_content"] = content.get("text", str(content))
            elif role == "tool":
                turn_map[key]["tool_calls"].append(content)

        parsed = []
        for (session_id, observed_at), data in sorted(turn_map.items(), key=lambda x: x[0][1]):
            parsed.append(
                ParsedTurn(
                    session_id=session_id,
                    agent_type="claude-code",
                    project_id=project_id,
                    git_branch=None,
                    user_content=data["user_content"],
                    assistant_content=data["assistant_content"],
                    tool_calls=data["tool_calls"],
                    started_at=observed_at,
                    completed_at=observed_at,
                )
            )
        return parsed
