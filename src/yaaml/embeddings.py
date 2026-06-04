"""ChromaDB-backed vector embedding store for YAAML memories."""

import logging
import os
from pathlib import Path

import chromadb
from chromadb.config import Settings

logger = logging.getLogger(__name__)

COLLECTION_NAME = "memories_v1"


def _make_embedding_function(embedding_model: str):
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

    def upsert(self, memory_id: str, text: str, metadata: dict) -> None:
        """Insert or update a memory embedding.

        Args:
            memory_id: Unique memory identifier.
            text: Text to embed (title + body).
            metadata: Arbitrary metadata dict stored alongside the embedding.
        """
        try:
            # ChromaDB metadata values must be str/int/float/bool
            safe_meta = {k: (v if isinstance(v, (str, int, float, bool)) else str(v))
                         for k, v in metadata.items()}
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
        where: dict | None = None,
    ) -> list[dict]:
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
            kwargs: dict = {
                "query_texts": [query_text],
                "n_results": actual_n,
                "include": ["distances", "metadatas"],
            }
            if where:
                kwargs["where"] = where

            results = self._collection.query(**kwargs)

            ids = results.get("ids", [[]])[0]
            distances = results.get("distances", [[]])[0]
            metadatas = results.get("metadatas", [[]])[0]

            output = []
            for doc_id, dist, meta in zip(ids, distances, metadatas):
                output.append({"id": doc_id, "distance": dist, "metadata": meta or {}})

            # Already sorted by distance ascending from ChromaDB, but be explicit
            output.sort(key=lambda x: x["distance"])
            return output
        except Exception as exc:
            logger.error("EmbeddingStore.search failed: %s", exc)
            return []

    def get_all(self) -> list[dict]:
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
            ids = results.get("ids", [])
            embeddings = results.get("embeddings", [])
            metadatas = results.get("metadatas", [])
            output = []
            for doc_id, emb, meta in zip(ids, embeddings, metadatas):
                output.append({"id": doc_id, "embedding": emb, "metadata": meta or {}})
            return output
        except Exception as exc:
            logger.error("EmbeddingStore.get_all failed: %s", exc)
            return []
