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

    def get_projects_with_pending_turns(self) -> list[str]:
        """Return project IDs that have turns not yet incorporated into any memory."""
        rows = self._db.execute(
            """
            SELECT DISTINCT s.project_id
            FROM turns t
            JOIN sessions s ON s.id = t.session_id
            WHERE t.observed_at > (
                SELECT COALESCE(MAX(m.created_at), '1970-01-01')
                FROM memories m
                WHERE m.project_id = s.project_id AND m.is_active = 1
            )
            """
        ).fetchall()
        return [row[0] for row in rows if row[0]]

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
        from . import llm as llm_module

        turns, source_turn_ids = self._get_pending_turns(project_id)
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
        """Return turns not yet incorporated into any memory for this project."""
        turns, _ = self._get_pending_turns(project_id)
        return turns

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _get_pending_turns(self, project_id: str) -> tuple[list[ParsedTurn], list[str]]:
        """Return (parsed_turns, raw_row_ids) for turns since the last memory.

        Runs a single query so both callers share the same cut-off timestamp.
        """
        last_memory_at = self._get_last_memory_at(project_id)
        rows = self._run_turns_query(project_id, last_memory_at)
        turns = self._rows_to_parsed_turns(rows, project_id)
        ids = [row[0] for row in rows]
        return turns, ids

    def _get_last_memory_at(self, project_id: str) -> str | None:
        row = self._db.execute(
            "SELECT MAX(created_at) FROM memories WHERE project_id = ? AND is_active = 1",
            (project_id,),
        ).fetchone()
        return row[0] if row and row[0] else None

    def _run_turns_query(self, project_id: str, last_memory_at: str | None) -> list[Any]:
        """Fetch turn rows for a project since last_memory_at.

        Returns rows with columns:
          0: t.id  1: t.session_id  2: t.role  3: t.content_json
          4: t.observed_at  5: group_key  6: s.agent_type
        """
        select = """
            SELECT t.id, t.session_id, t.role, t.content_json, t.observed_at,
                   COALESCE(t.turn_group_id, t.observed_at) AS group_key,
                   s.agent_type
            FROM turns t
            JOIN sessions s ON s.id = t.session_id
            WHERE s.project_id = ?
        """
        if last_memory_at:
            return self._db.execute(
                select + " AND t.observed_at > ? ORDER BY t.observed_at ASC",
                (project_id, last_memory_at),
            ).fetchall()
        return self._db.execute(
            select + " ORDER BY t.observed_at ASC",
            (project_id,),
        ).fetchall()

    def _rows_to_parsed_turns(self, rows: list[Any], project_id: str) -> list[ParsedTurn]:
        """Reconstruct ParsedTurn objects from DB rows.

        Groups rows by (session_id, group_key) where group_key is turn_group_id
        when present (V2 schema) or observed_at as a fallback (V1 rows).
        """
        turn_map: dict[tuple[str, str], dict[str, Any]] = {}

        for row in rows:
            turn_id = row[0]
            session_id = row[1]
            role = row[2]
            content_json = row[3]
            observed_at = row[4]
            group_key = row[5]
            agent_type = row[6]

            key = (session_id, group_key)
            if key not in turn_map:
                turn_map[key] = {
                    "session_id": session_id,
                    "observed_at": observed_at,
                    "agent_type": agent_type,
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

            # suppress unused variable warning — turn_id is consumed by callers via rows
            _ = turn_id

        parsed = []
        for data in sorted(turn_map.values(), key=lambda d: d["observed_at"]):
            parsed.append(
                ParsedTurn(
                    session_id=data["session_id"],
                    agent_type=data["agent_type"],
                    project_id=project_id,
                    git_branch=None,
                    user_content=data["user_content"],
                    assistant_content=data["assistant_content"],
                    tool_calls=data["tool_calls"],
                    started_at=data["observed_at"],
                    completed_at=data["observed_at"],
                )
            )
        return parsed
