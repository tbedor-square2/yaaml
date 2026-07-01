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
    pub cwd: Option<String>,
    pub context: Option<crate::ContextMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationSegmentStatus {
    Active,
    Superseded,
    Completed,
    Abandoned,
}

impl ConversationSegmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSegmentRecord {
    pub id: Option<i64>,
    pub session_id: String,
    pub start_turn_ordinal: u64,
    pub end_turn_ordinal: u64,
    pub summary: String,
    pub task_keys: Vec<String>,
    pub context: Option<crate::ContextMetadata>,
    pub status: ConversationSegmentStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentLabelRecord {
    pub id: Option<i64>,
    pub normalized_label: String,
    pub label: String,
    pub kind: String,
    pub segment_count: u64,
    pub session_count: u64,
    pub project_count: u64,
    pub merged_into_label_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentLabelDraft {
    pub label: String,
    #[serde(default)]
    pub normalized_label: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSegmentLabelRecord {
    pub segment_id: i64,
    pub label_id: i64,
    pub normalized_label: String,
    pub label: String,
    pub kind: String,
    pub evidence: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryScope {
    Project,
    Global,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryKind {
    Preference,
    Lesson,
    Workflow,
    ProjectFact,
    TaskCheckpoint,
    TaskState,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Lesson => "lesson",
            Self::Workflow => "workflow",
            Self::ProjectFact => "project_fact",
            Self::TaskCheckpoint => "task_checkpoint",
            Self::TaskState => "task_state",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryValidity {
    Durable,
    ValidWhileSegmentActive,
}

impl MemoryValidity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::ValidWhileSegmentActive => "valid_while_segment_active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTurnRef {
    pub session_id: String,
    pub ordinal: u64,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: Option<i64>,
    pub title: String,
    pub body: String,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub task_keys: Vec<String>,
    pub source_turn_refs: Vec<SourceTurnRef>,
    pub created_at: String,
    pub updated_at: String,
    pub is_active: bool,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub project_descriptor: Option<String>,
    pub lineage_refs: Vec<i64>,
    pub origin_segment_id: Option<i64>,
    pub origin_segment_status: Option<ConversationSegmentStatus>,
    pub validity: MemoryValidity,
    pub superseded_by_memory_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub memory_id: i64,
    pub embedding_model: String,
    pub dimensions: u64,
    pub embedding_blob: Vec<u8>,
    pub embedded_text_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Queued,
    Running,
    Parked,
    Completed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Parked => "parked",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: Option<i64>,
    pub kind: String,
    pub status: TaskStatus,
    pub priority: i64,
    pub payload_json: String,
    pub attempts: u64,
    pub max_attempts: u64,
    pub next_run_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
