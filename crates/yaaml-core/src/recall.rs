use std::collections::HashSet;
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
    pub filter_reasons: Vec<String>,
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

pub fn select_recall_candidates(
    candidates: Vec<RecallCandidate>,
    memories: &[MemoryRecord],
    current_project_id: &str,
    query_context: &ContextMetadata,
    query_task_keys: &[String],
    limit: usize,
) -> (Vec<RecallCandidate>, Vec<RecallCandidate>) {
    let mut kept = Vec::new();
    let mut debug = Vec::new();
    for mut candidate in candidates {
        let decision = memories
            .iter()
            .find(|memory| memory.id == Some(candidate.memory_id))
            .map(|memory| {
                recall_filter_decision(
                    &candidate,
                    memory,
                    current_project_id,
                    query_context,
                    query_task_keys,
                )
            })
            .unwrap_or_else(|| RecallFilterDecision {
                keep: false,
                reasons: vec!["drop:memory_not_loaded".to_string()],
            });
        candidate.rank.filter_reasons = decision.reasons;
        if decision.keep && kept.len() < limit {
            kept.push(candidate.clone());
        }
        debug.push(candidate);
    }
    (kept, debug)
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
    let strong_task_key_matches = matched_task_keys
        .iter()
        .filter(|key| is_strong_task_key(key))
        .count();
    let weak_task_key_matches = matched_task_keys
        .len()
        .saturating_sub(strong_task_key_matches);
    let task_key_bonus =
        (strong_task_key_matches as f32 * 0.28 + weak_task_key_matches as f32 * 0.06).min(0.70);
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
    if !query_task_keys.is_empty()
        && matched_task_keys.is_empty()
        && memory.project_id.as_deref() == Some(current_project_id)
    {
        penalty += 0.24;
        penalties.push("same_project_no_task_key_overlap".to_string());
    }
    if memory.kind == MemoryKind::TaskState && matched_task_keys.is_empty() {
        penalty += 0.45;
        penalties.push("task_state_without_task_key_overlap".to_string());
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
            filter_reasons: Vec::new(),
            matched_task_keys,
        },
    }
}

struct RecallFilterDecision {
    keep: bool,
    reasons: Vec<String>,
}

fn recall_filter_decision(
    candidate: &RecallCandidate,
    memory: &MemoryRecord,
    current_project_id: &str,
    query_context: &ContextMetadata,
    _query_task_keys: &[String],
) -> RecallFilterDecision {
    let memory_context = crate::infer_context_from_memory(memory);
    let same_project = memory.project_id.as_deref() == Some(current_project_id);
    let same_repo =
        query_context.repo_id.is_some() && query_context.repo_id == memory_context.repo_id;
    let same_work_area =
        query_context.work_area.is_some() && query_context.work_area == memory_context.work_area;
    let strong_task_key_match = candidate
        .rank
        .matched_task_keys
        .iter()
        .any(|key| is_strong_task_key(key));
    let weak_task_key_match = !candidate.rank.matched_task_keys.is_empty();
    let strong_context = same_work_area
        || (same_repo && candidate.rank.context_score >= 0.36)
        || candidate.rank.context_score >= 0.42;

    let mut reasons = Vec::new();
    if strong_task_key_match {
        reasons.push("keep:strong_task_key_match".to_string());
        return RecallFilterDecision {
            keep: true,
            reasons,
        };
    }

    match memory.kind {
        MemoryKind::TaskState => {
            if strong_task_key_match {
                reasons.push("keep:task_state_strong_task_key_match".to_string());
                return RecallFilterDecision {
                    keep: true,
                    reasons,
                };
            }
            if weak_task_key_match {
                reasons.push("drop:task_state_without_strong_task_key_match".to_string());
            } else if same_project && strong_context {
                reasons.push("drop:stale_task_state_semantic_context_only".to_string());
            } else {
                reasons.push("drop:task_state_without_task_key_match".to_string());
            }
            RecallFilterDecision {
                keep: false,
                reasons,
            }
        }
        MemoryKind::ProjectFact => {
            if same_project || strong_context {
                reasons.push("keep:project_fact_context".to_string());
                RecallFilterDecision {
                    keep: true,
                    reasons,
                }
            } else {
                reasons.push("drop:project_fact_wrong_context".to_string());
                RecallFilterDecision {
                    keep: false,
                    reasons,
                }
            }
        }
        MemoryKind::Preference | MemoryKind::Lesson | MemoryKind::Workflow => {
            if memory.scope == MemoryScope::Global {
                if candidate.rank.context_score < 0.0 && !weak_task_key_match {
                    reasons.push("drop:global_durable_wrong_context".to_string());
                    return RecallFilterDecision {
                        keep: false,
                        reasons,
                    };
                }
                reasons.push("keep:global_durable".to_string());
            } else if same_project {
                reasons.push("keep:same_project_durable".to_string());
            } else if strong_context {
                reasons.push("keep:cross_project_strong_context".to_string());
            } else if weak_task_key_match {
                reasons.push("keep:weak_task_key_semantic_durable".to_string());
            } else {
                reasons.push("keep:semantic_durable".to_string());
            }
            RecallFilterDecision {
                keep: true,
                reasons,
            }
        }
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
        if let Some(target) = target_key(token) {
            push_unique(&mut keys, target);
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
        let normalized_query_key = query_key.to_ascii_lowercase();
        if memory_task_keys
            .iter()
            .any(|memory_key| memory_key.to_ascii_lowercase() == normalized_query_key)
        {
            push_unique(&mut matched, query_key.clone());
        }
    }
    matched
}

fn is_strong_task_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.split_once(':').map(|(prefix, _)| prefix),
        Some(
            "app"
                | "branch"
                | "metric"
                | "path"
                | "pr"
                | "project"
                | "sentry"
                | "signal"
                | "target"
                | "ticket"
                | "trigger"
        )
    )
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
        || number.is_empty()
        || !prefix.chars().any(|ch| ch.is_ascii_alphabetic())
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
    let token = token.trim_matches(|ch: char| {
        matches!(ch, ',' | ';' | ':' | '"' | '\'' | ')' | ']' | '}' | '|')
    });
    if token.starts_with("//")
        || token.contains("://")
        || token.contains("](")
        || token.contains('`')
        || token.contains('<')
        || token.contains('>')
    {
        return None;
    }
    if !token.contains('/') || token.len() < 6 {
        return None;
    }
    let token = strip_path_line_suffix(token);
    if !has_path_shape(token) {
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

fn strip_path_line_suffix(token: &str) -> &str {
    let Some((path, _suffix)) = token.split_once(':') else {
        return token;
    };
    if path.rsplit('/').next().is_some_and(has_file_like_basename) {
        path
    } else {
        token
    }
}

fn has_path_shape(token: &str) -> bool {
    if token.starts_with("./") || token.starts_with("../") {
        return true;
    }
    if token.rsplit('/').next().is_some_and(has_file_like_basename) {
        return true;
    }
    let segments = token.split('/').collect::<Vec<_>>();
    let pathish_segments = ["crates", "docs", "src", "test", "tests"];
    if segments.len() < 3 {
        return false;
    }
    segments
        .iter()
        .any(|segment| pathish_segments.contains(&segment.to_ascii_lowercase().as_str()))
}

fn has_file_like_basename(segment: &str) -> bool {
    let segment = segment.trim_matches(trim_task_key_punctuation);
    if matches!(segment, "BUILD" | "BUILD.bazel" | "Makefile" | "justfile") {
        return true;
    }
    let Some((_, extension)) = segment.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "bazel"
            | "go"
            | "gradle"
            | "java"
            | "js"
            | "json"
            | "jsx"
            | "kt"
            | "kts"
            | "md"
            | "proto"
            | "py"
            | "rb"
            | "rs"
            | "scala"
            | "sh"
            | "sql"
            | "toml"
            | "ts"
            | "tsx"
            | "yaml"
            | "yml"
            | "xml"
    )
}

fn target_key(token: &str) -> Option<String> {
    let token =
        token.trim_matches(|ch: char| matches!(ch, ',' | ';' | '"' | '\'' | ')' | ']' | '}'));
    if !token.starts_with("//") || !token.contains(':') || token.contains("://") {
        return None;
    }
    Some(format!("target:{}", token.to_ascii_lowercase()))
}

fn branch_key(token: &str) -> Option<String> {
    let token = token.trim_matches(trim_task_key_punctuation);
    if token.len() < 3
        || token.starts_with('-')
        || !(token.contains('/') || token.contains('-') || token.starts_with("refs/"))
    {
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
    let mut suppressed_block: Option<&'static str> = None;
    for turn in turns {
        let Some(display_text) = &turn.display_text else {
            continue;
        };
        for line in display_text.lines() {
            if let Some(end_tag) = suppressed_block {
                if line.trim_start().starts_with(end_tag) {
                    suppressed_block = None;
                }
                continue;
            }
            if let Some(end_tag) = recall_query_suppressed_block_end(line) {
                suppressed_block = Some(end_tag);
                continue;
            }
            if recall_query_suppressed_line(line) {
                continue;
            }
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

pub fn build_active_segment_recall_query(
    turns: &[TurnRecord],
    max_chars: usize,
    tool_output_truncation_chars: usize,
) -> String {
    build_recall_query(
        active_segment_recall_turns(turns),
        max_chars,
        tool_output_truncation_chars,
    )
}

pub fn active_segment_recall_turns(turns: &[TurnRecord]) -> &[TurnRecord] {
    const FALLBACK_SUFFIX_TURNS: usize = 3;
    if turns.is_empty() {
        return turns;
    }

    let Some(seed_index) = (0..turns.len())
        .rev()
        .find(|index| !turn_segment_keys(&turns[*index]).is_empty())
    else {
        return &turns[turns.len().saturating_sub(FALLBACK_SUFFIX_TURNS)..];
    };

    let mut active_keys = turn_segment_keys(&turns[seed_index]);
    let mut start = seed_index;
    for index in (0..seed_index).rev() {
        let keys = turn_segment_keys(&turns[index]);
        if keys.is_empty() {
            start = index;
            continue;
        }
        if keys.iter().any(|key| active_keys.contains(key)) {
            active_keys.extend(keys);
            start = index;
        } else {
            break;
        }
    }
    &turns[start..]
}

fn turn_segment_keys(turn: &TurnRecord) -> HashSet<String> {
    turn.display_text
        .as_deref()
        .map(segment_task_keys)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub fn segment_task_keys(text: &str) -> Vec<String> {
    extract_task_keys(text)
        .into_iter()
        .filter(|key| is_strong_task_key(key))
        .collect()
}

fn recall_query_suppressed_block_end(line: &str) -> Option<&'static str> {
    let line = line.trim_start();
    if line.starts_with("<codex_internal_context") {
        Some("</codex_internal_context>")
    } else if line.starts_with("<environment_context>") {
        Some("</environment_context>")
    } else {
        None
    }
}

fn recall_query_suppressed_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("Working (")
        || line.starts_with("Thinking (")
        || line.starts_with("Continue working toward the active thread goal.")
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
    use crate::infer_context_from_text;

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
            "PR 481245 updates riskarbiter/src/main/java/Foo.java:42:public and //riskarbiter/src/test:unit for MLP-4400; run yaaml recall",
        );

        assert!(keys.contains(&"pr:481245".to_string()));
        assert!(keys.contains(&"ticket:MLP-4400".to_string()));
        assert!(keys.contains(&"path:riskarbiter/src/main/java/foo.java".to_string()));
        assert!(keys.contains(&"target://riskarbiter/src/test:unit".to_string()));
        assert!(!keys.contains(&"path://riskarbiter/src/test:unit".to_string()));
        assert!(keys.contains(&"tool:yaaml".to_string()));
    }

    #[test]
    fn task_key_extraction_ignores_generic_slash_phrases() {
        let keys = extract_task_keys(
            "compare before/after, repair/validation notes, restarts/crashloop/progress, create/fork/transfer, store/domain, </environment_context>, /objective, and ~/development",
        );

        assert!(!keys.contains(&"path:before/after".to_string()));
        assert!(!keys.contains(&"path:repair/validation".to_string()));
        assert!(!keys.contains(&"path:restarts/crashloop/progress".to_string()));
        assert!(!keys.contains(&"path:create/fork/transfer".to_string()));
        assert!(!keys.contains(&"path:store/domain".to_string()));
        assert!(!keys.contains(&"path:/environment_context".to_string()));
        assert!(!keys.contains(&"path:/objective".to_string()));
        assert!(!keys.contains(&"path:~/development".to_string()));
    }

    #[test]
    fn task_key_extraction_ignores_generic_branch_words() {
        let keys =
            extract_task_keys("branch state is clean but branch refs/heads/rust-impl matters");

        assert!(!keys.contains(&"branch:state".to_string()));
        assert!(!keys.contains(&"branch:clean".to_string()));
        assert!(keys.contains(&"branch:refs/heads/rust-impl".to_string()));
    }

    #[test]
    fn task_key_extraction_rejects_malformed_tickets() {
        let keys = extract_task_keys("items 477- and 123-456 are not ticket keys");

        assert!(!keys.contains(&"ticket:477-".to_string()));
        assert!(!keys.contains(&"ticket:123-456".to_string()));
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

    #[test]
    fn selection_drops_task_state_without_matching_task_key() {
        let current_project = "/Users/tbedor/Development/java";
        let hits = vec![
            VectorHit {
                memory_id: 1,
                similarity: 0.95,
            },
            VectorHit {
                memory_id: 2,
                similarity: 0.80,
            },
            VectorHit {
                memory_id: 3,
                similarity: 0.78,
            },
        ];
        let memories = vec![
            memory(
                1,
                "Abandoned PR state",
                MemoryKind::TaskState,
                Some(current_project),
                vec!["pr:111111".to_string()],
            ),
            memory(
                2,
                "Target PR state",
                MemoryKind::TaskState,
                Some(current_project),
                vec!["pr:481245".to_string()],
            ),
            memory(
                3,
                "Reusable Java workflow",
                MemoryKind::Workflow,
                Some(current_project),
                Vec::new(),
            ),
        ];
        let ranked = rank_recall_candidates(
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
        let (selected, debug) = select_recall_candidates(
            ranked,
            &memories,
            current_project,
            &ContextMetadata::default(),
            &["pr:481245".to_string()],
            5,
        );

        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.memory_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let dropped = debug
            .iter()
            .find(|candidate| candidate.memory_id == 1)
            .unwrap();
        assert!(dropped
            .rank
            .filter_reasons
            .contains(&"drop:task_state_without_task_key_match".to_string()));
    }

    #[test]
    fn tool_key_match_alone_does_not_keep_task_state() {
        let current_project = "/Users/tbedor/Development/yaaml";
        let hits = vec![VectorHit {
            memory_id: 1,
            similarity: 0.95,
        }];
        let memories = vec![memory(
            1,
            "Old YAAML status",
            MemoryKind::TaskState,
            Some(current_project),
            vec!["tool:yaaml".to_string()],
        )];
        let ranked = rank_recall_candidates(
            &hits,
            &memories,
            current_project,
            &ContextMetadata::default(),
            &["tool:yaaml".to_string()],
            RecallRankingOptions {
                project_tiebreaker: true,
                project_score_bonus: 0.05,
            },
        );
        assert!(ranked[0].rank.task_key_bonus > 0.0);

        let (selected, debug) = select_recall_candidates(
            ranked,
            &memories,
            current_project,
            &ContextMetadata::default(),
            &["tool:yaaml".to_string()],
            5,
        );

        assert!(selected.is_empty());
        assert!(debug[0]
            .rank
            .filter_reasons
            .contains(&"drop:task_state_without_strong_task_key_match".to_string()));
    }

    #[test]
    fn semantic_context_alone_does_not_keep_task_state() {
        let current_project = "/Users/tbedor/Development/java";
        let hits = vec![VectorHit {
            memory_id: 1,
            similarity: 0.95,
        }];
        let memories = vec![MemoryRecord {
            body: "RiskArbiter PR #483111 is ready for final review after rule archival changes."
                .to_string(),
            task_keys: vec!["pr:483111".to_string()],
            project_descriptor: Some("java riskarbiter".to_string()),
            ..memory(
                1,
                "Old RiskArbiter PR state",
                MemoryKind::TaskState,
                Some(current_project),
                Vec::new(),
            )
        }];
        let query_context = infer_context_from_text(
            "RiskArbiter JVM startup troubleshooting in squareup/java with cloud profiler errors",
        );
        let ranked = rank_recall_candidates(
            &hits,
            &memories,
            current_project,
            &query_context,
            &[],
            RecallRankingOptions {
                project_tiebreaker: true,
                project_score_bonus: 0.05,
            },
        );
        assert!(ranked[0]
            .rank
            .penalties
            .contains(&"task_state_without_task_key_overlap".to_string()));

        let (selected, debug) =
            select_recall_candidates(ranked, &memories, current_project, &query_context, &[], 5);

        assert!(selected.is_empty());
        assert!(debug[0]
            .rank
            .filter_reasons
            .contains(&"drop:stale_task_state_semantic_context_only".to_string()));
    }

    #[test]
    fn global_durable_with_negative_context_and_no_task_match_abstains() {
        let current_project = "/Users/tbedor/Development/java";
        let hits = vec![VectorHit {
            memory_id: 1,
            similarity: 0.95,
        }];
        let memories = vec![MemoryRecord {
            scope: MemoryScope::Global,
            body: "Use Cloud CD retrigger deploy for stuck canary cleanup.".to_string(),
            project_id: None,
            project_descriptor: Some("cloud-cd".to_string()),
            ..memory(
                1,
                "Cloud CD canary cleanup",
                MemoryKind::Workflow,
                None,
                Vec::new(),
            )
        }];
        let query_context =
            infer_context_from_text("RiskArbiter JVM startup troubleshooting and test timeout");
        let ranked = rank_recall_candidates(
            &hits,
            &memories,
            current_project,
            &query_context,
            &[],
            RecallRankingOptions {
                project_tiebreaker: true,
                project_score_bonus: 0.05,
            },
        );
        let (selected, debug) =
            select_recall_candidates(ranked, &memories, current_project, &query_context, &[], 5);

        assert!(selected.is_empty());
        assert!(debug[0]
            .rank
            .filter_reasons
            .contains(&"drop:global_durable_wrong_context".to_string()));
    }

    #[test]
    fn active_segment_query_uses_latest_contiguous_strong_task_keys() {
        let turns = vec![
            turn(1, "user: finish PR #483111 for MLP-4410 rule archival"),
            turn(2, "assistant: final review fixes for PR #483111 are done"),
            turn(
                3,
                "user: now debug PR #483601 timeout in riskarbiter/src/test/java/FooTest.java",
            ),
            turn(
                4,
                "assistant: update riskarbiter/src/test/java/FooTest.java and run bin/buildifier",
            ),
            turn(5, "assistant: tests are still timing out; checking logs"),
        ];

        let active = active_segment_recall_turns(&turns);
        let query = build_active_segment_recall_query(&turns, 4_000, 80);

        assert_eq!(
            active.iter().map(|turn| turn.ordinal).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(!query.contains("PR #483111"));
        assert!(query.contains("PR #483601"));
        assert!(query.contains("FooTest.java"));
    }

    fn empty_rank() -> RecallRankDetails {
        RecallRankDetails {
            vector_score: 0.0,
            context_score: 0.0,
            project_bonus: 0.0,
            task_key_bonus: 0.0,
            global_durable_bonus: 0.0,
            penalties: Vec::new(),
            filter_reasons: Vec::new(),
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

    fn turn(ordinal: u64, text: &str) -> TurnRecord {
        TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal,
            byte_end: ordinal + 1,
            observed_at: None,
            status: TurnStatus::Completed,
            display_text: Some(text.to_string()),
            cwd: None,
            context: None,
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
    fn recall_query_strips_codex_internal_context_blocks() {
        let turn = TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 0,
            byte_end: 1,
            observed_at: None,
            status: TurnStatus::Completed,
            display_text: Some(
                "user: investigate recall\n<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n<objective>unrelated prior objective</objective>\n</codex_internal_context>\nassistant: checking production code".to_string(),
            ),
            cwd: None,
            context: None,
        };

        let query = build_recall_query(&[turn], 2_000, 80);

        assert!(query.contains("user: investigate recall"));
        assert!(query.contains("assistant: checking production code"));
        assert!(!query.contains("unrelated prior objective"));
        assert!(!query.contains("codex_internal_context"));
    }

    #[test]
    fn recall_query_strips_environment_context_blocks_and_progress_lines() {
        let turn = TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            ordinal: 0,
            byte_start: 0,
            byte_end: 1,
            observed_at: None,
            status: TurnStatus::Completed,
            display_text: Some(
                "user: fix recall\n<environment_context>\n<cwd>/tmp/wrong</cwd>\n</environment_context>\nWorking (15s - esc to interrupt)\nassistant: done".to_string(),
            ),
            cwd: None,
            context: None,
        };

        let query = build_recall_query(&[turn], 2_000, 80);

        assert_eq!(query, "user: fix recall\nassistant: done");
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
