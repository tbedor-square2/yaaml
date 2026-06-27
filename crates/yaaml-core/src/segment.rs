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
                range.end_index = index;
                range.end_turn_ordinal = turn.ordinal;
                push_keys(&mut range.keys, keys);
                push_markers(&mut range.context_markers, context_markers);
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
    let text = turns
        .iter()
        .filter_map(|turn| turn.display_text.as_deref())
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    merge_contexts(&mut context, infer_context_from_text(&text));
    context
}

fn segment_summary(
    keys: &[String],
    context: &ContextMetadata,
    start_turn_ordinal: u64,
    end_turn_ordinal: u64,
) -> String {
    let context_label = context
        .repo_id
        .as_deref()
        .or(context.work_area.as_deref())
        .or(context.activity_domain.as_deref())
        .unwrap_or("unclassified context");
    if keys.is_empty() {
        format!("Turns {start_turn_ordinal}..={end_turn_ordinal} discuss {context_label}.")
    } else {
        let rendered_keys = rendered_segment_keys(keys).join(", ");
        format!(
            "Turns {start_turn_ordinal}..={end_turn_ordinal} discuss {context_label} with task keys {rendered_keys}."
        )
    }
}

fn overlaps(left: &[String], right: &[String]) -> bool {
    let left = left.iter().collect::<HashSet<_>>();
    right.iter().any(|key| left.contains(key))
}

fn context_conflicts(left: &[String], right: &[String]) -> bool {
    !left.is_empty() && !right.is_empty() && !overlaps(left, right)
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
            context_markers,
        }
    }

    fn matches(&self, keys: &[String], context_markers: &[String]) -> bool {
        if !self.keys.is_empty() && !keys.is_empty() {
            return overlaps(&self.keys, keys);
        }
        !context_conflicts(&self.context_markers, context_markers)
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
