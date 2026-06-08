"""Memory recall and recall-file writing for YAAML."""

import json
import logging
import sqlite3
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .config import Config
from .parsers import ParsedTurn

if TYPE_CHECKING:
    from .embeddings import EmbeddingStore

logger = logging.getLogger(__name__)


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


class RecallManager:
    """Queries memories and materializes them into a per-project recall file."""

    def __init__(
        self,
        db: sqlite3.Connection,
        store: "EmbeddingStore",
        config: Config,
    ) -> None:
        self._db = db
        self._store = store
        self._config = config
        # Track sessions we've already seen to skip first-turn recall
        self._seen_sessions: set[str] = set()

    async def on_turn(self, turn: ParsedTurn) -> None:
        """Called after each completed turn.

        Skips the very first logical turn of a brand-new session (no prior
        context to query against). Uses the DB to detect "first turn" so the
        check survives daemon restarts — if prior turns already exist for the
        session, recall runs normally.

        Args:
            turn: The just-completed ParsedTurn.
        """
        session_id = turn.session_id
        if session_id not in self._seen_sessions:
            self._seen_sessions.add(session_id)
            # Count distinct logical turns already stored for this session.
            # After _persist_turn the current turn is already in the DB, so
            # a count of 1 means this is genuinely the first turn.
            n_groups = self._db.execute(
                "SELECT COUNT(DISTINCT turn_group_id) FROM turns WHERE session_id = ?",
                (session_id,),
            ).fetchone()[0]
            if n_groups <= 1:
                return

        context_text = self._build_context(turn)
        if self._config.recall_classifier_enabled and not self._is_meaningful_context(context_text):
            return
        memories = self.query(context_text, turn.project_id)

        if not memories:
            return

        current_ids = [m["id"] for m in memories]
        if not self.recall_changed(turn.project_id, current_ids):
            return

        self.write_recall_file(memories, turn.project_id, query_source="turn-context")
        self.update_recall_state(turn.project_id, current_ids)

    def _build_context(self, turn: ParsedTurn) -> str:
        """Build a query context string from the current turn and up to 2 prior turns."""
        parts = []

        # Fetch the most recent rows for this project; the current turn is already
        # persisted to DB before this method is called, so no manual append needed.
        rows = self._db.execute(
            """
            SELECT t.role, t.content_json
            FROM turns t
            JOIN sessions s ON s.id = t.session_id
            WHERE s.project_id = ?
            ORDER BY t.observed_at DESC
            LIMIT 6
            """,
            (turn.project_id,),
        ).fetchall()

        for row in reversed(rows):
            role = row[0]
            try:
                content = json.loads(row[1])
                text = (
                    content.get("text", str(content)) if isinstance(content, dict) else str(content)
                )
            except (json.JSONDecodeError, TypeError):
                text = str(row[1])
            if text:
                parts.append(f"{role}: {text[:500]}")

        return "\n".join(parts)

    @staticmethod
    def _is_meaningful_context(context_text: str) -> bool:
        """Cheap first-stage recall gate for empty or trivial exchanges."""
        words = [word for word in context_text.split() if any(char.isalnum() for char in word)]
        return len(words) >= 6

    def query(self, context_text: str, project_id: str) -> list[dict[str, Any]]:
        """Run vector search with project boost and deduplication.

        Args:
            context_text: Text to embed as the query.
            project_id: Current project — memories from this project get a
                        distance boost (lower effective distance).

        Returns:
            List of memory dicts sorted by boosted distance ascending.
        """
        candidates = self._store.search(
            query_text=context_text,
            n_results=self._config.recall_candidate_pool,
        )

        # Apply project boost: divide distance by boost factor for same project
        boosted: list[dict[str, Any]] = []
        for result in candidates:
            meta = result.get("metadata", {})
            dist = result["distance"]
            if meta.get("project_id") == project_id:
                dist = dist / self._config.recall_project_boost
            boosted.append({**result, "boosted_distance": dist})

        # Sort by boosted distance, apply threshold
        boosted.sort(key=lambda x: x["boosted_distance"])
        filtered = [
            r for r in boosted if r["boosted_distance"] <= self._config.recall_distance_threshold
        ]

        # Cap at result limit
        filtered = filtered[: self._config.recall_result_limit]

        # Deduplicate by ID (shouldn't be needed but be safe)
        seen: set[str] = set()
        deduped = []
        for r in filtered:
            if r["id"] not in seen:
                seen.add(r["id"])
                deduped.append(r)

        # Fetch full memory details from DB
        result_memories = []
        for r in deduped:
            mem = self._fetch_memory(r["id"])
            if mem and mem["is_active"]:
                result_memories.append(mem)

        return result_memories

    def _fetch_memory(self, memory_id: str) -> dict[str, Any] | None:
        """Load a memory row from SQLite."""
        row = self._db.execute(
            "SELECT id, title, body, project_id, created_at, is_active FROM memories WHERE id = ?",
            (memory_id,),
        ).fetchone()
        if not row:
            return None
        return {
            "id": row[0],
            "title": row[1],
            "body": row[2],
            "project_id": row[3],
            "created_at": row[4],
            "is_active": row[5],
        }

    def recall_changed(self, project_id: str, new_ids: list[str]) -> bool:
        """Return True if the recall set is different from what's stored."""
        row = self._db.execute(
            "SELECT memory_ids FROM recall_state WHERE project_id = ?",
            (project_id,),
        ).fetchone()
        if not row:
            return True
        if not self.recall_file_path(project_id).is_file():
            return True
        try:
            stored_ids = json.loads(row[0])
            return bool(stored_ids != new_ids)
        except (json.JSONDecodeError, TypeError):
            return True

    def update_recall_state(self, project_id: str, memory_ids: list[str]) -> None:
        """Persist the current recall result IDs to DB."""
        now = _utcnow()
        self._db.execute(
            """
            INSERT INTO recall_state (project_id, memory_ids, last_recall_at)
            VALUES (?, ?, ?)
            ON CONFLICT(project_id) DO UPDATE SET
              memory_ids = excluded.memory_ids,
              last_recall_at = excluded.last_recall_at
            """,
            (project_id, json.dumps(memory_ids), now),
        )
        self._db.commit()

    def write_recall_file(
        self,
        memories: list[dict[str, Any]],
        project_id: str,
        query_source: str,
    ) -> None:
        """Write the recall Markdown file for the given project.

        Args:
            memories: List of memory dicts to write.
            project_id: Normalized project directory path (used as the write location).
            query_source: Label describing what triggered the recall (e.g. "turn-context").
        """
        timestamp = _utcnow()
        n = len(memories)

        lines = [
            f"<!-- yaaml recall — {timestamp} | {n} memories | query: {query_source} -->",
            "",
        ]

        for mem in memories:
            title = mem.get("title", "Untitled")
            body = mem.get("body", "")
            proj = mem.get("project_id", "")
            created = mem.get("created_at", "")[:10]  # date portion

            lines.append(f"## {title}")
            origin = f" · {proj}" if proj and proj != project_id else ""
            lines.append(f"*{created}{origin}*")
            lines.append("")
            lines.append(body)
            lines.append("")
            lines.append("---")
            lines.append("")

        content = "\n".join(lines)

        recall_file = self.recall_file_path(project_id)
        try:
            recall_file.parent.mkdir(parents=True, exist_ok=True)
            recall_file.write_text(content, encoding="utf-8")
            logger.info("Wrote recall file: %s (%d memories)", recall_file, n)
        except Exception as exc:
            logger.error("Failed to write recall file for %s: %s", project_id, exc)

    def recall_file_path(self, project_id: str) -> Path:
        """Resolve the configured recall path for a project."""
        configured = self._config.recall_file_path
        if configured.is_absolute():
            return configured
        return Path(project_id) / configured
