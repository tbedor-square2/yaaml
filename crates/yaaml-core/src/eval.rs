use crate::{MemoryRecord, TurnRecord};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalJudgeOutcome {
    pub score: String,
    pub rationale: String,
}

pub fn replay_context_before_turn(
    turns: &[TurnRecord],
    replay_index: usize,
    window: usize,
) -> Vec<TurnRecord> {
    let start = replay_index.saturating_sub(window);
    turns[start..replay_index.min(turns.len())].to_vec()
}

pub fn memories_created_before<'a>(
    memories: &'a [MemoryRecord],
    replay_turn: &TurnRecord,
) -> Vec<&'a MemoryRecord> {
    let Some(observed_at) = replay_turn.observed_at.as_deref() else {
        return Vec::new();
    };
    memories
        .iter()
        .filter(|memory| memory.created_at.as_str() < observed_at)
        .collect()
}

pub fn counterfactual_citation_score(
    memory: &MemoryRecord,
    replay_turn: &TurnRecord,
) -> &'static str {
    if memory.source_turn_refs.is_empty() {
        return "none";
    }
    if memory.source_turn_refs.iter().any(|source| {
        source.session_id == replay_turn.session_id && source.ordinal < replay_turn.ordinal
    }) {
        "same_session_prior"
    } else {
        "different_context"
    }
}

pub fn parse_eval_judge_response(value: &Value) -> EvalJudgeOutcome {
    let score = value
        .get("score")
        .and_then(Value::as_str)
        .map(normalize_eval_score)
        .unwrap_or_else(|| "neutral".to_string());
    let rationale = value
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|rationale| !rationale.is_empty())
        .unwrap_or("judge returned no rationale")
        .to_string();
    EvalJudgeOutcome { score, rationale }
}

fn normalize_eval_score(score: &str) -> String {
    match score.trim().to_ascii_lowercase().as_str() {
        "useful" => "useful".to_string(),
        "distracting" => "distracting".to_string(),
        _ => "neutral".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::{MemoryScope, SourceTurnRef, TurnStatus};

    use super::*;

    fn turn(ordinal: u64, text: &str) -> TurnRecord {
        TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: None,
            ordinal,
            byte_start: ordinal,
            byte_end: ordinal + 1,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: Some(text.to_string()),
        }
    }

    fn memory(id: i64, created_at: &str) -> MemoryRecord {
        MemoryRecord {
            id: Some(id),
            title: format!("memory {id}"),
            body: "body".to_string(),
            scope: MemoryScope::Project,
            source_turn_refs: Vec::new(),
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: None,
            lineage_refs: Vec::new(),
        }
    }

    #[test]
    fn replay_query_excludes_future_transcript_content() {
        let turns = vec![turn(0, "past"), turn(1, "replay"), turn(2, "future")];

        let context = replay_context_before_turn(&turns, 1, 3);

        assert_eq!(context.len(), 1);
        assert_eq!(context[0].display_text.as_deref(), Some("past"));
    }

    #[test]
    fn memories_created_after_replay_turn_are_excluded() {
        let replay = turn(1, "replay");
        let memories = vec![
            memory(1, "2026-06-08T00:00:00Z"),
            memory(2, "2026-06-08T00:00:02Z"),
        ];

        let available = memories_created_before(&memories, &replay);

        assert_eq!(available.len(), 1);
        assert_eq!(available[0].id, Some(1));
    }

    #[test]
    fn counterfactual_citation_distinguishes_prior_same_session_refs() {
        let replay = turn(3, "replay");
        let mut memory = memory(1, "2026-06-08T00:00:00Z");
        memory.source_turn_refs = vec![SourceTurnRef {
            session_id: "session-1".to_string(),
            ordinal: 1,
            byte_start: 0,
            byte_end: 1,
        }];

        assert_eq!(
            counterfactual_citation_score(&memory, &replay),
            "same_session_prior"
        );

        memory.source_turn_refs[0].session_id = "other-session".to_string();
        assert_eq!(
            counterfactual_citation_score(&memory, &replay),
            "different_context"
        );

        memory.source_turn_refs.clear();
        assert_eq!(counterfactual_citation_score(&memory, &replay), "none");
    }

    #[test]
    fn parses_eval_judge_response_with_score_normalization() {
        let outcome = parse_eval_judge_response(
            &serde_json::json!({"score":"USEFUL","rationale":"directly relevant"}),
        );

        assert_eq!(outcome.score, "useful");
        assert_eq!(outcome.rationale, "directly relevant");

        let outcome = parse_eval_judge_response(&serde_json::json!({"score":"surprising"}));

        assert_eq!(outcome.score, "neutral");
        assert_eq!(outcome.rationale, "judge returned no rationale");
    }
}
