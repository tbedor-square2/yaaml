use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    Codex,
    ClaudeCode,
}

impl AgentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub agent_type: AgentType,
    pub project_id: String,
    pub transcript_file_path: String,
    pub started_at: Option<String>,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Completed,
    Aborted,
}

impl TurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub ordinal: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub observed_at: Option<String>,
    pub status: TurnStatus,
    pub display_text: Option<String>,
}
