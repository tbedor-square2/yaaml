use std::collections::HashSet;

use crate::{
    infer_context_from_text, merge_contexts, segment_context_markers, segment_task_keys,
    ContextMetadata, ConversationSegmentRecord, ConversationSegmentStatus, TurnRecord,
};

const MAX_SEGMENT_KEYS: usize = 16;
const MAX_SEGMENT_PATH_KEYS: usize = 4;

pub fn build_conversation_segments(
    session_id: &str,
    turns: &[TurnRecord],
    timestamp: &str,
) -> Vec<ConversationSegmentRecord> {
    let mut ranges = Vec::<SegmentRange>::new();
    let mut current: Option<SegmentRange> = None;

    for (index, turn) in turns.iter().enumerate() {
        let keys = turn
            .display_text
            .as_deref()
            .map(segment_task_keys)
            .unwrap_or_default();
        let context_markers = segment_context_markers(turn);
        match &mut current {
            None => {
                current = Some(SegmentRange::new(
                    index,
                    turn.ordinal,
                    keys,
                    context_markers,
                ));
            }
            Some(range) if range.matches(&keys, &context_markers) => {
                range.extend(index, turn.ordinal, keys, context_markers);
            }
            Some(_) => {
                ranges.push(current.take().expect("current segment exists"));
                current = Some(SegmentRange::new(
                    index,
                    turn.ordinal,
                    keys,
                    context_markers,
                ));
            }
        }
    }
    if let Some(range) = current {
        ranges.push(range);
    }

    let last_index = ranges.len().saturating_sub(1);
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, range)| {
            let status = if index == last_index {
                ConversationSegmentStatus::Active
            } else {
                ConversationSegmentStatus::Superseded
            };
            segment_record(session_id, turns, range, timestamp, status)
        })
        .collect()
}

fn segment_record(
    session_id: &str,
    turns: &[TurnRecord],
    range: SegmentRange,
    timestamp: &str,
    status: ConversationSegmentStatus,
) -> ConversationSegmentRecord {
    let segment_turns = &turns[range.start_index..=range.end_index];
    let context = segment_context(segment_turns);
    let summary = segment_summary(
        &range.keys,
        &context,
        range.start_turn_ordinal,
        range.end_turn_ordinal,
    );
    ConversationSegmentRecord {
        id: None,
        session_id: session_id.to_string(),
        start_turn_ordinal: range.start_turn_ordinal,
        end_turn_ordinal: range.end_turn_ordinal,
        summary,
        task_keys: range.keys,
        context: Some(context),
        status,
        created_at: timestamp.to_string(),
        updated_at: timestamp.to_string(),
    }
}

fn segment_context(turns: &[TurnRecord]) -> ContextMetadata {
    let mut context = turns
        .iter()
        .find_map(|turn| turn.context.clone())
        .unwrap_or_default();
    for turn in turns.iter().skip(1) {
        if let Some(turn_context) = &turn.context {
            merge_contexts(&mut context, turn_context.clone());
        }
    }
    let text = segment_context_text(turns);
    merge_contexts(&mut context, infer_context_from_text(&text));
    context
}

fn segment_context_text(turns: &[TurnRecord]) -> String {
    const HEAD_TURNS: usize = 2;
    const TAIL_TURNS: usize = 4;
    if turns.len() <= HEAD_TURNS + TAIL_TURNS {
        return turns
            .iter()
            .filter_map(|turn| turn.display_text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
    }
    turns
        .iter()
        .take(HEAD_TURNS)
        .chain(turns.iter().skip(turns.len().saturating_sub(TAIL_TURNS)))
        .filter_map(|turn| turn.display_text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

fn segment_summary(
    keys: &[String],
    context: &ContextMetadata,
    start_turn_ordinal: u64,
    end_turn_ordinal: u64,
) -> String {
    let location_label = context
        .repo_id
        .as_deref()
        .or(context.work_area.as_deref())
        .or(context.activity_domain.as_deref())
        .unwrap_or("unclassified context");
    let topic_label = segment_topic_label(context);
    let context_label = topic_label
        .as_ref()
        .map(|topic| format!("{topic} in {location_label}"))
        .unwrap_or_else(|| location_label.to_string());
    if keys.is_empty() {
        format!("Turns {start_turn_ordinal}..={end_turn_ordinal} discuss {context_label}.")
    } else {
        let rendered_keys = rendered_segment_keys(keys).join(", ");
        format!(
            "Turns {start_turn_ordinal}..={end_turn_ordinal} discuss {context_label} with task keys {rendered_keys}."
        )
    }
}

fn segment_topic_label(context: &ContextMetadata) -> Option<String> {
    let mut tags = context
        .subject_tags
        .iter()
        .filter(|tag| segment_summary_tag(tag))
        .cloned()
        .collect::<Vec<_>>();
    tags.sort_by(|left, right| {
        segment_summary_tag_priority(left)
            .cmp(&segment_summary_tag_priority(right))
            .then_with(|| left.cmp(right))
    });
    tags.dedup();
    if tags.is_empty() {
        None
    } else {
        Some(tags.into_iter().take(3).collect::<Vec<_>>().join(", "))
    }
}

fn segment_summary_tag(tag: &str) -> bool {
    !matches!(
        tag,
        "ci" | "claude-code"
            | "codex"
            | "docs"
            | "github"
            | "java"
            | "linear"
            | "pr"
            | "slack"
            | "work-tracking"
    )
}

fn segment_summary_tag_priority(tag: &str) -> u8 {
    match tag {
        "task-state" => 0,
        "conversation-segment" | "segment" => 1,
        "recall-quality" | "recall-eval" | "tool-recall" | "background-recall" => 2,
        "memory-consolidation" | "memory-formation" => 3,
        "llm-filter" | "vector-search" | "embedding" => 4,
        "transcript" | "ingestion" | "backlog" => 5,
        "daemon" | "tool-hook" => 6,
        "eval" | "recall" => 7,
        _ => 8,
    }
}

fn overlaps(left: &[String], right: &[String]) -> bool {
    let left = left.iter().collect::<HashSet<_>>();
    right.iter().any(|key| left.contains(key))
}

fn overlapping_keys(left: &[String], right: &[String]) -> Vec<String> {
    let left = left.iter().collect::<HashSet<_>>();
    right
        .iter()
        .filter(|key| left.contains(key))
        .cloned()
        .collect()
}

fn context_conflicts(left: &[String], right: &[String]) -> bool {
    !left.is_empty() && !right.is_empty() && !overlaps(left, right)
}

fn path_overlap_context_conflicts(left: &[String], right: &[String]) -> bool {
    let left = specific_path_overlap_markers(left);
    let right = specific_path_overlap_markers(right);
    context_conflicts(&left, &right)
}

fn specific_path_overlap_markers(markers: &[String]) -> Vec<String> {
    markers
        .iter()
        .filter(|marker| !is_broad_segment_marker(marker))
        .cloned()
        .collect()
}

fn is_broad_segment_marker(marker: &str) -> bool {
    marker.starts_with("repo:") || marker == "tag:yaaml"
}

fn push_keys(keys: &mut Vec<String>, incoming: Vec<String>) {
    let mut incoming = incoming;
    incoming.sort_by_key(|key| segment_key_priority(key));
    for key in incoming {
        if !keys.contains(&key) {
            if is_path_key(&key) && path_key_count(keys) >= MAX_SEGMENT_PATH_KEYS {
                continue;
            }
            if keys.len() >= MAX_SEGMENT_KEYS && !is_identity_key(&key) {
                continue;
            }
            keys.push(key);
        }
    }
}

fn push_markers(markers: &mut Vec<String>, incoming: Vec<String>) {
    for marker in incoming {
        if !markers.contains(&marker) {
            markers.push(marker);
        }
    }
}

fn bounded_segment_keys(keys: Vec<String>) -> Vec<String> {
    let mut bounded = Vec::new();
    push_keys(&mut bounded, keys);
    bounded
}

fn rendered_segment_keys(keys: &[String]) -> Vec<String> {
    let mut keys = keys.to_vec();
    keys.sort_by_key(|key| segment_key_priority(key));
    keys.into_iter().take(8).collect()
}

fn segment_key_priority(key: &str) -> u8 {
    if is_identity_key(key) {
        0
    } else if key.starts_with("project:") || key.starts_with("app:") {
        1
    } else if key.starts_with("target:") {
        2
    } else if key.starts_with("path:") {
        3
    } else {
        4
    }
}

fn is_identity_key(key: &str) -> bool {
    matches!(
        key.split_once(':').map(|(prefix, _)| prefix),
        Some(
            "branch"
                | "flag"
                | "generator"
                | "metric"
                | "pr"
                | "sentry"
                | "signal"
                | "task"
                | "ticket"
                | "trigger"
        )
    )
}

fn is_path_key(key: &str) -> bool {
    key.starts_with("path:")
}

fn path_key_count(keys: &[String]) -> usize {
    keys.iter().filter(|key| is_path_key(key)).count()
}

#[derive(Debug)]
struct SegmentRange {
    start_index: usize,
    end_index: usize,
    start_turn_ordinal: u64,
    end_turn_ordinal: u64,
    keys: Vec<String>,
    context_markers: Vec<String>,
    latest_context_markers: Vec<String>,
}

impl SegmentRange {
    fn new(
        index: usize,
        turn_ordinal: u64,
        keys: Vec<String>,
        context_markers: Vec<String>,
    ) -> Self {
        Self {
            start_index: index,
            end_index: index,
            start_turn_ordinal: turn_ordinal,
            end_turn_ordinal: turn_ordinal,
            keys: bounded_segment_keys(keys),
            context_markers: context_markers.clone(),
            latest_context_markers: context_markers,
        }
    }

    fn matches(&self, keys: &[String], context_markers: &[String]) -> bool {
        if !self.keys.is_empty() && !keys.is_empty() {
            let overlapping = overlapping_keys(&self.keys, keys);
            if overlapping.is_empty() {
                return false;
            }
            if overlapping.iter().all(|key| is_path_key(key)) {
                return !path_overlap_context_conflicts(
                    &self.latest_context_markers,
                    context_markers,
                );
            }
            return true;
        }
        !context_conflicts(&self.latest_context_markers, context_markers)
    }

    fn extend(&mut self, index: usize, turn_ordinal: u64, keys: Vec<String>, markers: Vec<String>) {
        self.end_index = index;
        self.end_turn_ordinal = turn_ordinal;
        push_keys(&mut self.keys, keys);
        if !specific_path_overlap_markers(&markers).is_empty() {
            self.latest_context_markers = markers.clone();
        }
        push_markers(&mut self.context_markers, markers);
    }
}

#[cfg(test)]
mod tests {
    use crate::{build_conversation_segments, TurnStatus};

    use super::*;

    #[test]
    fn segment_builder_splits_on_strong_task_key_change() {
        let turns = vec![
            turn(1, "user: finish PR #483111 for MLP-4410"),
            turn(2, "assistant: PR #483111 tests pass"),
            turn(
                3,
                "user: now debug PR #483601 in riskarbiter/src/test/java/FooTest.java",
            ),
            turn(4, "assistant: tests are timing out"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_turn_ordinal, 1);
        assert_eq!(segments[0].end_turn_ordinal, 2);
        assert_eq!(segments[0].status, ConversationSegmentStatus::Superseded);
        assert!(segments[0].task_keys.contains(&"pr:483111".to_string()));
        assert_eq!(segments[1].start_turn_ordinal, 3);
        assert_eq!(segments[1].end_turn_ordinal, 4);
        assert_eq!(segments[1].status, ConversationSegmentStatus::Active);
        assert!(segments[1].task_keys.contains(&"pr:483601".to_string()));
        assert!(segments[1]
            .task_keys
            .contains(&"path:riskarbiter/src/test/java/footest.java".to_string()));
    }

    #[test]
    fn segment_builder_prioritizes_identity_keys_and_caps_paths() {
        let turns = vec![
            turn(
                1,
                "user: continue PR #483111 for MLP-4410 in \
                 crates/a/src/lib.rs crates/b/src/lib.rs crates/c/src/lib.rs \
                 crates/d/src/lib.rs crates/e/src/lib.rs crates/f/src/lib.rs",
            ),
            turn(
                2,
                "assistant: PR #483111 updated crates/g/src/lib.rs and crates/h/src/lib.rs",
            ),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 1);
        let segment = &segments[0];
        assert_eq!(segment.status, ConversationSegmentStatus::Active);
        assert!(segment.task_keys.contains(&"pr:483111".to_string()));
        assert!(segment.task_keys.contains(&"ticket:MLP-4410".to_string()));
        assert!(
            segment
                .task_keys
                .iter()
                .filter(|key| key.starts_with("path:"))
                .count()
                <= MAX_SEGMENT_PATH_KEYS
        );
        let pr_position = segment
            .summary
            .find("pr:483111")
            .expect("summary includes PR key");
        let path_position = segment
            .summary
            .find("path:")
            .expect("summary includes path key");
        assert!(pr_position < path_position);
    }

    #[test]
    fn segment_builder_splits_on_keyless_context_shift() {
        let turns = vec![
            turn(1, "user: keep improving YAAML recall eval quality"),
            turn(2, "assistant: adjusted recall metrics"),
            turn(3, "user: now investigate Risk Arbiter rollout behavior"),
            turn(4, "assistant: checked Risk Arbiter staging evidence"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_turn_ordinal, 1);
        assert_eq!(segments[0].end_turn_ordinal, 2);
        assert_eq!(segments[0].status, ConversationSegmentStatus::Superseded);
        assert_eq!(segments[1].start_turn_ordinal, 3);
        assert_eq!(segments[1].end_turn_ordinal, 4);
        assert_eq!(segments[1].status, ConversationSegmentStatus::Active);
        assert!(segments[0].summary.contains("yaaml"));
        assert!(segments[1].summary.contains("riskarbiter"));
    }

    #[test]
    fn segment_builder_splits_same_repo_memory_system_topic_shift() {
        let turns = vec![
            turn(1, "user: inspect YAAML recall eval failures"),
            turn(2, "assistant: found low recall evaluation scores"),
            turn(3, "user: now focus on stale task-state segment lifecycle"),
            turn(4, "assistant: tightened task state expiry"),
            turn(5, "user: next look at transcript ingestion backlog"),
            turn(6, "assistant: checked ingestion progress"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start_turn_ordinal, 1);
        assert_eq!(segments[0].end_turn_ordinal, 2);
        assert_eq!(segments[0].status, ConversationSegmentStatus::Superseded);
        assert_eq!(segments[1].start_turn_ordinal, 3);
        assert_eq!(segments[1].end_turn_ordinal, 4);
        assert_eq!(segments[1].status, ConversationSegmentStatus::Superseded);
        assert_eq!(segments[2].start_turn_ordinal, 5);
        assert_eq!(segments[2].end_turn_ordinal, 6);
        assert_eq!(segments[2].status, ConversationSegmentStatus::Active);
        let first_tags = &segments[0].context.as_ref().unwrap().subject_tags;
        let second_tags = &segments[1].context.as_ref().unwrap().subject_tags;
        let third_tags = &segments[2].context.as_ref().unwrap().subject_tags;
        assert!(first_tags.contains(&"recall-eval".to_string()));
        assert!(second_tags.contains(&"task-state".to_string()));
        assert!(third_tags.contains(&"ingestion".to_string()));
        assert!(segments[0].summary.contains("recall-eval"));
        assert!(segments[1].summary.contains("segment"));
        assert!(segments[2].summary.contains("ingestion"));
    }

    #[test]
    fn segment_builder_keeps_same_repo_overlapping_recall_topic() {
        let turns = vec![
            turn(1, "user: inspect YAAML recall eval failures"),
            turn(2, "assistant: found low recall evaluation scores"),
            turn(3, "user: continue recall quality experiments"),
            turn(4, "assistant: compared recall ranking variants"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start_turn_ordinal, 1);
        assert_eq!(segments[0].end_turn_ordinal, 4);
        assert_eq!(segments[0].status, ConversationSegmentStatus::Active);
    }

    #[test]
    fn segment_builder_splits_path_only_overlap_on_topic_shift() {
        let turns = vec![
            turn(
                1,
                "user: inspect recall eval failures in crates/yaaml-core/src/recall.rs",
            ),
            turn(2, "assistant: updated crates/yaaml-core/src/recall.rs"),
            turn(
                3,
                "user: now fix task-state segment lifecycle in crates/yaaml-core/src/recall.rs",
            ),
            turn(4, "assistant: changed crates/yaaml-core/src/recall.rs"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_turn_ordinal, 1);
        assert_eq!(segments[0].end_turn_ordinal, 2);
        assert_eq!(segments[1].start_turn_ordinal, 3);
        assert_eq!(segments[1].end_turn_ordinal, 4);
        assert!(segments[0]
            .task_keys
            .contains(&"path:crates/yaaml-core/src/recall.rs".to_string()));
        assert!(segments[1]
            .task_keys
            .contains(&"path:crates/yaaml-core/src/recall.rs".to_string()));
    }

    #[test]
    fn segment_summary_uses_tail_turns_for_long_segments() {
        let turns = vec![
            turn(1, "user: inspect YAAML recall eval behavior"),
            turn(2, "assistant: adjusted recall eval diagnostics"),
            turn(3, "assistant: unchanged recall progress"),
            turn(4, "assistant: unchanged recall progress"),
            turn(5, "assistant: unchanged recall progress"),
            turn(6, "user: continue recall quality for task-state segments"),
            turn(7, "assistant: tightened task-state recall expiry"),
        ];

        let segments = build_conversation_segments("session-1", &turns, "unix:1");

        assert_eq!(segments.len(), 1);
        assert!(segments[0].summary.contains("task-state"));
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
}
