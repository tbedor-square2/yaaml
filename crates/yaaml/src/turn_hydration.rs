use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use yaaml_core::{
    infer_context_from_path, infer_context_from_text, merge_contexts, AgentType, ContextMetadata,
    SessionRecord, TurnRecord,
};
use yaaml_store::Database;
use yaaml_transcript::claude::{hydrate_claude_turn_bytes, parse_claude_jsonl};
use yaaml_transcript::codex::{hydrate_codex_turn_bytes, parse_codex_jsonl};
use yaaml_transcript::TurnHydration;

pub fn hydrate_turns(db: &Database, turns: &[TurnRecord]) -> Result<Vec<TurnRecord>> {
    let mut transcripts = HashMap::<String, TranscriptBytes>::new();
    let mut hydrated = Vec::with_capacity(turns.len());
    for turn in turns {
        hydrated.push(hydrate_turn(db, &mut transcripts, turn)?);
    }
    Ok(hydrated)
}

pub fn context_from_turns(
    turns: &[TurnRecord],
    fallback_project_id: &Path,
    query_text: &str,
) -> ContextMetadata {
    let mut context = turns
        .iter()
        .rev()
        .find_map(|turn| turn.context.clone())
        .unwrap_or_else(|| infer_context_from_path(fallback_project_id));
    for turn in turns.iter().rev().skip(1) {
        if let Some(turn_context) = &turn.context {
            merge_contexts(&mut context, turn_context.clone());
        }
    }
    merge_contexts(&mut context, infer_context_from_text(query_text));
    context
}

fn hydrate_turn(
    db: &Database,
    transcripts: &mut HashMap<String, TranscriptBytes>,
    turn: &TurnRecord,
) -> Result<TurnRecord> {
    if turn.display_text.is_some() && turn.cwd.is_some() && turn.context.is_some() {
        return Ok(turn.clone());
    }

    let transcript = match load_transcript(db, transcripts, &turn.session_id) {
        Ok(transcript) => transcript,
        Err(_) if turn.display_text.is_some() => return Ok(turn.clone()),
        Err(error) => return Err(error),
    };
    let start = usize::try_from(turn.byte_start).unwrap_or(usize::MAX);
    let end = usize::try_from(turn.byte_end).unwrap_or(usize::MAX);
    let Some(slice) = transcript
        .bytes
        .get(start.min(transcript.bytes.len())..end.min(transcript.bytes.len()))
    else {
        return Ok(turn.clone());
    };
    let hydrated = match hydrate_turn_slice(transcript, slice) {
        Ok(hydrated) => hydrated,
        Err(error) => {
            return hydrate_turn_from_full_parse(transcript, turn)
                .with_context(|| format!("failed to hydrate stored turn slice: {error:#}"));
        }
    };

    Ok(apply_hydration(turn, hydrated))
}

fn hydrate_turn_slice(transcript: &TranscriptBytes, slice: &[u8]) -> Result<TurnHydration> {
    match transcript.agent_type {
        AgentType::Codex => hydrate_codex_turn_bytes(slice).context("failed to hydrate Codex turn"),
        AgentType::ClaudeCode => {
            hydrate_claude_turn_bytes(slice).context("failed to hydrate Claude turn")
        }
    }
}

fn hydrate_turn_from_full_parse(
    transcript: &mut TranscriptBytes,
    turn: &TurnRecord,
) -> Result<TurnRecord> {
    if transcript.reparsed_turns.is_none() {
        let transcript_path = Path::new(&transcript.session.transcript_file_path);
        let parsed_turns = match transcript.agent_type {
            AgentType::Codex => {
                parse_codex_jsonl(transcript_path, &transcript.bytes, 0)
                    .context("failed to reparse Codex transcript")?
                    .turns
            }
            AgentType::ClaudeCode => {
                parse_claude_jsonl(transcript_path, &transcript.bytes, 0)
                    .context("failed to reparse Claude transcript")?
                    .turns
            }
        };
        transcript.reparsed_turns = Some(parsed_turns);
    }
    let Some(reparsed) = transcript.reparsed_turns.as_ref().and_then(|turns| {
        turns
            .iter()
            .find(|candidate| candidate.ordinal == turn.ordinal)
    }) else {
        return Ok(turn.clone());
    };
    Ok(apply_hydrated_turn(turn, reparsed))
}

fn apply_hydration(turn: &TurnRecord, hydration: TurnHydration) -> TurnRecord {
    let mut turn = turn.clone();
    if turn.display_text.is_none() {
        turn.display_text = hydration.display_text;
    }
    if turn.cwd.is_none() {
        turn.cwd = hydration.cwd;
    }
    if turn.context.is_none() {
        turn.context = hydration.context;
    }
    turn
}

fn apply_hydrated_turn(turn: &TurnRecord, hydrated: &TurnRecord) -> TurnRecord {
    let mut turn = turn.clone();
    if turn.display_text.is_none() {
        turn.display_text = hydrated.display_text.clone();
    }
    if turn.cwd.is_none() {
        turn.cwd = hydrated.cwd.clone();
    }
    if turn.context.is_none() {
        turn.context = hydrated.context.clone();
    }
    turn
}

fn load_transcript<'a>(
    db: &Database,
    transcripts: &'a mut HashMap<String, TranscriptBytes>,
    session_id: &str,
) -> Result<&'a mut TranscriptBytes> {
    if !transcripts.contains_key(session_id) {
        let session = db
            .session_by_id(session_id)
            .context("failed to load turn session")?
            .with_context(|| format!("session {session_id} not found"))?;
        let bytes = fs::read(&session.transcript_file_path)
            .with_context(|| format!("failed to read {}", session.transcript_file_path))?;
        transcripts.insert(
            session_id.to_string(),
            TranscriptBytes {
                agent_type: session.agent_type,
                bytes,
                session,
                reparsed_turns: None,
            },
        );
    }
    Ok(transcripts
        .get_mut(session_id)
        .expect("transcript cache entry inserted"))
}

struct TranscriptBytes {
    agent_type: AgentType,
    bytes: Vec<u8>,
    session: SessionRecord,
    reparsed_turns: Option<Vec<TurnRecord>>,
}
