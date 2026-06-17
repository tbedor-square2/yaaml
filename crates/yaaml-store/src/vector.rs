use rusqlite::{params, OptionalExtension};
use yaaml_core::{cosine_similarity, EmbeddingRecord, VectorHit, VectorIndex};

use crate::database::{decode_f32_embedding, encode_f32_embedding, Database, DatabaseError};

pub struct SqliteExactVectorIndex<'a> {
    db: &'a Database,
    embedding_model: String,
    updated_at: String,
}

impl<'a> SqliteExactVectorIndex<'a> {
    pub fn new(db: &'a Database, embedding_model: String, updated_at: String) -> Self {
        Self {
            db,
            embedding_model,
            updated_at,
        }
    }
}

impl VectorIndex for SqliteExactVectorIndex<'_> {
    type Error = DatabaseError;

    fn upsert(
        &self,
        memory_id: i64,
        embedding: &[f32],
        embedded_text_hash: &str,
    ) -> Result<(), Self::Error> {
        self.db.upsert_embedding(&EmbeddingRecord {
            memory_id,
            embedding_model: self.embedding_model.clone(),
            dimensions: embedding.len() as u64,
            embedding_blob: encode_f32_embedding(embedding),
            embedded_text_hash: embedded_text_hash.to_string(),
            updated_at: self.updated_at.clone(),
        })
    }

    fn remove(&self, memory_id: i64) -> Result<(), Self::Error> {
        self.db.conn().execute(
            "DELETE FROM embeddings WHERE memory_id = ?1",
            params![memory_id],
        )?;
        Ok(())
    }

    fn search(
        &self,
        query: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<VectorHit>, Self::Error> {
        let mut stmt = self.db.conn().prepare(
            "SELECT embeddings.memory_id, embeddings.embedding_blob
             FROM embeddings
             JOIN memories ON memories.id = embeddings.memory_id
             WHERE memories.is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (memory_id, blob) = row?;
            let Some(vector) = decode_f32_embedding(&blob) else {
                continue;
            };
            let Some(similarity) = cosine_similarity(query, &vector) else {
                continue;
            };
            if similarity >= threshold {
                hits.push(VectorHit {
                    memory_id,
                    similarity,
                });
            }
        }
        hits.sort_by(|left, right| {
            right
                .similarity
                .partial_cmp(&left.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}

pub fn has_embedding(db: &Database, memory_id: i64) -> Result<bool, DatabaseError> {
    let exists: i64 = db
        .conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM embeddings WHERE memory_id = ?1)",
            params![memory_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(exists != 0)
}

#[cfg(test)]
mod tests {
    use yaaml_core::{embedded_text_hash, MemoryKind, MemoryRecord, MemoryScope};

    use super::*;

    #[test]
    fn sqlite_exact_vector_index_ranks_by_cosine_similarity() {
        let mut db = Database::in_memory().unwrap();
        db.migrate().unwrap();
        let memory = |title: &str| MemoryRecord {
            id: None,
            title: title.to_string(),
            body: title.to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust".to_string()),
            lineage_refs: Vec::new(),
        };
        let first = db.insert_memory(&memory("first")).unwrap();
        let second = db.insert_memory(&memory("second")).unwrap();
        let index = SqliteExactVectorIndex::new(
            &db,
            "text-embedding-3-small".to_string(),
            "2026-06-08T00:00:01Z".to_string(),
        );

        index
            .upsert(first, &[1.0, 0.0], &embedded_text_hash("first"))
            .unwrap();
        index
            .upsert(second, &[0.0, 1.0], &embedded_text_hash("second"))
            .unwrap();
        let hits = index.search(&[1.0, 0.0], 5, 0.0).unwrap();

        assert_eq!(hits[0].memory_id, first);
        assert!(hits[0].similarity > hits[1].similarity);
    }
}
