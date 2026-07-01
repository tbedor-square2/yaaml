use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use yaaml_core::{
    select_recall_candidates_for_segment, Config, ContextMetadata, MemoryRecord, RecallCandidate,
    TurnRecord,
};

use crate::llm_judge::JudgeClient;

pub const RECALL_DYNAMIC_SELECTION_LIMIT: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallFilterTelemetry {
    pub candidate_count: usize,
    pub deterministic_selected_count: usize,
    pub final_selected_count: usize,
    #[serde(default)]
    pub recent_recall_candidate_count: usize,
    #[serde(default)]
    pub cooldown_suppressed_count: usize,
    #[serde(default)]
    pub cooldown_window_seconds: u64,
    #[serde(default)]
    pub cooldown_since_unix: Option<i64>,
    pub llm_attempted: bool,
    pub llm_applied: bool,
    pub llm_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecallFilterResult {
    pub selected: Vec<RecallCandidate>,
    pub debug_candidates: Vec<RecallCandidate>,
    pub telemetry: RecallFilterTelemetry,
}

#[derive(Debug, Clone, Copy)]
pub struct RecallFilterRequest<'a> {
    pub current_project_id: &'a str,
    pub query_text: &'a str,
    pub query_context: &'a ContextMetadata,
    pub query_task_keys: &'a [String],
    pub current_segment_id: Option<i64>,
}

pub fn select_recall_candidates_with_llm_filter(
    config: &Config,
    candidates: Vec<RecallCandidate>,
    memories: &[MemoryRecord],
    request: RecallFilterRequest<'_>,
) -> RecallFilterResult {
    let selection_limit = effective_recall_selection_limit(config);
    let filter_limit = config
        .recall_llm_filter_candidate_limit
        .max(selection_limit);
    let (filter_pool, mut debug_candidates) = select_recall_candidates_for_segment(
        candidates,
        memories,
        request.current_project_id,
        request.query_context,
        request.query_task_keys,
        request.current_segment_id,
        filter_limit,
    );
    let deterministic_selected =
        strict_kind_diverse_top_fallback_selection(&filter_pool, memories, selection_limit);
    annotate_deterministic_selection(&mut debug_candidates, &deterministic_selected);
    let mut telemetry = RecallFilterTelemetry {
        candidate_count: filter_pool.len(),
        deterministic_selected_count: deterministic_selected.len(),
        final_selected_count: deterministic_selected.len(),
        recent_recall_candidate_count: 0,
        cooldown_suppressed_count: 0,
        cooldown_window_seconds: config.recall_memory_cooldown_seconds,
        cooldown_since_unix: None,
        llm_attempted: false,
        llm_applied: false,
        llm_error: None,
    };

    let Some(client) = recall_filter_client(config) else {
        return RecallFilterResult {
            selected: deterministic_selected,
            debug_candidates,
            telemetry,
        };
    };
    telemetry.llm_attempted = true;

    match run_llm_filter(
        &client,
        config,
        request.query_text,
        request.query_context,
        request.current_project_id,
        memories,
        &filter_pool,
    ) {
        Ok(selected_ids) => {
            let selected_id_set = selected_ids.into_iter().collect::<HashSet<_>>();
            annotate_llm_filter(&mut debug_candidates, &filter_pool, &selected_id_set);
            let selected = filter_pool
                .iter()
                .filter(|candidate| selected_id_set.contains(&candidate.memory_id))
                .take(selection_limit)
                .cloned()
                .collect::<Vec<_>>();
            telemetry.final_selected_count = selected.len();
            telemetry.llm_applied = true;
            RecallFilterResult {
                selected,
                debug_candidates,
                telemetry,
            }
        }
        Err(error) => {
            telemetry.llm_error = Some(error.to_string());
            RecallFilterResult {
                selected: deterministic_selected,
                debug_candidates,
                telemetry,
            }
        }
    }
}

pub fn effective_recall_selection_limit(config: &Config) -> usize {
    config
        .recall_result_limit
        .min(RECALL_DYNAMIC_SELECTION_LIMIT)
}

pub fn suppress_recently_recalled_candidates(
    selected: Vec<RecallCandidate>,
    debug_candidates: &mut [RecallCandidate],
    recent_memory_ids: &HashSet<i64>,
) -> Vec<RecallCandidate> {
    if recent_memory_ids.is_empty() {
        return selected;
    }
    let mut suppressed = HashSet::new();
    let kept = selected
        .into_iter()
        .filter(|candidate| {
            if recent_memory_ids.contains(&candidate.memory_id) {
                suppressed.insert(candidate.memory_id);
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    for candidate in debug_candidates {
        if suppressed.contains(&candidate.memory_id) {
            candidate
                .rank
                .filter_reasons
                .push("drop:recent_recall_cooldown".to_string());
        }
    }
    kept
}

pub fn suppress_source_overlapping_candidates(
    selected: Vec<RecallCandidate>,
    debug_candidates: &mut [RecallCandidate],
    memories: &[MemoryRecord],
    query_turns: &[TurnRecord],
) -> Vec<RecallCandidate> {
    let query_turn_keys = query_turns
        .iter()
        .map(|turn| (turn.session_id.clone(), turn.ordinal))
        .collect::<HashSet<_>>();
    if query_turn_keys.is_empty() {
        return selected;
    }
    let suppressed = memories
        .iter()
        .filter(|memory| {
            memory.source_turn_refs.iter().any(|source_ref| {
                query_turn_keys.contains(&(source_ref.session_id.clone(), source_ref.ordinal))
            })
        })
        .filter_map(|memory| memory.id)
        .collect::<HashSet<_>>();
    if suppressed.is_empty() {
        return selected;
    }
    for candidate in debug_candidates {
        if suppressed.contains(&candidate.memory_id) {
            candidate
                .rank
                .filter_reasons
                .push("drop:source_turn_already_in_query".to_string());
        }
    }
    selected
        .into_iter()
        .filter(|candidate| !suppressed.contains(&candidate.memory_id))
        .collect()
}

fn strict_kind_diverse_top_fallback_selection(
    candidates: &[RecallCandidate],
    memories: &[MemoryRecord],
    limit: usize,
) -> Vec<RecallCandidate> {
    let Some(top_candidate) = candidates.first() else {
        return Vec::new();
    };
    let active_segment_task_states = candidates
        .iter()
        .filter(|candidate| is_active_segment_task_state(candidate))
        .collect::<Vec<_>>();
    if top_candidate.score < 0.75 && active_segment_task_states.is_empty() {
        return Vec::new();
    }

    let mut selected = Vec::new();
    for candidate in active_segment_task_states {
        selected.push(candidate.clone());
        if selected.len() == limit {
            return selected;
        }
    }

    let mut seen_kinds = HashSet::new();
    for candidate in &selected {
        seen_kinds.insert(memory_kind(candidate, memories));
    }
    let selected_ids = selected
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if selected_ids.contains(&candidate.memory_id) {
            continue;
        }
        let kind = memory_kind(candidate, memories);
        let keep = (top_candidate_allowed(index, candidate, &kind)
            || has_task_match(candidate)
            || (!seen_kinds.contains(&kind) && has_positive_health_signal(candidate)))
            && candidate.score >= 0.90;
        if keep {
            selected.push(candidate.clone());
            seen_kinds.insert(kind);
        }
        if selected.len() == limit {
            break;
        }
    }

    selected
}

fn is_active_segment_task_state(candidate: &RecallCandidate) -> bool {
    candidate
        .rank
        .filter_reasons
        .iter()
        .any(|reason| reason == "keep:task_state_same_active_segment")
}

fn has_task_match(candidate: &RecallCandidate) -> bool {
    candidate
        .rank
        .filter_reasons
        .iter()
        .any(|reason| reason == "keep:strong_task_key_match")
        || candidate
            .rank
            .matched_task_keys
            .iter()
            .any(|key| is_specific_task_match_key(key))
}

fn is_specific_task_match_key(key: &str) -> bool {
    !key.starts_with("label:") && !key.starts_with("topic:") && !key.starts_with("tool:")
}

fn has_positive_health_signal(candidate: &RecallCandidate) -> bool {
    candidate
        .rank
        .penalties
        .iter()
        .any(|penalty| penalty.contains(":proven_useful:"))
}

fn top_candidate_allowed(index: usize, candidate: &RecallCandidate, kind: &str) -> bool {
    index == 0 && !risky_unproven_procedural_candidate(candidate, kind)
}

fn risky_unproven_procedural_candidate(candidate: &RecallCandidate, kind: &str) -> bool {
    matches!(kind, "lesson" | "workflow")
        && !has_task_match(candidate)
        && !has_positive_health_signal(candidate)
        && candidate.score < 1.50
}

fn memory_kind(candidate: &RecallCandidate, memories: &[MemoryRecord]) -> String {
    memories
        .iter()
        .find(|memory| memory.id == Some(candidate.memory_id))
        .map(|memory| memory.kind.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn annotate_deterministic_selection(
    debug_candidates: &mut [RecallCandidate],
    selected: &[RecallCandidate],
) {
    let selected_ids = selected
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<HashSet<_>>();
    for candidate in debug_candidates {
        if selected_ids.contains(&candidate.memory_id) {
            candidate
                .rank
                .filter_reasons
                .push("keep:strict_kind_diverse".to_string());
        } else if candidate.rank.filter_reasons.iter().any(|reason| {
            reason.starts_with("keep:") && !matches!(reason.as_str(), "keep:strict_kind_diverse")
        }) {
            candidate
                .rank
                .filter_reasons
                .push("drop:strict_kind_diverse".to_string());
        }
    }
}

fn recall_filter_client(config: &Config) -> Option<JudgeClient> {
    JudgeClient::from_config(config, !config.recall_llm_filter_enabled)
}

fn run_llm_filter(
    client: &JudgeClient,
    config: &Config,
    query_text: &str,
    query_context: &ContextMetadata,
    current_project_id: &str,
    memories: &[MemoryRecord],
    candidates: &[RecallCandidate],
) -> Result<Vec<i64>, yaaml_llm::ProviderError> {
    let prompt = recall_filter_prompt(
        config,
        query_text,
        query_context,
        current_project_id,
        memories,
        candidates,
    );
    let value = client.structured_json(recall_filter_system_prompt(), &prompt)?;
    Ok(parse_selected_memory_ids(&value, candidates))
}

fn recall_filter_system_prompt() -> &'static str {
    "You filter memory recall for a coding agent. Return compact JSON only. Treat the deterministic ranking as a useful baseline and prune memories that are unrelated, stale, redundant, or too generic to justify context cost. Select 0-2 memories; empty recall is better than bad recall when no candidate is clearly useful. Do not invent memory ids."
}

fn recall_filter_prompt(
    config: &Config,
    query_text: &str,
    query_context: &ContextMetadata,
    current_project_id: &str,
    memories: &[MemoryRecord],
    candidates: &[RecallCandidate],
) -> String {
    const PROMPT_ATTEMPTS: &[(usize, usize)] = &[
        (6000, 1200),
        (4500, 800),
        (3000, 500),
        (2000, 300),
        (1200, 160),
        (800, 80),
        (400, 40),
    ];

    let budget = config.recall_llm_filter_prompt_max_chars;
    let mut shortest_prompt = String::new();
    for (turn_max_chars, body_max_chars) in PROMPT_ATTEMPTS {
        let prompt = build_recall_filter_prompt(
            config,
            query_text,
            query_context,
            current_project_id,
            memories,
            candidates,
            *turn_max_chars,
            *body_max_chars,
        );
        if shortest_prompt.is_empty() || prompt.chars().count() < shortest_prompt.chars().count() {
            shortest_prompt = prompt.clone();
        }
        if prompt.chars().count() <= budget {
            return prompt;
        }
    }
    let compact_prompt = build_compact_recall_filter_prompt(
        config,
        query_text,
        current_project_id,
        memories,
        candidates,
        240,
    );
    if compact_prompt.chars().count() < shortest_prompt.chars().count() {
        return compact_prompt;
    }
    shortest_prompt
}

#[allow(clippy::too_many_arguments)]
fn build_recall_filter_prompt(
    config: &Config,
    query_text: &str,
    query_context: &ContextMetadata,
    current_project_id: &str,
    memories: &[MemoryRecord],
    candidates: &[RecallCandidate],
    turn_max_chars: usize,
    body_max_chars: usize,
) -> String {
    let candidates_json = candidates
        .iter()
        .filter_map(|candidate| {
            memories
                .iter()
                .find(|memory| memory.id == Some(candidate.memory_id))
                .map(|memory| {
                    json!({
                        "memory_id": candidate.memory_id,
                        "title": memory.title,
                        "body": truncate_chars(&memory.body, body_max_chars),
                        "kind": memory.kind.as_str(),
                        "scope": memory.scope.as_str(),
                        "project_id": memory.project_id,
                        "project_descriptor": memory.project_descriptor,
                        "task_keys": memory.task_keys,
                        "score": candidate.score,
                        "filter_reasons": candidate.rank.filter_reasons,
                    })
                })
        })
        .collect::<Vec<_>>();
    json!({
        "current_project_id": current_project_id,
        "query_context": query_context,
        "current_turn_text": truncate_chars(query_text, turn_max_chars),
        "max_selected_memories": effective_recall_selection_limit(config),
        "selection_policy": "Select at most two concise, useful memories. Prefer abstaining over adding weak or redundant context.",
        "candidate_memories": candidates_json,
        "response_schema": {
            "selected_memory_ids": ["integer memory ids to keep, in candidate order or fewer"],
            "memory_scores": [{"memory_id": "integer", "score": "1-5 relevance score"}]
        }
    })
    .to_string()
}

fn build_compact_recall_filter_prompt(
    config: &Config,
    query_text: &str,
    current_project_id: &str,
    memories: &[MemoryRecord],
    candidates: &[RecallCandidate],
    turn_max_chars: usize,
) -> String {
    let candidates_json = candidates
        .iter()
        .filter_map(|candidate| {
            memories
                .iter()
                .find(|memory| memory.id == Some(candidate.memory_id))
                .map(|memory| {
                    json!({
                        "memory_id": candidate.memory_id,
                        "title": truncate_chars(&memory.title, 80),
                        "kind": memory.kind.as_str(),
                        "scope": memory.scope.as_str(),
                        "project_id": memory.project_id,
                        "score": candidate.score,
                    })
                })
        })
        .collect::<Vec<_>>();
    json!({
        "current_project_id": current_project_id,
        "current_turn_text": truncate_chars(query_text, turn_max_chars),
        "max_selected_memories": effective_recall_selection_limit(config),
        "selection_policy": "Keep at most two candidate memories. Empty recall is acceptable when candidates are weak, redundant, or unrelated.",
        "candidate_memories": candidates_json,
        "response_schema": {"selected_memory_ids": ["integer memory ids"]}
    })
    .to_string()
}

fn parse_selected_memory_ids(value: &Value, candidates: &[RecallCandidate]) -> Vec<i64> {
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<HashSet<_>>();
    let mut ids = Vec::new();
    for key in [
        "selected_memory_ids",
        "memory_ids",
        "selected_memories",
        "selected",
    ] {
        collect_selected_ids(value.get(key), &mut ids);
    }
    if ids.is_empty() {
        ids = parse_scored_memory_ids(value);
    }
    candidate_ordered_ids(&ids, candidates, &candidate_ids)
}

fn collect_selected_ids(value: Option<&Value>, ids: &mut Vec<i64>) {
    match value {
        Some(Value::Array(values)) => {
            for value in values {
                collect_selected_ids(Some(value), ids);
            }
        }
        Some(Value::Object(map)) => {
            if let Some(id) = value_to_memory_id(map.get("memory_id").or_else(|| map.get("id"))) {
                ids.push(id);
            }
        }
        Some(value) => {
            if let Some(id) = value_to_memory_id(Some(value)) {
                ids.push(id);
            }
        }
        None => {}
    }
}

fn value_to_memory_id(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
    })
}

fn parse_scored_memory_ids(value: &Value) -> Vec<i64> {
    let scored = ["memory_scores", "scores", "candidate_scores", "candidates"]
        .iter()
        .filter_map(|key| value.get(key).and_then(Value::as_array))
        .flatten()
        .filter_map(|value| {
            let object = value.as_object()?;
            let id = value_to_memory_id(object.get("memory_id").or_else(|| object.get("id")))?;
            let score = object
                .get("relevance_score")
                .or_else(|| object.get("score"))
                .or_else(|| object.get("rating"))
                .and_then(value_to_f32)?;
            Some((id, score))
        })
        .collect::<Vec<_>>();

    let strong = scored
        .iter()
        .filter(|(_, score)| *score >= 4.0)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if !strong.is_empty() {
        return strong;
    }
    scored
        .into_iter()
        .filter(|(_, score)| *score >= 3.0)
        .map(|(id, _)| id)
        .take(3)
        .collect()
}

fn value_to_f32(value: &Value) -> Option<f32> {
    value
        .as_f64()
        .map(|number| number as f32)
        .or_else(|| value.as_str().and_then(|text| text.parse::<f32>().ok()))
}

fn candidate_ordered_ids(
    ids: &[i64],
    candidates: &[RecallCandidate],
    candidate_ids: &HashSet<i64>,
) -> Vec<i64> {
    let selected_ids = ids
        .iter()
        .filter(|id| candidate_ids.contains(id))
        .copied()
        .collect::<HashSet<_>>();
    candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .filter(|id| selected_ids.contains(id))
        .collect()
}

fn annotate_llm_filter(
    debug_candidates: &mut [RecallCandidate],
    filter_pool: &[RecallCandidate],
    selected_id_set: &HashSet<i64>,
) {
    let filter_pool_ids = filter_pool
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<HashSet<_>>();
    for candidate in debug_candidates {
        if !filter_pool_ids.contains(&candidate.memory_id) {
            continue;
        }
        if selected_id_set.contains(&candidate.memory_id) {
            candidate
                .rank
                .filter_reasons
                .push("keep:llm_semantic_filter".to_string());
        } else {
            candidate
                .rank
                .filter_reasons
                .push("drop:llm_semantic_filter".to_string());
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yaaml_core::{MemoryKind, MemoryScope};

    use super::*;

    #[test]
    fn parses_selected_memory_ids_from_numbers_and_strings() {
        let candidates = vec![candidate(1), candidate(2), candidate(3)];
        let value = json!({"selected_memory_ids": [2, "3", 9, "bad", 2]});

        assert_eq!(parse_selected_memory_ids(&value, &candidates), vec![2, 3]);
    }

    #[test]
    fn parses_selected_memory_ids_from_object_arrays_and_aliases() {
        let candidates = vec![candidate(1), candidate(2), candidate(3), candidate(4)];

        assert_eq!(
            parse_selected_memory_ids(
                &json!({"selected_memories": [{"memory_id": "3"}, {"id": 1}, {"id": 99}]}),
                &candidates,
            ),
            vec![1, 3]
        );
        assert_eq!(
            parse_selected_memory_ids(&json!({"memory_ids": ["4", 2, 2]}), &candidates),
            vec![2, 4]
        );
        assert_eq!(
            parse_selected_memory_ids(&json!({"selected": [{"id": "2"}]}), &candidates),
            vec![2]
        );
    }

    #[test]
    fn parses_scored_memory_ids_when_selected_ids_are_absent() {
        let candidates = vec![candidate(1), candidate(2), candidate(3), candidate(4)];
        let value = json!({
            "memory_scores": [
                {"memory_id": 3, "score": 4},
                {"memory_id": 1, "relevance_score": "5"},
                {"memory_id": 4, "score": 2},
                {"memory_id": 99, "score": 5}
            ]
        });

        assert_eq!(parse_selected_memory_ids(&value, &candidates), vec![1, 3]);
    }

    #[test]
    fn scored_memory_ids_keep_medium_scores_when_no_strong_scores_exist() {
        let candidates = vec![candidate(1), candidate(2), candidate(3), candidate(4)];
        let value = json!({
            "scores": [
                {"id": 4, "rating": "3"},
                {"id": 2, "score": 3.5},
                {"id": 1, "score": 2}
            ]
        });

        assert_eq!(parse_selected_memory_ids(&value, &candidates), vec![2, 4]);
    }

    #[test]
    fn strict_kind_diverse_selection_abstains_when_top_score_is_weak() {
        let candidates = vec![candidate_with_score(1, 0.74), candidate_with_score(2, 1.20)];
        let memories = vec![memory(1, "body"), memory(2, "body")];

        assert!(strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3).is_empty());
    }

    #[test]
    fn strict_kind_diverse_selection_abstains_below_threshold() {
        let candidates = vec![candidate_with_score(1, 0.89), candidate_with_score(2, 0.88)];
        let mut memories = vec![memory(1, "body"), memory(2, "body")];
        memories[0].kind = MemoryKind::ProjectFact;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert!(selected.is_empty());
    }

    #[test]
    fn strict_kind_diverse_selection_abstains_on_unproven_workflow_without_task_match() {
        let candidates = vec![candidate_with_score(1, 1.20), candidate_with_score(2, 1.10)];
        let mut memories = vec![memory(1, "body"), memory(2, "body")];
        memories[0].kind = MemoryKind::Workflow;
        memories[1].kind = MemoryKind::ProjectFact;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert!(selected.is_empty());
    }

    #[test]
    fn strict_kind_diverse_selection_does_not_treat_topic_only_match_as_task_match() {
        let mut candidates = vec![candidate_with_score(1, 1.20), candidate_with_score(2, 1.10)];
        candidates[0]
            .rank
            .matched_task_keys
            .push("topic:segment".to_string());
        candidates[1]
            .rank
            .matched_task_keys
            .push("topic:task-state".to_string());
        let mut memories = vec![memory(1, "body"), memory(2, "body")];
        memories[0].kind = MemoryKind::Workflow;
        memories[1].kind = MemoryKind::Lesson;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3);

        assert!(selected.is_empty());
    }

    #[test]
    fn strict_kind_diverse_selection_limits_repeated_kinds() {
        let candidates = vec![
            candidate_with_score(1, 1.20),
            candidate_with_score(2, 1.10),
            proven_useful_candidate(3, 1.00),
        ];
        let mut memories = vec![memory(1, "body"), memory(2, "body"), memory(3, "body")];
        memories[0].kind = MemoryKind::ProjectFact;
        memories[1].kind = MemoryKind::ProjectFact;
        memories[2].kind = MemoryKind::Workflow;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert_eq!(selected, vec![1, 3]);
    }

    #[test]
    fn strict_kind_diverse_selection_does_not_fill_with_unproven_no_task_memory() {
        let candidates = vec![
            candidate_with_score(1, 1.20),
            candidate_with_score(2, 1.15),
            candidate_with_score(3, 1.10),
        ];
        let mut memories = vec![memory(1, "body"), memory(2, "body"), memory(3, "body")];
        memories[0].kind = MemoryKind::ProjectFact;
        memories[1].kind = MemoryKind::Workflow;
        memories[2].kind = MemoryKind::ProjectFact;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert_eq!(selected, vec![1]);
    }

    #[test]
    fn strict_kind_diverse_selection_keeps_task_matches_across_same_kind() {
        let mut candidates = vec![
            candidate_with_score(1, 1.20),
            candidate_with_score(2, 0.95),
            candidate_with_score(3, 0.94),
        ];
        candidates[1]
            .rank
            .matched_task_keys
            .push("path:src/lib.rs".to_string());
        let mut memories = vec![memory(1, "body"), memory(2, "body"), memory(3, "body")];
        memories[0].kind = MemoryKind::ProjectFact;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 3)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert_eq!(selected, vec![1, 2]);
    }

    #[test]
    fn strict_kind_diverse_selection_prioritizes_active_segment_task_state() {
        let mut candidates = vec![
            candidate_with_score(1, 1.20),
            candidate_with_score(2, 0.40),
            candidate_with_score(3, 1.10),
        ];
        candidates[1]
            .rank
            .filter_reasons
            .push("keep:task_state_same_active_segment".to_string());
        let mut memories = vec![memory(1, "body"), memory(2, "body"), memory(3, "body")];
        memories[0].kind = MemoryKind::TaskCheckpoint;
        memories[1].kind = MemoryKind::TaskState;
        memories[2].kind = MemoryKind::Workflow;

        let selected = strict_kind_diverse_top_fallback_selection(&candidates, &memories, 2)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert_eq!(selected, vec![2, 1]);
    }

    #[test]
    fn deterministic_selection_caps_to_top_two_even_when_config_limit_is_higher() {
        let config = Config {
            recall_result_limit: 5,
            ..Config::default()
        };
        let mut candidates = vec![
            candidate_with_score(1, 1.20),
            proven_useful_candidate(2, 1.10),
            candidate_with_score(3, 1.00),
        ];
        candidates[0]
            .rank
            .matched_task_keys
            .push("path:src/lib.rs".to_string());
        let mut memories = vec![memory(1, "body"), memory(2, "body"), memory(3, "body")];
        memories[0].kind = MemoryKind::ProjectFact;
        memories[1].kind = MemoryKind::Workflow;
        memories[2].kind = MemoryKind::Preference;
        let query_context = ContextMetadata {
            subject_tags: vec!["yaaml".to_string()],
            ..ContextMetadata::default()
        };

        let selected = select_recall_candidates_with_llm_filter(
            &config,
            candidates,
            &memories,
            RecallFilterRequest {
                current_project_id: "/tmp/yaaml",
                query_text: "query",
                query_context: &query_context,
                query_task_keys: &["path:src/lib.rs".to_string()],
                current_segment_id: None,
            },
        )
        .selected
        .into_iter()
        .map(|candidate| candidate.memory_id)
        .collect::<Vec<_>>();

        assert_eq!(selected, vec![1, 2]);
    }

    #[test]
    fn recent_recall_cooldown_suppresses_selected_memories() {
        let selected = vec![candidate_with_score(1, 1.20), candidate_with_score(2, 1.10)];
        let mut debug_candidates = selected.clone();
        let recent = HashSet::from([1]);

        let kept = suppress_recently_recalled_candidates(selected, &mut debug_candidates, &recent)
            .into_iter()
            .map(|candidate| candidate.memory_id)
            .collect::<Vec<_>>();

        assert_eq!(kept, vec![2]);
        assert!(debug_candidates[0]
            .rank
            .filter_reasons
            .contains(&"drop:recent_recall_cooldown".to_string()));
    }

    #[test]
    fn source_overlap_suppresses_selected_memories() {
        let selected = vec![candidate_with_score(1, 1.20), candidate_with_score(2, 1.10)];
        let mut debug_candidates = selected.clone();
        let mut memories = vec![memory(1, "overlap"), memory(2, "older")];
        memories[0].source_turn_refs = vec![yaaml_core::SourceTurnRef {
            session_id: "session-1".to_string(),
            ordinal: 7,
            byte_start: 0,
            byte_end: 1,
        }];
        memories[1].source_turn_refs = vec![yaaml_core::SourceTurnRef {
            session_id: "session-1".to_string(),
            ordinal: 3,
            byte_start: 0,
            byte_end: 1,
        }];
        let query_turns = vec![TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some("turn-7".to_string()),
            ordinal: 7,
            byte_start: 0,
            byte_end: 1,
            observed_at: Some("unix:7".to_string()),
            status: yaaml_core::TurnStatus::Completed,
            display_text: Some("turn text".to_string()),
            cwd: None,
            context: None,
        }];

        let kept = suppress_source_overlapping_candidates(
            selected,
            &mut debug_candidates,
            &memories,
            &query_turns,
        )
        .into_iter()
        .map(|candidate| candidate.memory_id)
        .collect::<Vec<_>>();

        assert_eq!(kept, vec![2]);
        assert!(debug_candidates[0]
            .rank
            .filter_reasons
            .contains(&"drop:source_turn_already_in_query".to_string()));
    }

    #[test]
    fn recall_filter_prompt_never_truncates_serialized_json() {
        let config = Config {
            recall_llm_filter_prompt_max_chars: 3200,
            ..Config::default()
        };
        let memories = (1..=12)
            .map(|id| memory(id, &"body ".repeat(500)))
            .collect::<Vec<_>>();
        let candidates = (1..=12).map(candidate).collect::<Vec<_>>();
        let query_text = "current turn ".repeat(800);

        let prompt = recall_filter_prompt(
            &config,
            &query_text,
            &ContextMetadata::default(),
            "/tmp/yaaml",
            &memories,
            &candidates,
        );
        let parsed = serde_json::from_str::<Value>(&prompt).unwrap();

        assert!(prompt.chars().count() <= config.recall_llm_filter_prompt_max_chars);
        assert_eq!(parsed["candidate_memories"].as_array().unwrap().len(), 12);
    }

    fn candidate(memory_id: i64) -> RecallCandidate {
        candidate_with_score(memory_id, 0.5)
    }

    fn candidate_with_score(memory_id: i64, score: f32) -> RecallCandidate {
        RecallCandidate {
            memory_id,
            similarity: score,
            score,
            project_id: None,
            rank: yaaml_core::RecallRankDetails {
                vector_score: score,
                context_score: 0.0,
                project_bonus: 0.0,
                task_key_bonus: 0.0,
                global_durable_bonus: 0.0,
                penalties: Vec::new(),
                filter_reasons: Vec::new(),
                matched_task_keys: Vec::new(),
            },
        }
    }

    fn proven_useful_candidate(memory_id: i64, score: f32) -> RecallCandidate {
        let mut candidate = candidate_with_score(memory_id, score);
        candidate
            .rank
            .penalties
            .push("health_action_rerank:project_fact:proven_useful:0.14".to_string());
        candidate
    }

    fn memory(memory_id: i64, body: &str) -> MemoryRecord {
        MemoryRecord {
            id: Some(memory_id),
            title: format!("memory {memory_id}"),
            body: body.to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
            superseded_by_memory_id: None,
        }
    }
}
