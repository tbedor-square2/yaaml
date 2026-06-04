"""Memory consolidation via DBSCAN clustering and LLM merging."""

import json
import logging
import sqlite3
import uuid
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import numpy as np
from sklearn.cluster import DBSCAN

from .config import Config

if TYPE_CHECKING:
    from .embeddings import EmbeddingStore

logger = logging.getLogger(__name__)


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


class ConsolidationManager:
    """Identifies and merges near-duplicate memories via DBSCAN + LLM."""

    def __init__(
        self,
        db: sqlite3.Connection,
        store: "EmbeddingStore",
        config: Config,
    ) -> None:
        self._db = db
        self._store = store
        self._config = config

    async def run(self) -> None:
        """Full consolidation pass.

        1. Fetch all active memory embeddings from ChromaDB.
        2. Run DBSCAN with cosine metric to find clusters.
        3. For each cluster, call the LLM to merge memories.
        4. Insert the merged memory into DB + ChromaDB.
        5. Mark source memories as inactive.
        """
        from . import llm as llm_module

        logger.info("Starting consolidation pass.")

        all_items = self._store.get_all()
        if len(all_items) < 2:
            logger.info("Fewer than 2 memories — nothing to consolidate.")
            return

        # Filter to only active memories
        active_ids = self._get_active_memory_ids()
        active_items = [item for item in all_items if item["id"] in active_ids]

        if len(active_items) < 2:
            logger.info("Fewer than 2 active memories — nothing to consolidate.")
            return

        # Group by project_id so memories from different projects are never merged.
        by_project: dict[str, list[dict[str, Any]]] = {}
        for item in active_items:
            pid = item.get("metadata", {}).get("project_id", "")
            by_project.setdefault(pid, []).append(item)

        all_clusters: list[list[str]] = []

        for pid, project_items in by_project.items():
            if len(project_items) < 2:
                continue

            ids = [item["id"] for item in project_items]
            embeddings = np.array([item["embedding"] for item in project_items], dtype=np.float32)

            # DBSCAN with cosine distance; eps=0.08 ≈ cosine distance for ~0.92 similarity
            try:
                labels = DBSCAN(
                    eps=0.08,
                    min_samples=2,
                    metric="cosine",
                ).fit_predict(embeddings)
            except Exception as exc:
                logger.error("DBSCAN clustering failed for project %s: %s", pid, exc)
                continue

            # Group IDs by cluster label (exclude noise label -1)
            clusters: dict[int, list[str]] = {}
            for label, memory_id in zip(labels, ids, strict=False):
                if label == -1:
                    continue
                clusters.setdefault(label, []).append(memory_id)

            all_clusters.extend(clusters.values())

        if not all_clusters:
            logger.info("No clusters found — no consolidation needed.")
            return

        logger.info("Found %d clusters to consolidate.", len(all_clusters))

        for cluster_ids in all_clusters:
            await self._consolidate_cluster(cluster_ids, llm_module)

        logger.info("Consolidation pass complete.")

    async def _consolidate_cluster(
        self,
        cluster_ids: list[str],
        llm_module: Any,
    ) -> None:
        """Merge one cluster of memories into a single consolidated memory."""
        # Fetch full memory records
        placeholders = ",".join("?" * len(cluster_ids))
        rows = self._db.execute(
            f"SELECT id, title, body, project_id, session_id FROM memories "
            f"WHERE id IN ({placeholders}) AND is_active = 1",
            cluster_ids,
        ).fetchall()

        if not rows:
            return

        memories = [
            {
                "id": row[0],
                "title": row[1],
                "body": row[2],
                "project_id": row[3],
                "session_id": row[4],
            }
            for row in rows
        ]

        logger.info(
            "Merging cluster of %d memories: %s",
            len(memories),
            [m["title"] for m in memories],
        )

        try:
            title, body = await llm_module.merge_memories(memories, self._config)
        except Exception as exc:
            logger.error("LLM merge failed for cluster: %s", exc)
            return

        now = _utcnow()
        merged_id = str(uuid.uuid4())

        # Use the project_id from the first memory in the cluster
        project_id = memories[0]["project_id"]
        session_id = memories[0]["session_id"]
        source_ids = [m["id"] for m in memories]

        # Insert merged memory
        self._db.execute(
            """
            INSERT INTO memories
              (id, title, body, source_turn_ids, created_at, updated_at,
               is_active, session_id, project_id, consolidated_into)
            VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, NULL)
            """,
            (
                merged_id,
                title,
                body,
                json.dumps(source_ids),  # reuse source_ids as source_turn_ids for merged
                now,
                now,
                session_id,
                project_id,
            ),
        )

        # Mark source memories as inactive
        for mem_id in source_ids:
            self._db.execute(
                "UPDATE memories SET is_active = 0, consolidated_into = ? WHERE id = ?",
                (merged_id, mem_id),
            )

        self._db.commit()

        # Index merged memory in ChromaDB
        try:
            self._store.upsert(
                merged_id,
                f"{title}\n\n{body}",
                {"project_id": project_id, "session_id": session_id or ""},
            )
        except Exception as exc:
            logger.error("Failed to upsert merged memory embedding %s: %s", merged_id, exc)

        # Remove source embeddings from ChromaDB
        for mem_id in source_ids:
            try:
                self._store.delete(mem_id)
            except Exception as exc:
                logger.warning("Failed to delete source embedding %s: %s", mem_id, exc)

        logger.info("Consolidated %d memories into %s: %s", len(source_ids), merged_id, title)

    def _get_active_memory_ids(self) -> set[str]:
        """Return the set of active memory IDs from SQLite."""
        rows = self._db.execute("SELECT id FROM memories WHERE is_active = 1").fetchall()
        return {row[0] for row in rows}
