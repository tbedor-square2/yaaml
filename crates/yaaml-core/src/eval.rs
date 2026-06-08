use crate::{MemoryRecord, TurnRecord};

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

#[cfg(test)]
mod tests {
    use crate::{MemoryScope, TurnStatus};

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
}
