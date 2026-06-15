use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{
    AgentType, MemoryKind, MemoryRecord, MemoryScope, SessionRecord, SourceTurnRef, TaskRecord,
    TaskStatus, TurnRecord, TurnStatus,
};
use yaaml_store::Database;

#[test]
fn memories_rebuild_deactivates_and_requeues_from_transcripts() {
    let fixture = MemoryFixture::new();
    fixture.insert_session_with_turns(10);
    fixture.insert_memory(true);
    fixture.insert_task("memory_formulation", TaskStatus::Queued);
    fixture.insert_task("memory_consolidation", TaskStatus::Parked);

    let output = fixture.command(["memories", "rebuild", "--yes"]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["deactivated_memories"], 1);
    assert_eq!(value["cleared_memory_tasks"], 2);
    assert_eq!(value["queued_memory_jobs"], 1);

    let db = Database::open(&fixture.db_path).unwrap();
    let memories = db.list_memories().unwrap();
    assert!(!memories[0].is_active);
    assert_eq!(
        db.count_tasks_by_status("memory_formulation", TaskStatus::Queued)
            .unwrap(),
        1
    );
    assert_eq!(
        db.count_tasks_by_status("memory_consolidation", TaskStatus::Parked)
            .unwrap(),
        0
    );
}

#[test]
fn memories_stats_reports_active_project_counts() {
    let fixture = MemoryFixture::new();
    fixture.insert_memory(true);
    fixture.insert_memory(false);

    let output = fixture.command(["memories", "stats", "--json"]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"], 2);
    assert_eq!(value["active"], 1);
    assert_eq!(value["top_projects"][0]["project_id"], "/tmp/project");
    assert_eq!(value["top_projects"][0]["active_count"], 1);
}

struct MemoryFixture {
    _tmp: TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    db_path: std::path::PathBuf,
}

impl MemoryFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        let db_path = home.join(".yaaml").join("yaaml.db");
        std::fs::create_dir_all(home.join(".yaaml")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join(".yaaml").join("config.toml"),
            format!(
                r#"
db_path = "{}"
backlog_formulation_turn_window = 10
"#,
                db_path.display()
            ),
        )
        .unwrap();
        let mut db = Database::open(&db_path).unwrap();
        db.migrate().unwrap();
        Self {
            _tmp: tmp,
            home,
            project,
            db_path,
        }
    }

    fn command<const N: usize>(&self, args: [&str; N]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_yaaml"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .output()
            .unwrap()
    }

    fn insert_session_with_turns(&self, count: u64) {
        let db = Database::open(&self.db_path).unwrap();
        db.upsert_session(&SessionRecord {
            id: "session-1".to_string(),
            agent_type: AgentType::Codex,
            project_id: "/tmp/project".to_string(),
            transcript_file_path: "/tmp/session-1.jsonl".to_string(),
            started_at: Some("unix:1".to_string()),
            last_seen_at: Some("unix:2".to_string()),
        })
        .unwrap();
        for ordinal in 0..count {
            db.insert_turn(&TurnRecord {
                session_id: "session-1".to_string(),
                turn_id: Some(format!("turn-{ordinal}")),
                ordinal,
                byte_start: ordinal,
                byte_end: ordinal + 1,
                observed_at: Some(format!("unix:{}", 10 + ordinal)),
                status: TurnStatus::Completed,
                display_text: Some(format!("turn {ordinal}")),
                cwd: None,
                context: None,
            })
            .unwrap();
        }
    }

    fn insert_memory(&self, active: bool) {
        let db = Database::open(&self.db_path).unwrap();
        db.insert_memory(&MemoryRecord {
            id: None,
            title: "memory".to_string(),
            body: "body".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: vec![SourceTurnRef {
                session_id: "session-1".to_string(),
                ordinal: 0,
                byte_start: 0,
                byte_end: 1,
            }],
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: active,
            session_id: Some("session-1".to_string()),
            project_id: Some("/tmp/project".to_string()),
            project_descriptor: Some("project".to_string()),
            lineage_refs: Vec::new(),
        })
        .unwrap();
    }

    fn insert_task(&self, kind: &str, status: TaskStatus) {
        let db = Database::open(&self.db_path).unwrap();
        db.enqueue_task(&TaskRecord {
            id: None,
            kind: kind.to_string(),
            status,
            priority: 0,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
        })
        .unwrap();
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
