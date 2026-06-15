use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::paths::project_hash;
use crate::{context_score, ContextMetadata, MemoryKind, MemoryRecord, MemoryScope, TurnRecord};

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
    pub rank: RecallRankDetails,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallRankDetails {
    pub vector_score: f32,
    pub context_score: f32,
    pub project_bonus: f32,
    pub task_key_bonus: f32,
    pub global_durable_bonus: f32,
    pub penalties: Vec<String>,
    pub matched_task_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallMemory {
    pub memory_id: i64,
    pub title: String,
    pub body: String,
    pub created_at: String,
    pub project_id: Option<String>,
    pub project_descriptor: Option<String>,
    pub score: f32,
    pub rank: RecallRankDetails,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallRankingOptions {
    pub project_tiebreaker: bool,
    pub project_score_bonus: f32,
}

pub fn rank_recall_candidates(
    hits: &[VectorHit],
    memories: &[MemoryRecord],
    current_project_id: &str,
    query_context: &ContextMetadata,
    query_task_keys: &[String],
    options: RecallRankingOptions,
) -> Vec<RecallCandidate> {
    let mut candidates = hits
        .iter()
        .filter_map(|hit| {
            memories
                .iter()
                .find(|memory| memory.id == Some(hit.memory_id))
                .map(|memory| {
                    rank_recall_candidate(
                        *hit,
                        memory,
                        current_project_id,
                        query_context,
                        query_task_keys,
                        options,
                    )
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    candidates
}

fn rank_recall_candidate(
    hit: VectorHit,
    memory: &MemoryRecord,
    current_project_id: &str,
    query_context: &ContextMetadata,
    query_task_keys: &[String],
    options: RecallRankingOptions,
) -> RecallCandidate {
    let memory_context = crate::infer_context_from_memory(memory);
    let context_component = context_score(query_context, &memory_context);
    let project_bonus =
        if options.project_tiebreaker && memory.project_id.as_deref() == Some(current_project_id) {
            options.project_score_bonus
        } else {
            0.0
        };
    let matched_task_keys = matched_task_keys(query_task_keys, &memory.task_keys);
    let task_key_bonus = (matched_task_keys.len() as f32 * 0.28).min(0.70);
    let global_durable_bonus = if memory.scope == MemoryScope::Global
        && matches!(
            memory.kind,
            MemoryKind::Preference | MemoryKind::Lesson | MemoryKind::Workflow
        ) {
        0.08
    } else {
        0.0
    };
    let mut penalty = 0.0;
    let mut penalties = Vec::new();
    if !query_task_keys.is_empty() && matched_task_keys.is_empty() {
        if memory.project_id.as_deref() == Some(current_project_id) {
            penalty += 0.24;
            penalties.push("same_project_no_task_key_overlap".to_string());
        }
        if memory.kind == MemoryKind::TaskState {
            penalty += 0.35;
            penalties.push("task_state_without_task_key_overlap".to_string());
        }
    }
    if memory.scope == MemoryScope::Global
        && memory.kind == MemoryKind::TaskState
        && matched_task_keys.is_empty()
    {
        penalty += 0.35;
        penalties.push("global_task_state_without_task_key_overlap".to_string());
    }

    let score =
        hit.similarity + context_component + project_bonus + task_key_bonus + global_durable_bonus
            - penalty;
    RecallCandidate {
        memory_id: hit.memory_id,
        similarity: hit.similarity,
        score,
        project_id: memory.project_id.clone(),
        rank: RecallRankDetails {
            vector_score: hit.similarity,
            context_score: context_component,
            project_bonus,
            task_key_bonus,
            global_durable_bonus,
            penalties,
            matched_task_keys,
        },
    }
}

pub fn extract_task_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let tokens = task_key_tokens(text);
    for index in 0..tokens.len() {
        let token = tokens[index].trim_matches(trim_task_key_punctuation);
        if token.is_empty() {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if let Some(pr) = pull_request_key(&lower) {
            push_unique(&mut keys, pr);
        }
        if matches!(
            lower.as_str(),
            "pr" | "pull" | "pull-request" | "pull_request"
        ) {
            if let Some(next) = tokens.get(index + 1).and_then(|next| numeric_token(next)) {
                push_unique(&mut keys, format!("pr:{next}"));
            }
        }
        if let Some(ticket) = ticket_key(token) {
            push_unique(&mut keys, ticket);
        }
        if let Some(path) = path_key(token) {
            push_unique(&mut keys, path);
        }
        if matches!(lower.as_str(), "branch" | "branch:") {
            if let Some(next) = tokens.get(index + 1).and_then(|next| branch_key(next)) {
                push_unique(&mut keys, next);
            }
        }
        if let Some(tool) = tool_key(&lower) {
            push_unique(&mut keys, tool);
        }
        if keys.len() >= 48 {
            break;
        }
    }
    keys
}

pub fn infer_memory_kind(title: &str, body: &str, scope: MemoryScope) -> MemoryKind {
    let text = format!("{} {}", title, body).to_ascii_lowercase();
    if text.contains("prefer")
        || text.contains("preference")
        || text.contains("user correction")
        || text.contains("user redirected")
        || text.contains("repeatedly prefers")
    {
        MemoryKind::Preference
    } else if text.contains("workflow")
        || text.contains("command")
        || text.contains("run ")
        || text.contains("use ")
        || text.contains("skill")
    {
        MemoryKind::Workflow
    } else if text.contains("pr ")
        || text.contains("pull/")
        || text.contains("branch")
        || text.contains("status")
        || text.contains("blocked")
    {
        MemoryKind::TaskState
    } else if scope == MemoryScope::Project {
        MemoryKind::ProjectFact
    } else {
        MemoryKind::Lesson
    }
}

pub fn normalize_memory_kind(
    kind: Option<&str>,
    title: &str,
    body: &str,
    scope: MemoryScope,
) -> MemoryKind {
    match kind.map(str::trim) {
        Some("preference") => MemoryKind::Preference,
        Some("workflow") => MemoryKind::Workflow,
        Some("project_fact") | Some("project-fact") => MemoryKind::ProjectFact,
        Some("task_state") | Some("task-state") => MemoryKind::TaskState,
        Some("lesson") => MemoryKind::Lesson,
        _ => infer_memory_kind(title, body, scope),
    }
}

fn matched_task_keys(query_task_keys: &[String], memory_task_keys: &[String]) -> Vec<String> {
    let mut matched = Vec::new();
    for query_key in query_task_keys {
        if memory_task_keys
            .iter()
            .any(|memory_key| memory_key == query_key)
        {
            push_unique(&mut matched, query_key.clone());
        }
    }
    matched
}

fn task_key_tokens(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn trim_task_key_punctuation(character: char) -> bool {
    matches!(
        character,
        ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '`'
    )
}

fn pull_request_key(token: &str) -> Option<String> {
    if let Some((_, number)) = token.rsplit_once("/pull/") {
        return numeric_token(number).map(|number| format!("pr:{number}"));
    }
    if let Some(number) = token.strip_prefix("pull/") {
        return numeric_token(number).map(|number| format!("pr:{number}"));
    }
    if let Some(number) = token.strip_prefix("pr#") {
        return numeric_token(number).map(|number| format!("pr:{number}"));
    }
    None
}

fn ticket_key(token: &str) -> Option<String> {
    let token = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-');
    let (prefix, number) = token.split_once('-')?;
    if prefix.len() < 2
        || !prefix
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || !number.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some(format!("ticket:{}-{}", prefix.to_ascii_uppercase(), number))
}

fn path_key(token: &str) -> Option<String> {
    let token =
        token.trim_matches(|ch: char| matches!(ch, ',' | ';' | ':' | '"' | '\'' | ')' | ']' | '}'));
    if token.starts_with("http://") || token.starts_with("https://") {
        return None;
    }
    if !token.contains('/') || token.len() < 6 {
        return None;
    }
    if !token
        .chars()
        .any(|ch| ch.is_ascii_alphabetic() || ch.is_ascii_digit())
    {
        return None;
    }
    let normalized = token
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    Some(format!("path:{normalized}"))
}

fn branch_key(token: &str) -> Option<String> {
    let token = token.trim_matches(trim_task_key_punctuation);
    if token.len() < 3 || token.starts_with('-') {
        return None;
    }
    Some(format!("branch:{}", token.to_ascii_lowercase()))
}

fn tool_key(token: &str) -> Option<String> {
    match token {
        "yaaml" | "gt" | "bazel" | "bin/bazel" | "sq" | "cargo" | "just" | "gh" => {
            Some(format!("tool:{token}"))
        }
        _ => None,
    }
}

fn numeric_token(token: &str) -> Option<String> {
    let number = token.trim_matches(|ch: char| !ch.is_ascii_digit());
    if number.is_empty() {
        return None;
    }
    Some(number.to_string())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
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

pub fn session_recall_file_path(recall_dir: &Path, session_id: &str) -> PathBuf {
    recall_dir.join(format!("session-{}.md", safe_session_id(session_id)))
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

pub fn parse_memory_ids(contents: &str) -> Vec<i64> {
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

    use crate::{MemoryKind, MemoryRecord, MemoryScope};

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
                    rank: empty_rank(),
                },
                RecallCandidate {
                    memory_id: 2,
                    similarity: 0.7,
                    score: 0.7,
                    project_id: Some("/tmp/current".to_string()),
                    rank: empty_rank(),
                },
            ],
            "/tmp/current",
            0.05,
        );

        assert_eq!(candidates[0].memory_id, 1);
    }

    #[test]
    fn project_bonus_preserves_existing_score_adjustments() {
        let candidates = apply_project_bonus(
            vec![RecallCandidate {
                memory_id: 1,
                similarity: 0.7,
                score: 0.9,
                project_id: Some("/tmp/current".to_string()),
                rank: empty_rank(),
            }],
            "/tmp/current",
            0.05,
        );

        assert_eq!(candidates[0].score, 0.95);
    }

    #[test]
    fn task_key_extraction_finds_pr_ticket_path_and_tool_keys() {
        let keys = extract_task_keys(
            "PR 481245 updates riskarbiter/src/main/java/Foo.java for MLP-4400; run yaaml recall",
        );

        assert!(keys.contains(&"pr:481245".to_string()));
        assert!(keys.contains(&"ticket:MLP-4400".to_string()));
        assert!(keys.contains(&"path:riskarbiter/src/main/java/foo.java".to_string()));
        assert!(keys.contains(&"tool:yaaml".to_string()));
    }

    #[test]
    fn task_key_overlap_beats_same_project_wrong_task() {
        let current_project = "/Users/tbedor/Development/java";
        let hits = vec![
            VectorHit {
                memory_id: 1,
                similarity: 0.90,
            },
            VectorHit {
                memory_id: 2,
                similarity: 0.70,
            },
        ];
        let memories = vec![
            memory(
                1,
                "Unrelated Java PR",
                MemoryKind::TaskState,
                Some(current_project),
                vec!["pr:111111".to_string()],
            ),
            memory(
                2,
                "Risk Arbiter target PR",
                MemoryKind::TaskState,
                Some("/Users/tbedor/Development/java/riskarbiter"),
                vec!["pr:481245".to_string()],
            ),
        ];
        let candidates = rank_recall_candidates(
            &hits,
            &memories,
            current_project,
            &ContextMetadata::default(),
            &["pr:481245".to_string()],
            RecallRankingOptions {
                project_tiebreaker: true,
                project_score_bonus: 0.05,
            },
        );

        assert_eq!(candidates[0].memory_id, 2);
        assert!(candidates[0]
            .rank
            .matched_task_keys
            .contains(&"pr:481245".to_string()));
        assert!(candidates[1]
            .rank
            .penalties
            .contains(&"same_project_no_task_key_overlap".to_string()));
    }

    fn empty_rank() -> RecallRankDetails {
        RecallRankDetails {
            vector_score: 0.0,
            context_score: 0.0,
            project_bonus: 0.0,
            task_key_bonus: 0.0,
            global_durable_bonus: 0.0,
            penalties: Vec::new(),
            matched_task_keys: Vec::new(),
        }
    }

    fn memory(
        id: i64,
        title: &str,
        kind: MemoryKind,
        project_id: Option<&str>,
        task_keys: Vec<String>,
    ) -> MemoryRecord {
        MemoryRecord {
            id: Some(id),
            title: title.to_string(),
            body: title.to_string(),
            scope: MemoryScope::Project,
            kind,
            task_keys,
            source_turn_refs: Vec::new(),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            is_active: true,
            session_id: None,
            project_id: project_id.map(str::to_string),
            project_descriptor: project_id.map(str::to_string),
            lineage_refs: Vec::new(),
        }
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
            cwd: None,
            context: None,
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
    fn session_recall_path_uses_only_session_id() {
        let path = session_recall_file_path(Path::new("/tmp/recall"), "a/b");

        assert_eq!(path, Path::new("/tmp/recall/session-a_b.md"));
    }
}
