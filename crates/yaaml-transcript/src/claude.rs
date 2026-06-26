use std::fs;
use std::path::{Path, PathBuf};

use crate::TurnHydration;
use serde_json::Value;
use thiserror::Error;
use yaaml_core::{
    infer_context_from_path, paths::normalize_project_id, AgentType, ContextMetadata,
    SessionRecord, TurnRecord, TurnStatus,
};

#[derive(Debug, Error)]
pub enum ClaudeParseError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("malformed Claude JSONL at byte offset {offset}: {source}")]
    MalformedJson {
        offset: u64,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedClaudeChunk {
    pub session: SessionRecord,
    pub turns: Vec<TurnRecord>,
    pub next_offset: u64,
}

#[derive(Debug, Clone)]
struct CurrentTurn {
    byte_start: u64,
    byte_end: u64,
    observed_at: Option<String>,
    cwd: Option<String>,
    context: Option<ContextMetadata>,
    display_parts: Vec<String>,
}

pub fn discover_claude_transcripts(root: &Path) -> Result<Vec<PathBuf>, ClaudeParseError> {
    let mut files = Vec::new();
    visit(root, &mut files)?;
    files.sort();
    Ok(files)
}

pub fn parse_claude_file_from_offset(
    path: impl AsRef<Path>,
    start_offset: u64,
) -> Result<ParsedClaudeChunk, ClaudeParseError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| ClaudeParseError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let start = usize::try_from(start_offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    parse_claude_jsonl(path, &bytes[start..], start_offset)
}

pub fn parse_claude_jsonl(
    transcript_path: &Path,
    bytes: &[u8],
    start_offset: u64,
) -> Result<ParsedClaudeChunk, ClaudeParseError> {
    let session_id = transcript_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-claude-session")
        .to_string();
    let mut session = SessionRecord {
        id: session_id.clone(),
        agent_type: AgentType::ClaudeCode,
        project_id: infer_project_id(transcript_path),
        transcript_file_path: transcript_path.display().to_string(),
        started_at: None,
        last_seen_at: None,
    };
    let mut current: Option<CurrentTurn> = None;
    let mut current_cwd: Option<String> = None;
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
            serde_json::from_slice(line).map_err(|source| ClaudeParseError::MalformedJson {
                offset: line_start,
                source,
            })?;
        let timestamp = value
            .get("timestamp")
            .or_else(|| value.get("created_at"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if session.started_at.is_none() {
            session.started_at = timestamp.clone();
        }
        session.last_seen_at = timestamp.clone().or_else(|| session.last_seen_at.clone());
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            session.project_id = normalized_project_id(cwd);
            current_cwd = Some(cwd.to_string());
            if let Some(turn) = current.as_mut() {
                turn.cwd = Some(cwd.to_string());
                turn.context = Some(infer_context_from_path(Path::new(cwd)));
            }
        }
        let role = value
            .get("role")
            .or_else(|| value.pointer("/message/role"))
            .and_then(Value::as_str);
        let content = value
            .get("content")
            .or_else(|| value.pointer("/message/content"));

        match role {
            Some("user") => {
                let is_tool_result = content_has_type(content, "tool_result");
                if current.is_none() || !is_tool_result {
                    current = Some(CurrentTurn {
                        byte_start: line_start,
                        byte_end: line_end,
                        observed_at: timestamp.clone(),
                        cwd: current_cwd.clone(),
                        context: current_cwd
                            .as_deref()
                            .map(|cwd| infer_context_from_path(Path::new(cwd))),
                        display_parts: Vec::new(),
                    });
                }
                if let Some(turn) = current.as_mut() {
                    turn.byte_end = line_end;
                    turn.observed_at = timestamp.or_else(|| turn.observed_at.clone());
                    extract_content_text(Some("user"), content, &mut turn.display_parts);
                }
            }
            Some("assistant") => {
                if current.is_none() {
                    current = Some(CurrentTurn {
                        byte_start: line_start,
                        byte_end: line_end,
                        observed_at: timestamp.clone(),
                        cwd: current_cwd.clone(),
                        context: current_cwd
                            .as_deref()
                            .map(|cwd| infer_context_from_path(Path::new(cwd))),
                        display_parts: Vec::new(),
                    });
                }
                if let Some(turn) = current.as_mut() {
                    turn.byte_end = line_end;
                    turn.observed_at = timestamp.clone().or_else(|| turn.observed_at.clone());
                    extract_content_text(Some("assistant"), content, &mut turn.display_parts);
                }
                if content_has_text(content) && !content_has_type(content, "tool_use") {
                    finish_turn(&mut current, &mut turns, &session_id, line_end, timestamp);
                }
            }
            _ => {}
        }
        next_offset = line_end;
    }

    Ok(ParsedClaudeChunk {
        session,
        turns,
        next_offset,
    })
}

fn finish_turn(
    current: &mut Option<CurrentTurn>,
    turns: &mut Vec<TurnRecord>,
    session_id: &str,
    line_end: u64,
    timestamp: Option<String>,
) {
    let Some(mut turn) = current.take() else {
        return;
    };
    turn.byte_end = line_end;
    turn.observed_at = timestamp.or(turn.observed_at);
    turns.push(TurnRecord {
        session_id: session_id.to_string(),
        turn_id: None,
        ordinal: turns.len() as u64,
        byte_start: turn.byte_start,
        byte_end: turn.byte_end,
        observed_at: turn.observed_at,
        status: TurnStatus::Completed,
        display_text: (!turn.display_parts.is_empty()).then(|| turn.display_parts.join("\n")),
        cwd: turn.cwd,
        context: turn.context,
    });
}

pub fn hydrate_claude_turn_bytes(bytes: &[u8]) -> Result<TurnHydration, ClaudeParseError> {
    let mut display_parts = Vec::new();
    let mut cwd = None;
    let mut context = None;
    let mut line_offset = 0_u64;
    for raw_line in complete_lines(bytes) {
        let line_start = line_offset;
        line_offset += u64::try_from(raw_line.len()).unwrap_or(u64::MAX);
        let line = trim_line_ending(raw_line);
        if line.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_slice(line).map_err(|source| ClaudeParseError::MalformedJson {
                offset: line_start,
                source,
            })?;
        if let Some(line_cwd) = value.get("cwd").and_then(Value::as_str) {
            cwd = Some(line_cwd.to_string());
            context = Some(infer_context_from_path(Path::new(line_cwd)));
        }
        let role = value
            .get("role")
            .or_else(|| value.pointer("/message/role"))
            .and_then(Value::as_str);
        let content = value
            .get("content")
            .or_else(|| value.pointer("/message/content"));
        extract_content_text(role, content, &mut display_parts);
    }
    Ok(TurnHydration {
        display_text: (!display_parts.is_empty()).then(|| display_parts.join("\n")),
        cwd,
        context,
    })
}

pub fn display_text_from_claude_turn_bytes(
    bytes: &[u8],
) -> Result<Option<String>, ClaudeParseError> {
    hydrate_claude_turn_bytes(bytes).map(|hydration| hydration.display_text)
}

fn extract_content_text(role: Option<&str>, content: Option<&Value>, output: &mut Vec<String>) {
    match content {
        Some(Value::String(text)) => output.push(prefixed_display_text(role, text)),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    output.push(prefixed_display_text(role, text));
                } else if let Some(name) = item.get("name").and_then(Value::as_str) {
                    output.push(format!("tool: {name}"));
                } else if let Some(content) = item.get("content").and_then(Value::as_str) {
                    output.push(format!("tool output: {}", single_line_tool_output(content)));
                }
            }
        }
        _ => {}
    }
}

fn prefixed_display_text(role: Option<&str>, text: &str) -> String {
    let Some(role) = role else {
        return text.to_string();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("{role}: {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn single_line_tool_output(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn content_has_text(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => items
            .iter()
            .any(|item| item.get("text").and_then(Value::as_str).is_some()),
        _ => false,
    }
}

fn content_has_type(content: Option<&Value>, expected: &str) -> bool {
    matches!(content, Some(Value::Array(items)) if items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some(expected)))
}

fn infer_project_id(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

fn normalized_project_id(cwd: &str) -> String {
    normalize_project_id(Path::new(cwd)).display().to_string()
}

fn visit(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), ClaudeParseError> {
    if path
        .components()
        .any(|component| component.as_os_str() == "subagents")
    {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|source| ClaudeParseError::Read {
        path: path.display().to_string(),
        source,
    })?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|source| ClaudeParseError::Read {
            path: path.display().to_string(),
            source,
        })? {
            visit(
                &entry
                    .map_err(|source| ClaudeParseError::Read {
                        path: path.display().to_string(),
                        source,
                    })?
                    .path(),
                files,
            )?;
        }
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        files.push(path.to_path_buf());
    }
    Ok(())
}

fn complete_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&bytes[start..=index]);
            start = index + 1;
        }
    }
    lines
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn main_claude_session_parses_turn_pair() {
        let path = Path::new("/tmp/project/session-1.jsonl");
        let input = concat!(
            r#"{"timestamp":"2026-06-08T00:00:00Z","role":"user","content":"help"}"#,
            "\n",
            r#"{"timestamp":"2026-06-08T00:00:01Z","role":"assistant","content":[{"type":"text","text":"done"}]}"#,
            "\n",
        );

        let parsed = parse_claude_jsonl(path, input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.session.agent_type, AgentType::ClaudeCode);
        assert_eq!(parsed.turns.len(), 1);
        assert!(parsed.turns[0]
            .display_text
            .as_ref()
            .unwrap()
            .contains("done"));
    }

    #[test]
    fn assistant_text_only_message_completes_turn() {
        let input = concat!(
            r#"{"role":"user","content":"question"}"#,
            "\n",
            r#"{"role":"assistant","content":"answer"}"#,
            "\n",
        );

        let parsed =
            parse_claude_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.turns.len(), 1);
        assert!(parsed.turns[0]
            .display_text
            .as_ref()
            .unwrap()
            .contains("answer"));
    }

    #[test]
    fn tool_use_and_tool_result_stay_in_same_turn() {
        let input = concat!(
            r#"{"role":"user","content":"read file"}"#,
            "\n",
            r#"{"role":"assistant","content":[{"type":"tool_use","name":"Read","id":"tool-1"}]}"#,
            "\n",
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"file\ncontents"}]}"#,
            "\n",
            r#"{"role":"assistant","content":[{"type":"text","text":"summarized"}]}"#,
            "\n",
        );

        let parsed =
            parse_claude_jsonl(Path::new("/tmp/session.jsonl"), input.as_bytes(), 0).unwrap();

        assert_eq!(parsed.turns.len(), 1);
        let text = parsed.turns[0].display_text.as_ref().unwrap();
        assert!(text.contains("tool: Read"));
        assert!(text.contains("tool output: file contents"));
        assert!(!text.contains("file\ncontents"));
        assert!(text.contains("summarized"));
    }

    #[test]
    fn subagents_files_are_ignored_by_default() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("projects").join("encoded");
        let subagent = main.join("subagents");
        fs::create_dir_all(&subagent).unwrap();
        fs::write(main.join("session.jsonl"), "").unwrap();
        fs::write(subagent.join("child.jsonl"), "").unwrap();

        let files = discover_claude_transcripts(&tmp.path().join("projects")).unwrap();

        assert_eq!(files, vec![main.join("session.jsonl")]);
    }
}
