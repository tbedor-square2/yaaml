use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub db_path: String,
    pub memory_count: u64,
    pub active_memory_count: u64,
    pub last_creation_at: Option<String>,
    pub last_recall_at: Option<String>,
    pub backlog: BacklogStatus,
    pub workers: WorkerStatus,
    pub recent_failures: Vec<TaskFailure>,
    pub parked_jobs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklogStatus {
    pub discovered_files: u64,
    pub processed_files: u64,
    pub processed_turns: u64,
    pub queued_memory_jobs: u64,
    pub failures: u64,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub active_workers: u64,
    pub queued_jobs: u64,
    pub running_jobs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFailure {
    pub task_kind: String,
    pub error: String,
    pub failed_at: String,
}
