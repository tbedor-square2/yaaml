use std::fs;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;
use yaaml_core::{paths::normalize_project_id, AgentType, SessionRecord, TurnRecord, TurnStatus};

#[derive(Debug, Error)]
pub enum CodexParseError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("malformed JSONL at byte offset {offset}: {source}")]
    MalformedJson {
        offset: u64,
        source: serde_json::Error,
    },
    #[error("missing Codex session metadata")]
    MissingSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCodexChunk {
    pub session: SessionRecord,
    pub turns: Vec<TurnRecord>,
    pub next_offset: u64,
}

#[derive(Debug, Clone)]
struct CurrentTurn {
    turn_id: Option<String>,
    byte_start: u64,
    byte_end: u64,
    observed_at: Option<String>,
    status: Option<TurnStatus>,
    display_parts: Vec<String>,
}

pub fn parse_codex_file_from_offset(
    path: impl AsRef<Path>,
    start_offset: u64,
) -> Result<ParsedCodexChunk, CodexParseError> {
    parse_codex_file_from_offset_with_session(path, start_offset, None)
}

pub fn parse_codex_file_from_offset_with_session(
    path: impl AsRef<Path>,
    start_offset: u64,
    fallback_session: Option<SessionRecord>,
) -> Result<ParsedCodexChunk, CodexParseError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| CodexParseError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let start = usize::try_from(start_offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    parse_codex_jsonl_with_session(path, &bytes[start..], start_offset, fallback_session)
}

pub fn parse_codex_jsonl(
    transcript_path: &Path,
    bytes: &[u8],
    start_offset: u64,
) -> Result<ParsedCodexChunk, CodexParseError> {
    parse_codex_jsonl_with_session(transcript_path, bytes, start_offset, None)
}

fn parse_codex_jsonl_with_session(
    transcript_path: &Path,
    bytes: &[u8],
    start_offset: u64,
    fallback_session: Option<SessionRecord>,
) -> Result<ParsedCodexChunk, CodexParseError> {
    let mut session: Option<SessionRecord> = fallback_session;
    let mut current: Option<CurrentTurn> = None;
    let mut turns = Vec::new();
    let mut next_offset = start_offset;
    let mut line_offset = start_offset;

    for raw_line in complete_lines(bytes) {
        let line_start = line_offset;
        let line_end = line_start + u64::try_from(raw_line.len()).unwrap_or(u64::MAX);
        line_offset = line_end;

        let line = trim_line_ending(raw_line);
        if line.is_empty() {
            next_offset = line_end;
            continue;
        }

        let value: Value =
            serde_json::from_slice(line).map_err(|source| CodexParseError::MalformedJson {
                offset: line_start,
                source,
            })?;

        let top_type = value.get("type").and_then(Value::as_str);
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);

        match top_type {
            Some("session_meta") => {
                if session.is_none() {
                    session = Some(parse_session(transcript_path, &value));
                }
                if current.is_none() {
                    next_offset = line_end;
                }
            }
            Some("event_msg") => {
                let before_turn_count = turns.len();
                handle_event_msg(
                    &value,
                    line_start,
                    line_end,
                    timestamp,
                    &mut current,
                    &mut turns,
                    session.as_ref().map(|s| s.id.as_str()),
                );
                if turns.len() > before_turn_count || current.is_none() {
                    next_offset = line_end;
                }
            }
            Some("turn_context") => {
                if current.is_none() {
                    current = Some(start_turn(&value, line_start, line_end, timestamp.clone()));
                }
                if let (Some(session), Some(cwd)) = (
                    session.as_mut(),
                    value.pointer("/payload/cwd").and_then(Value::as_str),
                ) {
                    session.project_id = normalized_project_id(cwd);
                }
                if let Some(turn) = current.as_mut() {
                    turn.byte_end = line_end;
                    turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
                } else {
                    next_offset = line_end;
                }
            }
            Some("response_item") => {
                if let Some(turn) = current.as_mut() {
                    turn.byte_end = line_end;
                    turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
                    extract_response_item_text(&value, &mut turn.display_parts);
                } else {
                    next_offset = line_end;
                }
            }
            _ => {
                if let Some(turn) = current.as_mut() {
                    turn.byte_end = line_end;
                    turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
                } else {
                    next_offset = line_end;
                }
            }
        }
    }

    let session = session.ok_or(CodexParseError::MissingSession)?;
    Ok(ParsedCodexChunk {
        session,
        turns,
        next_offset,
    })
}

fn parse_session(transcript_path: &Path, value: &Value) -> SessionRecord {
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-codex-session")
        .to_string();
    let project_id = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(normalized_project_id)
        .unwrap_or_default();
    let started_at = payload
        .get("timestamp")
        .or_else(|| value.get("timestamp"))
        .and_then(Value::as_str)
        .map(str::to_string);

    SessionRecord {
        id,
        agent_type: AgentType::Codex,
        project_id,
        transcript_file_path: transcript_path.display().to_string(),
        started_at: started_at.clone(),
        last_seen_at: started_at,
    }
}

fn normalized_project_id(cwd: &str) -> String {
    normalize_project_id(Path::new(cwd)).display().to_string()
}

fn handle_event_msg(
    value: &Value,
    line_start: u64,
    line_end: u64,
    timestamp: Option<String>,
    current: &mut Option<CurrentTurn>,
    turns: &mut Vec<TurnRecord>,
    session_id: Option<&str>,
) {
    let payload_type = value.pointer("/payload/type").and_then(Value::as_str);
    match payload_type {
        Some("task_started") | Some("turn_started") => {
            *current = Some(start_turn(value, line_start, line_end, timestamp));
        }
        Some("task_complete") | Some("turn_complete") => {
            finish_turn(
                current,
                turns,
                session_id,
                line_end,
                timestamp,
                TurnStatus::Completed,
            );
        }
        Some("task_aborted") | Some("turn_aborted") => {
            finish_turn(
                current,
                turns,
                session_id,
                line_end,
                timestamp,
                TurnStatus::Aborted,
            );
        }
        Some("user_message") | Some("agent_message") => {
            if let Some(turn) = current.as_mut() {
                turn.byte_end = line_end;
                turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
                if let Some(message) = value.pointer("/payload/message").and_then(Value::as_str) {
                    turn.display_parts.push(message.to_string());
                }
            }
        }
        _ => {
            if let Some(turn) = current.as_mut() {
                turn.byte_end = line_end;
                turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
            }
        }
    }
}

fn start_turn(
    value: &Value,
    line_start: u64,
    line_end: u64,
    timestamp: Option<String>,
) -> CurrentTurn {
    CurrentTurn {
        turn_id: value
            .pointer("/payload/turn_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        byte_start: line_start,
        byte_end: line_end,
        observed_at: timestamp,
        status: None,
        display_parts: Vec::new(),
    }
}

fn finish_turn(
    current: &mut Option<CurrentTurn>,
    turns: &mut Vec<TurnRecord>,
    session_id: Option<&str>,
    line_end: u64,
    timestamp: Option<String>,
    status: TurnStatus,
) {
    let Some(mut turn) = current.take() else {
        return;
    };
    let Some(session_id) = session_id else {
        *current = Some(turn);
        return;
    };

    turn.byte_end = line_end;
    turn.status = Some(status);
    turn.observed_at = timestamp.or(turn.observed_at);
    let display_text = compact_display_text(turn.display_parts);

    turns.push(TurnRecord {
        session_id: session_id.to_string(),
        turn_id: turn.turn_id,
        ordinal: turns.len() as u64,
        byte_start: turn.byte_start,
        byte_end: turn.byte_end,
        observed_at: turn.observed_at,
        status,
        display_text,
    });
}

fn extract_response_item_text(value: &Value, display_parts: &mut Vec<String>) {
    let payload = value.get("payload").unwrap_or(&Value::Null);
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            if let Some(items) = payload.get("content").and_then(Value::as_array) {
                for item in items {
                    if let Some(text) = item
                        .get("text")
                        .or_else(|| item.get("message"))
                        .and_then(Value::as_str)
                    {
                        display_parts.push(text.to_string());
                    }
                }
            }
        }
        Some("function_call") => {
            if let Some(name) = payload.get("name").and_then(Value::as_str) {
                display_parts.push(format!("tool call: {name}"));
            }
        }
        Some("function_call_output") => {
            if let Some(output) = payload.get("output").and_then(Value::as_str) {
                display_parts.push(format!("tool output: {}", truncate_chars(output, 500)));
            }
        }
        _ => {}
    }
}

fn compact_display_text(parts: Vec<String>) -> Option<String> {
    let text = parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let complete_len = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => index + 1,
        None => 0,
    };
    bytes[..complete_len].split_inclusive(|byte| *byte == b'\n')
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn parses_session_meta_and_completed_turn() {
        let input = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/tmp/yaaml"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"implement this"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
            "\n",
        );

        let parsed =
            parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.session.id, "session-1");
        assert_eq!(parsed.session.project_id, "/tmp/yaaml");
        assert_eq!(parsed.turns.len(), 1);
        let turn = &parsed.turns[0];
        assert_eq!(turn.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(turn.ordinal, 0);
        let expected_turn_start = input.lines().next().unwrap().len() + 1;
        assert_eq!(turn.byte_start, expected_turn_start as u64);
        assert_eq!(turn.byte_end, input.len() as u64);
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn
            .display_text
            .as_ref()
            .unwrap()
            .contains("implement this"));
        assert!(turn
            .display_text
            .as_ref()
            .unwrap()
            .contains("tool call: exec_command"));
    }

    #[test]
    fn parses_aborted_turn() {
        let input = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/yaaml"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-1"}}"#,
            "\n",
        );

        let parsed =
            parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].status, TurnStatus::Aborted);
    }

    #[test]
    fn ignores_incomplete_trailing_line() {
        let input = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/yaaml"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg""#
        );

        let parsed =
            parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.turns.len(), 0);
        assert!(parsed.next_offset < input.len() as u64);
    }

    #[test]
    fn does_not_advance_cursor_past_incomplete_turn() {
        let session = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/yaaml"}}"#,
            "\n"
        );
        let input = format!(
            "{}{}{}",
            session,
            concat!(
                r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                "\n"
            ),
            concat!(
                r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"remember this"}}"#,
                "\n"
            )
        );

        let parsed =
            parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.turns.len(), 0);
        assert_eq!(parsed.next_offset, session.len() as u64);
    }

    #[test]
    fn keeps_first_session_meta_as_transcript_identity() {
        let input = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"child-session","cwd":"/tmp/yaaml-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"session_meta","payload":{"id":"parent-session","cwd":"/tmp/yaaml-parent"}}"#,
            "\n"
        );

        let parsed = parse_codex_jsonl(Path::new("/tmp/child.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.session.id, "child-session");
        assert_eq!(parsed.session.project_id, "/tmp/yaaml-child");
        assert_eq!(
            parsed.session.transcript_file_path,
            "/tmp/child.jsonl".to_string()
        );
    }

    #[test]
    fn malformed_complete_line_reports_safe_offset() {
        let good = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","cwd":"/tmp/yaaml"}}"#,
            "\n"
        );
        let input = format!("{good}{{bad json}}\n");

        let error =
            parse_codex_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap_err();

        assert!(matches!(
            error,
            CodexParseError::MalformedJson { offset, .. } if offset == good.len() as u64
        ));
    }
}
