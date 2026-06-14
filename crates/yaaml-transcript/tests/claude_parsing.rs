use std::fs;
use std::path::Path;

use tempfile::TempDir;
use yaaml_transcript::claude::{discover_claude_transcripts, parse_claude_jsonl, ClaudeParseError};

fn line(json: &str) -> String {
    format!("{json}\n")
}

#[test]
fn parses_basic_user_assistant_turn_pair() {
    let input = format!(
        "{}{}",
        line(
            r#"{"timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml","role":"user","content":"implement this"}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant","content":[{"type":"text","text":"done"}]}"#
        ),
    );

    let parsed =
        parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(parsed.session.id, "claude-session");
    assert_eq!(parsed.turns.len(), 1);
    assert!(parsed.turns[0]
        .display_text
        .as_deref()
        .unwrap()
        .contains("implement this"));
    assert!(parsed.turns[0]
        .display_text
        .as_deref()
        .unwrap()
        .contains("done"));
    assert_eq!(parsed.turns[0].cwd.as_deref(), Some("/tmp/yaaml"));
}

#[test]
fn parses_tool_use_loop_until_text_only_assistant_close() {
    let input = format!(
        "{}{}{}{}",
        line(r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"run tests"}"#),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"cmd":"cargo test"}}]}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","role":"user","content":[{"type":"tool_result","content":"ok"}]}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:03Z","role":"assistant","content":[{"type":"text","text":"tests passed"}]}"#
        ),
    );

    let parsed =
        parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0).unwrap();

    assert_eq!(parsed.turns.len(), 1);
    let text = parsed.turns[0].display_text.as_deref().unwrap();
    assert!(text.contains("run tests"));
    assert!(text.contains("tool: Bash"));
    assert!(text.contains("tool output: ok"));
    assert!(text.contains("tests passed"));
}

#[test]
fn discovery_skips_subagent_transcripts() {
    let tmp = TempDir::new().unwrap();
    let main = tmp.path().join("main.jsonl");
    let subagent_dir = tmp.path().join("subagents");
    fs::create_dir_all(&subagent_dir).unwrap();
    let subagent = subagent_dir.join("nested.jsonl");
    fs::write(&main, "").unwrap();
    fs::write(&subagent, "").unwrap();

    let transcripts = discover_claude_transcripts(tmp.path()).unwrap();

    assert_eq!(transcripts, vec![main]);
}

#[test]
fn partial_write_mid_turn_does_not_complete_or_advance_past_complete_lines() {
    let complete = line(r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"hello"}"#);
    let input = format!(
        "{}{}",
        complete,
        r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant","content":[{"type":"text""#
    );

    let parsed =
        parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0).unwrap();

    assert!(parsed.turns.is_empty());
    assert_eq!(parsed.next_offset, complete.len() as u64);
}

#[test]
fn malformed_json_in_middle_of_stream_returns_error_offset() {
    let valid_prefix =
        line(r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"hello"}"#);
    let input = format!(
        "{}{}{}",
        valid_prefix,
        line(r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant""#),
        line(r#"{"timestamp":"2026-06-08T00:00:02Z","role":"assistant","content":"done"}"#),
    );

    let error = parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0)
        .expect_err("malformed complete line should fail");

    assert!(matches!(
        error,
        ClaudeParseError::MalformedJson { offset, .. } if offset == valid_prefix.len() as u64
    ));
}

#[test]
fn assistant_message_without_content_does_not_complete_turn() {
    let input = format!(
        "{}{}",
        line(r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"hello"}"#),
        line(r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant"}"#),
    );

    let parsed =
        parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0).unwrap();

    assert!(parsed.turns.is_empty());
}

#[test]
fn error_tool_result_is_included_in_turn_text() {
    let input = format!(
        "{}{}{}",
        line(r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"run command"}"#),
        line(
            r#"{"timestamp":"2026-06-08T00:00:01Z","role":"user","content":[{"type":"tool_result","is_error":true,"content":"permission denied"}]}"#
        ),
        line(
            r#"{"timestamp":"2026-06-08T00:00:02Z","role":"assistant","content":[{"type":"text","text":"failed"}]}"#
        ),
    );

    let parsed =
        parse_claude_jsonl(Path::new("/tmp/claude-session.jsonl"), input.as_bytes(), 0).unwrap();

    let text = parsed.turns[0].display_text.as_deref().unwrap();
    assert!(text.contains("tool output: permission denied"));
    assert!(text.contains("failed"));
}
