use std::fs;
use std::path::Path;

use tempfile::TempDir;
use yaaml_core::{AgentType, SessionRecord, TurnStatus};
use yaaml_transcript::codex::{
    parse_codex_file_from_offset_with_session, parse_codex_jsonl, CodexParseError,
};

fn line(json: &str) -> String {
    format!("{json}\n")
}

fn session_meta() -> String {
    line(
        r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml"}}"#,
    )
}

fn fallback_session(path: &Path) -> SessionRecord {
    SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: "/tmp/yaaml".to_string(),
        transcript_file_path: path.display().to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:00Z".to_string()),
    }
}

#[test]
fn parses_single_completed_turn() {
    let input = format!(
        "{}{}{}{}",
        session_meta(),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-1"}}"#
        ),
    );

    let parsed = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(parsed.session.id, "session-1");
    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.turns[0].turn_id.as_deref(), Some("turn-1"));
    assert_eq!(parsed.turns[0].status, TurnStatus::Completed);
    assert_eq!(
        parsed.turns[0].display_text.as_deref(),
        Some("assistant: done")
    );
}

#[test]
fn aborted_turn_is_preserved_as_aborted() {
    let input = format!(
        "{}{}{}",
        session_meta(),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-1"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg","payload":{"type":"task_aborted","turn_id":"turn-1"}}"#
        ),
    );

    let parsed = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.turns[0].status, TurnStatus::Aborted);
}

#[test]
fn resumes_from_mid_file_offset_with_fallback_session() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("session.jsonl");
    let prefix = format!(
        "{}{}{}",
        session_meta(),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-1"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-1"}}"#
        ),
    );
    let suffix = format!(
        "{}{}{}",
        line(
            r#"{"timestamp":"2026-06-08T00:01:01Z","type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-2"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:01:02Z","type":"event_msg","payload":{"type":"agent_message","message":"second turn"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:01:03Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-2"}}"#
        ),
    );
    fs::write(&path, format!("{prefix}{suffix}")).unwrap();

    let parsed = parse_codex_file_from_offset_with_session(
        &path,
        prefix.len() as u64,
        Some(fallback_session(&path)),
    )
    .unwrap();

    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.turns[0].turn_id.as_deref(), Some("turn-2"));
    assert_eq!(
        parsed.turns[0].display_text.as_deref(),
        Some("assistant: second turn")
    );
    assert_eq!(parsed.next_offset, (prefix.len() + suffix.len()) as u64);
}

#[test]
fn unknown_events_inside_turn_do_not_break_parsing() {
    let input = format!(
        "{}{}{}{}",
        session_meta(),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-1"}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"unknown_event","payload":{"ignored":true}}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-1"}}"#
        ),
    );

    let parsed = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.turns[0].byte_end, input.len() as u64);
}

#[test]
fn tool_output_is_normalized_to_one_display_line() {
    let input = format!(
        "{}{}{}{}",
        session_meta(),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-1"}}"#
        ),
        line(
            r##"{"timestamp":"2026-06-08T00:00:02Z","type":"response_item","payload":{"type":"function_call_output","output":"# YAAML Recall\nmemory_ids: 1\n\n## Old memory\nBody text"}}"##
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-1"}}"#
        ),
    );

    let parsed = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(
        parsed.turns[0].display_text.as_deref(),
        Some("tool output: # YAAML Recall memory_ids: 1 ## Old memory Body text")
    );
}

#[test]
fn malformed_json_in_complete_line_reports_byte_offset() {
    let valid_prefix = session_meta();
    let malformed = line(r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg""#);
    let input = format!("{valid_prefix}{malformed}");

    let error = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0)
        .expect_err("malformed complete line should fail");

    assert!(matches!(
        error,
        CodexParseError::MalformedJson { offset, .. } if offset == valid_prefix.len() as u64
    ));
}

#[test]
fn incomplete_trailing_line_does_not_advance_cursor() {
    let complete = session_meta();
    let input = format!(
        "{}{}",
        complete, r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg""#
    );

    let parsed = parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

    assert!(parsed.turns.is_empty());
    assert_eq!(parsed.next_offset, complete.len() as u64);
}
