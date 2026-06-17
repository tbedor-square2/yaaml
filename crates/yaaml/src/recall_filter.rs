use std::collections::HashSet;
use std::env;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use yaaml_core::{
    select_recall_candidates, Config, ContextMetadata, MemoryRecord, RecallCandidate,
};
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::{ProviderError, ReqwestTransport};

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

fn recall_filter_client(config: &Config) -> Option<AnthropicMessageClient<ReqwestTransport>> {
    if !config.recall_llm_filter_enabled || config.eval_judge_provider != "anthropic" {
        return None;
    }
    if env::var(&config.eval_judge_api_key_env).is_err() {
        return None;
    }
    Some(AnthropicMessageClient::new(
        AnthropicMessageConfig::judge_from_config(config),
        ReqwestTransport::default(),
    ))
}

fn run_llm_filter(
    client: &AnthropicMessageClient<ReqwestTransport>,
    config: &Config,
    query_text: &str,
    query_context: &ContextMetadata,
    current_project_id: &str,
    memories: &[MemoryRecord],
    candidates: &[RecallCandidate],
) -> Result<Vec<i64>, ProviderError> {
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
                        "body": truncate_chars(&memory.body, 1200),
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
    let prompt = json!({
        "current_project_id": current_project_id,
        "query_context": query_context,
        "current_turn_text": truncate_chars(query_text, 6000),
        "max_selected_memories": config.recall_result_limit,
        "selection_policy": "Prefer fewer memories than deterministic recall, but keep plausible background. Empty selection is allowed only when all candidates are clearly unrelated.",
        "candidate_memories": candidates_json,
        "response_schema": {
            "selected_memory_ids": ["integer memory ids to keep, in candidate order or fewer"]
        }
    })
    .to_string();
    truncate_chars(&prompt, config.recall_llm_filter_prompt_max_chars)
}

fn parse_selected_memory_ids(value: &Value, candidates: &[RecallCandidate]) -> Vec<i64> {
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<HashSet<_>>();
    value
        .get("selected_memory_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        })
        .filter(|id| candidate_ids.contains(id))
        .fold(Vec::new(), |mut ids, id| {
            if !ids.contains(&id) {
                ids.push(id);
            }
            ids
        })
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

    use super::*;

    #[test]
    fn parses_selected_memory_ids_from_numbers_and_strings() {
        let candidates = vec![candidate(1), candidate(2), candidate(3)];
        let value = json!({"selected_memory_ids": [2, "3", 9, "bad", 2]});

        assert_eq!(parse_selected_memory_ids(&value, &candidates), vec![2, 3]);
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
}
