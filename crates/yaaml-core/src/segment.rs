use std::collections::HashSet;

use crate::{
    infer_context_from_text, merge_contexts, segment_task_keys, ContextMetadata,
    ConversationSegmentRecord, ConversationSegmentStatus, TurnRecord,
};

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
        match &mut current {
            None => {
                current = Some(SegmentRange::new(index, turn.ordinal, keys));
            }
            Some(range)
                if keys.is_empty() || range.keys.is_empty() || overlaps(&range.keys, &keys) =>
            {
                range.end_index = index;
                range.end_turn_ordinal = turn.ordinal;
                push_keys(&mut range.keys, keys);
            }
            Some(_) => {
                ranges.push(current.take().expect("current segment exists"));
                current = Some(SegmentRange::new(index, turn.ordinal, keys));
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
        let rendered_keys = keys.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        format!(
            "Turns {start_turn_ordinal}..={end_turn_ordinal} discuss {context_label} with task keys {rendered_keys}."
        )
    }
}

fn overlaps(left: &[String], right: &[String]) -> bool {
    let left = left.iter().collect::<HashSet<_>>();
    right.iter().any(|key| left.contains(key))
}

fn push_keys(keys: &mut Vec<String>, incoming: Vec<String>) {
    for key in incoming {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
}

#[derive(Debug)]
struct SegmentRange {
    start_index: usize,
    end_index: usize,
    start_turn_ordinal: u64,
    end_turn_ordinal: u64,
    keys: Vec<String>,
}

impl SegmentRange {
    fn new(index: usize, turn_ordinal: u64, keys: Vec<String>) -> Self {
        Self {
            start_index: index,
            end_index: index,
            start_turn_ordinal: turn_ordinal,
            end_turn_ordinal: turn_ordinal,
            keys,
        }
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
