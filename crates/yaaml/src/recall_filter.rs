use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use yaaml_core::{
    select_recall_candidates, Config, ContextMetadata, MemoryRecord, RecallCandidate,
};

use crate::llm_judge::JudgeClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallFilterTelemetry {
    pub candidate_count: usize,
    pub deterministic_selected_count: usize,
    pub final_selected_count: usize,
    pub llm_attempted: bool,
    pub llm_applied: bool,
    #[serde(default)]
    pub llm_empty_fallback: bool,
    pub llm_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecallFilterResult {
    pub selected: Vec<RecallCandidate>,
    pub debug_candidates: Vec<RecallCandidate>,
    pub telemetry: RecallFilterTelemetry,
}

pub fn select_recall_candidates_with_llm_filter(
    config: &Config,
    candidates: Vec<RecallCandidate>,
    memories: &[MemoryRecord],
    current_project_id: &str,
    query_text: &str,
    query_context: &ContextMetadata,
    query_task_keys: &[String],
) -> RecallFilterResult {
    let filter_limit = config
        .recall_llm_filter_candidate_limit
        .max(config.recall_result_limit);
    let (filter_pool, mut debug_candidates) = select_recall_candidates(
        candidates,
        memories,
        current_project_id,
        query_context,
        query_task_keys,
        filter_limit,
    );
    let deterministic_selected = filter_pool
        .iter()
        .take(config.recall_result_limit)
        .cloned()
        .collect::<Vec<_>>();
    let mut telemetry = RecallFilterTelemetry {
        candidate_count: filter_pool.len(),
        deterministic_selected_count: deterministic_selected.len(),
        final_selected_count: deterministic_selected.len(),
        llm_attempted: false,
        llm_applied: false,
        llm_empty_fallback: false,
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
        query_text,
        query_context,
        current_project_id,
        memories,
        &filter_pool,
    ) {
        Ok(selected_ids) => {
            let mut selected_id_set = selected_ids.into_iter().collect::<HashSet<_>>();
            if selected_id_set.is_empty() && !deterministic_selected.is_empty() {
                selected_id_set.insert(deterministic_selected[0].memory_id);
                telemetry.llm_empty_fallback = true;
            }
            annotate_llm_filter(
                &mut debug_candidates,
                &filter_pool,
                &selected_id_set,
                telemetry.llm_empty_fallback,
            );
            let selected = filter_pool
                .iter()
                .filter(|candidate| selected_id_set.contains(&candidate.memory_id))
                .take(config.recall_result_limit)
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
    "You filter memory recall for a coding agent. Return compact JSON only. Treat the deterministic ranking as a useful baseline and prune only memories that are clearly unrelated, stale, or too generic to justify context cost. Select 1-5 memories whenever any candidate is plausibly useful as background or actionable guidance. Return an empty list only when every candidate is clearly wrong for the current task. Do not invent memory ids."
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
        "max_selected_memories": config.recall_result_limit,
        "selection_policy": "Prefer fewer memories than deterministic recall, but keep plausible background. Empty selection is allowed only when all candidates are clearly unrelated.",
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
        "max_selected_memories": config.recall_result_limit,
        "selection_policy": "Keep only candidate memories plausibly useful to this turn. Empty only when all are clearly unrelated.",
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
    empty_fallback: bool,
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
            candidate.rank.filter_reasons.push(if empty_fallback {
                "keep:llm_empty_fallback".to_string()
            } else {
                "keep:llm_semantic_filter".to_string()
            });
        } else {
            candidate.rank.filter_reasons.push(if empty_fallback {
                "drop:llm_empty_fallback".to_string()
            } else {
                "drop:llm_semantic_filter".to_string()
            });
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
        RecallCandidate {
            memory_id,
            similarity: 0.5,
            score: 0.5,
            project_id: None,
            rank: yaaml_core::RecallRankDetails {
                vector_score: 0.5,
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
        }
    }
}
