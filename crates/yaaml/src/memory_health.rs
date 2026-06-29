use std::collections::HashMap;

use yaaml_core::{MemoryKind, MemoryRecord, RecallCandidate};
use yaaml_store::database::MemoryEvalHistoryRecord;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryHealthSummary {
    pub judged_count: u64,
    pub useful_count: u64,
    pub low_count: u64,
    pub average_score: Option<f64>,
    pub failure_mode: String,
}

#[derive(Debug, Default)]
struct MemoryHealthAccumulator {
    judged_scores: Vec<u8>,
    useful_count: u64,
    low_count: u64,
    latest_low_rationale: Option<String>,
}

pub fn build_memory_health_summaries(
    memories: &[MemoryRecord],
    history: &[MemoryEvalHistoryRecord],
) -> HashMap<i64, MemoryHealthSummary> {
    let mut accumulators = HashMap::<i64, MemoryHealthAccumulator>::new();
    for record in history {
        let Some(score) = numeric_eval_score(&record.judge_score) else {
            continue;
        };
        let accumulator = accumulators.entry(record.memory_id).or_default();
        accumulator.judged_scores.push(score);
        if score >= 4 {
            accumulator.useful_count += 1;
        } else if score <= 2 {
            accumulator.low_count += 1;
            if let Some(rationale) = record.rationale.as_deref() {
                accumulator.latest_low_rationale = Some(eval_summary_snippet(rationale));
            }
        }
    }

    let memory_by_id = memories
        .iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();
    accumulators
        .into_iter()
        .filter_map(|(memory_id, accumulator)| {
            let memory = memory_by_id.get(&memory_id)?;
            Some((memory_id, memory_health_summary(memory, accumulator)))
        })
        .collect()
}

pub fn apply_health_action_rerank(
    mut candidates: Vec<RecallCandidate>,
    memories: &[MemoryRecord],
    health: &HashMap<i64, MemoryHealthSummary>,
) -> Vec<RecallCandidate> {
    let memory_by_id = memories
        .iter()
        .filter_map(|memory| memory.id.map(|id| (id, memory)))
        .collect::<HashMap<_, _>>();
    for candidate in &mut candidates {
        let Some(summary) = health.get(&candidate.memory_id) else {
            continue;
        };
        let Some(memory) = memory_by_id.get(&candidate.memory_id) else {
            continue;
        };
        let (mode_delta, clear_task_bonus) = health_mode_adjustment(candidate, summary);
        let delta = health_adjustment(summary) + mode_delta;
        candidate.score += delta;
        if clear_task_bonus {
            candidate.rank.task_key_bonus = 0.0;
            candidate.rank.matched_task_keys.clear();
            candidate
                .rank
                .filter_reasons
                .retain(|reason| reason != "keep:strong_task_key_match");
        }
        candidate.rank.penalties.push(format!(
            "health_action_rerank:{}:{}:{delta:.2}",
            memory.kind.as_str(),
            summary.failure_mode
        ));
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

fn memory_health_summary(
    memory: &MemoryRecord,
    accumulator: MemoryHealthAccumulator,
) -> MemoryHealthSummary {
    let judged_count = accumulator.judged_scores.len() as u64;
    let low_rate = ratio(accumulator.low_count, judged_count);
    let useful_rate = ratio(accumulator.useful_count, judged_count);
    let average_score = if accumulator.judged_scores.is_empty() {
        None
    } else {
        Some(
            accumulator
                .judged_scores
                .iter()
                .map(|score| f64::from(*score))
                .sum::<f64>()
                / accumulator.judged_scores.len() as f64,
        )
    };
    let failure_mode = diagnose_memory_failure(memory, &accumulator, low_rate, useful_rate);
    MemoryHealthSummary {
        judged_count,
        useful_count: accumulator.useful_count,
        low_count: accumulator.low_count,
        average_score,
        failure_mode,
    }
}

fn diagnose_memory_failure(
    memory: &MemoryRecord,
    accumulator: &MemoryHealthAccumulator,
    low_rate: f64,
    useful_rate: f64,
) -> String {
    let judged_count = accumulator.judged_scores.len() as u64;
    let body_len = memory.body.chars().count();
    let task_key_count = memory.task_keys.len();
    let latest_low = accumulator
        .latest_low_rationale
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();

    if judged_count >= 5 && accumulator.useful_count == 0 && low_rate >= 0.70 {
        if looks_stale_or_episodic(memory, &latest_low) {
            return "stale_episodic".to_string();
        }
        if rationale_mentions_wrong_context(&latest_low) {
            return "wrong_context".to_string();
        }
        if body_len < 300 {
            return "vague_under_contextualized".to_string();
        }
        if task_key_count >= 6 {
            return "noisy_metadata".to_string();
        }
        return "consistently_low_value".to_string();
    }

    if judged_count >= 5 && useful_rate >= 0.70 {
        return "proven_useful".to_string();
    }

    if accumulator.useful_count > 0 && accumulator.low_count > 0 {
        if rationale_mentions_wrong_context(&latest_low)
            && matches!(memory.kind, MemoryKind::Lesson | MemoryKind::Workflow)
        {
            return "context_sensitive_wrong_context".to_string();
        }
        if rationale_mentions_wrong_context(&latest_low)
            || memory.kind == MemoryKind::TaskState
            || memory.kind == MemoryKind::TaskCheckpoint
            || memory.kind == MemoryKind::ProjectFact
        {
            return "context_sensitive".to_string();
        }
        return "mixed_performance".to_string();
    }

    if judged_count > 0 && low_rate >= 0.70 {
        if body_len < 300 {
            return "vague_under_contextualized".to_string();
        }
        if task_key_count >= 6 {
            return "noisy_metadata".to_string();
        }
        return "likely_low_value".to_string();
    }

    "unproven".to_string()
}

fn health_adjustment(memory_health: &MemoryHealthSummary) -> f32 {
    let mut adjustment = 0.0_f32;
    if memory_health.judged_count >= 3 {
        adjustment += (memory_health.useful_ratio() * 0.12).min(0.16);
        adjustment -= (memory_health.low_ratio() * 0.18).min(0.24);
    }
    if memory_health.globally_bad() {
        adjustment -= 0.40;
    }
    adjustment
}

fn health_mode_adjustment(
    candidate: &RecallCandidate,
    memory_health: &MemoryHealthSummary,
) -> (f32, bool) {
    match memory_health.failure_mode.as_str() {
        "proven_useful" => (0.14, false),
        "wrong_context" => {
            let penalty = if wrong_context_applies(candidate, memory_health) {
                -0.55
            } else {
                -0.08
            };
            (penalty, false)
        }
        "context_sensitive_wrong_context" => {
            if strong_task(candidate) {
                (0.02, false)
            } else {
                (-0.45, false)
            }
        }
        "stale_episodic" | "consistently_low_value" => (-0.65, false),
        "likely_low_value" => (-0.25, false),
        "noisy_metadata" => (-(candidate.rank.task_key_bonus + 0.18).min(0.42), true),
        "context_sensitive" | "mixed_performance" => {
            if strong_task(candidate) || candidate.rank.context_score >= 0.50 {
                (0.04, false)
            } else {
                (-0.24, false)
            }
        }
        "vague_under_contextualized" => (-0.20, false),
        _ => (0.0, false),
    }
}

fn wrong_context_applies(candidate: &RecallCandidate, memory_health: &MemoryHealthSummary) -> bool {
    memory_health.failure_mode == "wrong_context"
        && !(strong_task(candidate) || candidate.rank.context_score >= 0.62)
}

fn strong_task(candidate: &RecallCandidate) -> bool {
    candidate.rank.task_key_bonus >= 0.20
        || candidate
            .rank
            .filter_reasons
            .iter()
            .any(|reason| reason == "keep:strong_task_key_match")
}

impl MemoryHealthSummary {
    fn low_ratio(&self) -> f32 {
        if self.judged_count == 0 {
            0.0
        } else {
            self.low_count as f32 / self.judged_count as f32
        }
    }

    fn useful_ratio(&self) -> f32 {
        if self.judged_count == 0 {
            0.0
        } else {
            self.useful_count as f32 / self.judged_count as f32
        }
    }

    fn globally_bad(&self) -> bool {
        self.judged_count >= 5 && self.useful_count == 0 && self.low_ratio() >= 0.70
    }
}

fn numeric_eval_score(score: &str) -> Option<u8> {
    match score.trim() {
        "1" => Some(1),
        "2" => Some(2),
        "3" => Some(3),
        "4" => Some(4),
        "5" => Some(5),
        _ => None,
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn rationale_mentions_wrong_context(rationale: &str) -> bool {
    [
        "unrelated",
        "wrong context",
        "different project",
        "different domain",
        "no connection",
        "no bearing",
        "irrelevant",
        "mismatch",
        "not directly actionable",
        "requires substantial reframing",
        "tangential",
    ]
    .iter()
    .any(|needle| rationale.contains(needle))
}

fn looks_stale_or_episodic(memory: &MemoryRecord, rationale: &str) -> bool {
    let text = format!("{} {} {}", memory.title, memory.body, rationale).to_ascii_lowercase();
    memory.kind == MemoryKind::TaskState
        || memory.kind == MemoryKind::TaskCheckpoint
        || [
            "stale",
            "already resolved",
            "old task",
            "past task",
            "previously",
            "no longer",
            "current pr",
            "this pr",
            "branch",
            "queued",
            "parked",
            "status",
        ]
        .iter()
        .any(|needle| text.contains(needle))
}

fn eval_summary_snippet(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    text.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use yaaml_core::{MemoryKind, MemoryRecord, MemoryScope, RecallCandidate, RecallRankDetails};
    use yaaml_store::database::MemoryEvalHistoryRecord;

    use super::{apply_health_action_rerank, build_memory_health_summaries};

    #[test]
    fn health_action_rerank_penalizes_consistently_low_memory() {
        let low_memory = memory(1, "Low memory", MemoryKind::Lesson);
        let useful_memory = memory(2, "Useful memory", MemoryKind::Lesson);
        let memories = vec![low_memory, useful_memory];
        let history = [
            scores(1, &["1", "2", "1", "2", "1"]),
            scores(2, &["5", "4", "5", "4", "5"]),
        ]
        .concat();
        let health = build_memory_health_summaries(&memories, &history);
        let reranked = apply_health_action_rerank(
            vec![candidate(1, 1.20), candidate(2, 0.95)],
            &memories,
            &health,
        );

        assert_eq!(reranked[0].memory_id, 2);
        assert!(reranked[0].score > reranked[1].score);
        assert!(reranked[1]
            .rank
            .penalties
            .iter()
            .any(|penalty| penalty.contains("consistently_low_value")));
    }

    #[test]
    fn health_action_rerank_clears_noisy_metadata_task_bonus() {
        let noisy_memory = MemoryRecord {
            task_keys: vec![
                "path:a".to_string(),
                "path:b".to_string(),
                "path:c".to_string(),
                "path:d".to_string(),
                "path:e".to_string(),
                "path:f".to_string(),
            ],
            ..memory(1, "Noisy metadata", MemoryKind::Workflow)
        };
        let memories = vec![noisy_memory];
        let history = scores(1, &["1", "2", "1"]);
        let health = build_memory_health_summaries(&memories, &history);
        let mut noisy_candidate = candidate(1, 1.20);
        noisy_candidate.rank.task_key_bonus = 0.28;
        noisy_candidate.rank.matched_task_keys = vec!["path:a".to_string()];

        let reranked = apply_health_action_rerank(vec![noisy_candidate], &memories, &health);

        assert_eq!(reranked[0].rank.task_key_bonus, 0.0);
        assert!(reranked[0].rank.matched_task_keys.is_empty());
    }

    #[test]
    fn health_action_rerank_penalizes_mixed_wrong_context_without_strong_task_match() {
        let memory = memory(1, "Context-sensitive workflow", MemoryKind::Workflow);
        let memories = vec![memory];
        let history = [
            score_with_rationale(1, "5", "Useful for the original cutover task."),
            score_with_rationale(
                1,
                "2",
                "The recalled context is tangential and not directly actionable for the cleanup task.",
            ),
        ];
        let health = build_memory_health_summaries(&memories, &history);
        let mut candidate = candidate(1, 1.20);
        candidate.rank.context_score = 0.84;

        let reranked = apply_health_action_rerank(vec![candidate], &memories, &health);

        assert!(reranked[0].score < 0.90);
        assert!(reranked[0].rank.penalties.iter().any(|penalty| {
            penalty.contains("health_action_rerank:workflow:context_sensitive_wrong_context")
        }));
    }

    fn memory(id: i64, title: &str, kind: MemoryKind) -> MemoryRecord {
        MemoryRecord {
            id: Some(id),
            title: title.to_string(),
            body: "Durable memory body with enough details to avoid short-body classification and make the failure mode depend on historical eval performance rather than body length alone. This fixture intentionally includes several concrete sentences about recall behavior, candidate ranking, prior evaluation outcomes, and task context so the diagnostic path is driven by scores and metadata instead of falling into the vague memory bucket.".to_string(),
            scope: MemoryScope::Project,
            kind,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some("/tmp/project".to_string()),
            project_descriptor: Some("project".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        }
    }

    fn candidate(memory_id: i64, score: f32) -> RecallCandidate {
        RecallCandidate {
            memory_id,
            similarity: score,
            score,
            project_id: Some("/tmp/project".to_string()),
            rank: RecallRankDetails {
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

    fn scores(memory_id: i64, scores: &[&str]) -> Vec<MemoryEvalHistoryRecord> {
        scores
            .iter()
            .enumerate()
            .map(|(index, score)| MemoryEvalHistoryRecord {
                id: index as i64,
                memory_id,
                judge_score: (*score).to_string(),
                rationale: Some("Useful or not useful in prior recall.".to_string()),
            })
            .collect()
    }

    fn score_with_rationale(
        memory_id: i64,
        score: &str,
        rationale: &str,
    ) -> MemoryEvalHistoryRecord {
        MemoryEvalHistoryRecord {
            id: 1,
            memory_id,
            judge_score: score.to_string(),
            rationale: Some(rationale.to_string()),
        }
    }
}
