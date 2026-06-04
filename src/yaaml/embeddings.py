"""ChromaDB-backed vector embedding store for YAAML memories."""

import logging
import os
from pathlib import Path
from typing import Any

import chromadb
from chromadb.config import Settings

logger = logging.getLogger(__name__)

COLLECTION_NAME = "memories_v1"


def _make_embedding_function(embedding_model: str) -> Any:
    """Build the appropriate ChromaDB embedding function.

    Uses OpenAI embeddings when OPENAI_API_KEY is set, otherwise falls back to
    the built-in sentence-transformers embedding function.
    """
    api_key = os.environ.get("OPENAI_API_KEY")
    if api_key:
        try:
            from chromadb.utils.embedding_functions import OpenAIEmbeddingFunction

            return OpenAIEmbeddingFunction(
                api_key=api_key,
                model_name=embedding_model,
            )
        except Exception as exc:
            logger.warning(
                "Could not initialize OpenAI embedding function (%s); falling back to default.",
                exc,
            )

    # Fallback: sentence-transformers (no external key required)
    from chromadb.utils.embedding_functions import DefaultEmbeddingFunction

    return DefaultEmbeddingFunction()


class EmbeddingStore:
    """Persistent vector store backed by ChromaDB."""

    def __init__(self, chroma_path: Path, embedding_model: str) -> None:
        chroma_path.mkdir(parents=True, exist_ok=True)
        self._client = chromadb.PersistentClient(
            path=str(chroma_path),
            settings=Settings(anonymized_telemetry=False),
        )
        self._ef = _make_embedding_function(embedding_model)
        self._collection = self._client.get_or_create_collection(
            name=COLLECTION_NAME,
            embedding_function=self._ef,
            metadata={"hnsw:space": "cosine"},
        )

    def upsert(self, memory_id: str, text: str, metadata: dict[str, Any]) -> None:
        """Insert or update a memory embedding.

        Args:
            memory_id: Unique memory identifier.
            text: Text to embed (title + body).
            metadata: Arbitrary metadata dict stored alongside the embedding.
        """
        try:
            # ChromaDB metadata values must be str/int/float/bool
            safe_meta = {
                k: (v if isinstance(v, (str, int, float, bool)) else str(v))
                for k, v in metadata.items()
            }
            self._collection.upsert(
                ids=[memory_id],
                documents=[text],
                metadatas=[safe_meta],
            )
        except Exception as exc:
            logger.error("EmbeddingStore.upsert failed for %s: %s", memory_id, exc)
            raise

    def delete(self, memory_id: str) -> None:
        """Remove a memory embedding.

        Args:
            memory_id: Memory ID to delete.
        """
        try:
            self._collection.delete(ids=[memory_id])
        except Exception as exc:
            logger.warning("EmbeddingStore.delete failed for %s: %s", memory_id, exc)

    def search(
        self,
        query_text: str,
        n_results: int,
        where: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        """Semantic search against stored memories.

        Args:
            query_text: Query string to embed.
            n_results: Maximum number of results to return.
            where: Optional ChromaDB metadata filter.

        Returns:
            List of dicts with keys: id, distance, metadata. Sorted by distance
            ascending (most similar first).
        """
        try:
            count = self._collection.count()
            if count == 0:
                return []
            actual_n = min(n_results, count)
            kwargs: dict[str, Any] = {
                "query_texts": [query_text],
                "n_results": actual_n,
                "include": ["distances", "metadatas"],
            }
            if where:
                kwargs["where"] = where

            results = self._collection.query(**kwargs)

            ids: list[str] = list((results.get("ids") or [[]])[0])
            distances: list[float] = list((results.get("distances") or [[]])[0])
            raw_metas = (results.get("metadatas") or [[]])[0]
            metadatas: list[dict[str, Any]] = [dict(m) for m in raw_metas]

            output: list[dict[str, Any]] = []
            for doc_id, dist, meta in zip(ids, distances, metadatas, strict=False):
                output.append({"id": doc_id, "distance": dist, "metadata": meta or {}})

            # Already sorted by distance ascending from ChromaDB, but be explicit
            output.sort(key=lambda x: x["distance"])
            return output
        except Exception as exc:
            logger.error("EmbeddingStore.search failed: %s", exc)
            return []

    def get_all(self) -> list[dict[str, Any]]:
        """Retrieve all stored embeddings.

        Returns:
            List of dicts with keys: id, embedding, metadata.
        """
        try:
            count = self._collection.count()
            if count == 0:
                return []
            results = self._collection.get(
                include=["embeddings", "metadatas"],
            )
            raw = results
            ids: list[str] = list(raw.get("ids") or [])
            embeddings: list[Any] = list(raw.get("embeddings") or [])
            raw_metas2 = raw.get("metadatas") or []
            metadatas: list[dict[str, Any]] = [dict(m) for m in raw_metas2]
            output: list[dict[str, Any]] = []
            for doc_id, emb, meta in zip(ids, embeddings, metadatas, strict=False):
                output.append({"id": doc_id, "embedding": emb, "metadata": meta or {}})
            return output
        except Exception as exc:
            logger.error("EmbeddingStore.get_all failed: %s", exc)
            return []
