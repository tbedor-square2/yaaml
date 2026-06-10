use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::project_hash;
use crate::TurnRecord;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorHit {
    pub memory_id: i64,
    pub similarity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecallCandidate {
    pub memory_id: i64,
    pub similarity: f32,
    pub score: f32,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecallMemory {
    pub memory_id: i64,
    pub title: String,
    pub body: String,
    pub created_at: String,
    pub project_id: Option<String>,
    pub project_descriptor: Option<String>,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallWrite {
    Written,
    Unchanged,
    NoopEmptyResults,
}

pub trait VectorIndex {
    type Error;

    fn upsert(
        &self,
        memory_id: i64,
        embedding: &[f32],
        embedded_text_hash: &str,
    ) -> Result<(), Self::Error>;
    fn remove(&self, memory_id: i64) -> Result<(), Self::Error>;
    fn search(
        &self,
        query: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<VectorHit>, Self::Error>;
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (left, right) in left.iter().zip(right.iter()) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        None
    } else {
        Some(dot / (left_norm.sqrt() * right_norm.sqrt()))
    }
}

pub fn apply_project_bonus(
    mut candidates: Vec<RecallCandidate>,
    current_project_id: &str,
    bonus: f32,
) -> Vec<RecallCandidate> {
    for candidate in &mut candidates {
        candidate.score = candidate.similarity;
        if candidate.project_id.as_deref() == Some(current_project_id) {
            candidate.score += bonus;
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    candidates
}

pub fn build_recall_query(
    turns: &[TurnRecord],
    max_chars: usize,
    tool_output_truncation_chars: usize,
) -> String {
    let mut query = String::new();
    for turn in turns {
        let Some(display_text) = &turn.display_text else {
            continue;
        };
        for line in display_text.lines() {
            let line = if line.trim_start().starts_with("tool output:") {
                truncate_chars(line, tool_output_truncation_chars)
            } else {
                line.to_string()
            };
            if !query.is_empty() {
                query.push('\n');
            }
            query.push_str(&line);
            if query.chars().count() >= max_chars {
                return truncate_chars(&query, max_chars);
            }
        }
    }
    query
}

pub fn recall_file_path(recall_dir: &Path, project_id: &Path) -> PathBuf {
    recall_dir.join(format!("{}.md", project_hash(project_id)))
}

pub fn session_recall_file_path(recall_dir: &Path, project_id: &Path, session_id: &str) -> PathBuf {
    recall_dir.join(format!(
        "{}-{}.md",
        project_hash(project_id),
        safe_session_id(session_id)
    ))
}

fn safe_session_id(session_id: &str) -> String {
    let safe = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "unknown".to_string()
    } else {
        safe
    }
}

pub fn render_recall_markdown(
    query_timestamp: &str,
    query_source: &str,
    current_project_id: &str,
    memories: &[RecallMemory],
) -> String {
    let ids = memories
        .iter()
        .map(|memory| memory.memory_id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut rendered = format!(
        "# YAAML Recall\n\nquery_timestamp: {query_timestamp}\nquery_source: {query_source}\nmemory_count: {}\nmemory_ids: {ids}\n\n",
        memories.len()
    );

    for memory in memories {
        rendered.push_str("## ");
        rendered.push_str(&memory.title);
        rendered.push_str("\n\n");
        rendered.push_str(&memory.body);
        rendered.push_str("\n\n");
        rendered.push_str(&format!("created_at: {}\n", memory.created_at));
        if memory.project_id.as_deref() != Some(current_project_id) {
            if let Some(project) = &memory.project_descriptor {
                rendered.push_str(&format!("originating_project: {project}\n"));
            }
        }
        rendered.push('\n');
    }

    rendered
}

pub fn write_recall_file(
    path: &Path,
    rendered: &str,
    memory_ids: &[i64],
) -> io::Result<RecallWrite> {
    if memory_ids.is_empty() {
        return Ok(RecallWrite::NoopEmptyResults);
    }
    if let Ok(existing) = fs::read_to_string(path) {
        if parse_memory_ids(&existing) == memory_ids {
            return Ok(RecallWrite::Unchanged);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, rendered)?;
    Ok(RecallWrite::Written)
}

fn parse_memory_ids(contents: &str) -> Vec<i64> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("memory_ids: "))
        .unwrap_or("")
        .split(',')
        .filter_map(|part| part.trim().parse::<i64>().ok())
        .collect()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::TurnStatus;

    use super::*;

    #[test]
    fn cosine_similarity_ranks_known_vectors() {
        let exact = cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).unwrap();
        let orthogonal = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).unwrap();

        assert!(exact > orthogonal);
        assert_eq!(orthogonal, 0.0);
    }

    #[test]
    fn same_project_bonus_cannot_beat_clearly_more_relevant_memory() {
        let candidates = apply_project_bonus(
            vec![
                RecallCandidate {
                    memory_id: 1,
                    similarity: 0.9,
                    score: 0.9,
                    project_id: Some("/tmp/other".to_string()),
                },
                RecallCandidate {
                    memory_id: 2,
                    similarity: 0.7,
                    score: 0.7,
                    project_id: Some("/tmp/current".to_string()),
                },
            ],
            "/tmp/current",
            0.05,
        );

        assert_eq!(candidates[0].memory_id, 1);
    }

    #[test]
    fn recall_query_excludes_long_tool_output() {
        let turn = TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 0,
            byte_end: 1,
            observed_at: None,
            status: TurnStatus::Completed,
            display_text: Some(format!(
                "user: fix it\ntool output: {}\nassistant: done",
                "x".repeat(1_000)
            )),
        };

        let query = build_recall_query(&[turn], 2_000, 80);

        assert!(query.contains("user: fix it"));
        assert!(query.contains("assistant: done"));
        assert!(!query.contains(&"x".repeat(200)));
    }

    #[test]
    fn zero_result_recall_preserves_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("recall.md");
        fs::write(&path, "existing").unwrap();

        let result = write_recall_file(&path, "", &[]).unwrap();

        assert_eq!(result, RecallWrite::NoopEmptyResults);
        assert_eq!(fs::read_to_string(path).unwrap(), "existing");
    }

    #[test]
    fn repeated_identical_top_n_avoids_file_rewrite() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("recall.md");
        let first = "memory_ids: 1,2\nold timestamp\n";
        fs::write(&path, first).unwrap();

        let result = write_recall_file(&path, "memory_ids: 1,2\nnew timestamp\n", &[1, 2]).unwrap();

        assert_eq!(result, RecallWrite::Unchanged);
        assert_eq!(fs::read_to_string(path).unwrap(), first);
    }

    #[test]
    fn session_recall_path_includes_project_and_session() {
        let path =
            session_recall_file_path(Path::new("/tmp/recall"), Path::new("/tmp/project"), "a/b");

        assert!(path.ends_with(format!(
            "{}-a_b.md",
            project_hash(Path::new("/tmp/project"))
        )));
    }
}
