use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{
    AgentType, MemoryRecord, MemoryScope, SessionRecord, SourceTurnRef, TaskRecord, TaskStatus,
    TurnRecord, TurnStatus,
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

#[test]
fn memories_health_classifies_failure_modes_and_actions() {
    let fixture = MemoryFixture::new();
    fixture.insert_session_with_turns(1);
    let wrong_context_memory_id = fixture.insert_memory_with(
        "Snowflake flag telemetry",
        "The APP_POS_PLAT.DEV_TOOLS.CLI_EXECUTIONS table stores CLI flags for a narrow sq-riskarbiter telemetry analysis.",
        true,
        yaaml_core::MemoryKind::ProjectFact,
        vec![
            "table:cli_executions".to_string(),
            "field:command_line".to_string(),
            "field:args".to_string(),
            "field:new_args".to_string(),
            "pattern:--json".to_string(),
            "tool:sq".to_string(),
        ],
    );
    let useful_memory_id = fixture.insert_memory_with(
        "Use tmux for long-running jobs",
        "When a long-running process must survive beyond the current interaction, start it in tmux and inspect it with capture-pane.",
        true,
        yaaml_core::MemoryKind::Workflow,
        vec!["tool:tmux".to_string()],
    );
    fixture.insert_eval_scores(
        wrong_context_memory_id,
        &[
            (
                "1",
                "The recalled context is unrelated to the active objective and has no connection to this task.",
            ),
            (
                "1",
                "This is the wrong context for the current project and provides no actionable guidance.",
            ),
            (
                "2",
                "The memory is from a different domain and does not help the current task.",
            ),
            (
                "1",
                "The recalled telemetry details are irrelevant to the active objective.",
            ),
            (
                "1",
                "There is a mismatch between this memory and the current coding task.",
            ),
        ],
    );
    fixture.insert_eval_scores(
        useful_memory_id,
        &[
            ("5", "Directly actionable and relevant."),
            ("4", "Relevant and concise."),
            ("5", "The tmux guidance helped preserve the running job."),
            ("5", "Actionable and exactly matched the task."),
            ("4", "Relevant durable workflow guidance."),
        ],
    );

    let output = fixture.command(["memories", "health", "--json", "--limit", "10"]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let memories = value["memories"].as_array().unwrap();
    let wrong_context = memories
        .iter()
        .find(|memory| memory["memory_id"] == wrong_context_memory_id)
        .unwrap();
    assert_eq!(wrong_context["failure_mode"], "wrong_context");
    assert_eq!(
        wrong_context["recommended_action"],
        "regenerate_metadata_or_tighten_gates"
    );
    let useful = memories
        .iter()
        .find(|memory| memory["memory_id"] == useful_memory_id)
        .unwrap();
    assert_eq!(useful["failure_mode"], "proven_useful");
    assert_eq!(useful["recommended_action"], "boost_or_keep_active");
}

#[test]
fn memories_health_hides_inactive_memories_by_default() {
    let fixture = MemoryFixture::new();
    fixture.insert_session_with_turns(1);
    let inactive_memory_id = fixture.insert_memory_with(
        "Inactive stale task",
        "A stale paused task that should not be shown in active health output.",
        false,
        yaaml_core::MemoryKind::TaskState,
        Vec::new(),
    );
    fixture.insert_eval_scores(
        inactive_memory_id,
        &[
            ("1", "This stale task state is unrelated."),
            ("1", "This stale task state is unrelated."),
            ("1", "This stale task state is unrelated."),
        ],
    );

    let default_output = fixture.command(["memories", "health", "--json"]);
    let included_output = fixture.command(["memories", "health", "--json", "--include-inactive"]);

    assert_success(&default_output);
    assert_success(&included_output);
    let default_value: serde_json::Value = serde_json::from_slice(&default_output.stdout).unwrap();
    let included_value: serde_json::Value =
        serde_json::from_slice(&included_output.stdout).unwrap();
    assert!(default_value["memories"].as_array().unwrap().is_empty());
    assert_eq!(
        included_value["memories"][0]["memory_id"],
        inactive_memory_id
    );
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
        self.insert_memory_with(
            "memory",
            "body",
            active,
            yaaml_core::MemoryKind::Lesson,
            Vec::new(),
        );
    }

    fn insert_memory_with(
        &self,
        title: &str,
        body: &str,
        active: bool,
        kind: yaaml_core::MemoryKind,
        task_keys: Vec<String>,
    ) -> i64 {
        let db = Database::open(&self.db_path).unwrap();
        db.insert_memory(&MemoryRecord {
            id: None,
            title: title.to_string(),
            body: body.to_string(),
            scope: MemoryScope::Project,
            kind,
            task_keys,
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
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap()
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

    fn insert_eval_scores(&self, memory_id: i64, scores: &[(&str, &str)]) {
        let db = Database::open(&self.db_path).unwrap();
        let turn_id = db
            .turn_row_id_for_session_ordinal("session-1", 0)
            .unwrap()
            .unwrap();
        let run_id = db.insert_eval_run("recall", "unix:20", "{}").unwrap();
        for (index, (score, rationale)) in scores.iter().enumerate() {
            db.insert_eval_result(
                run_id,
                turn_id,
                Some(memory_id),
                score,
                rationale,
                &format!("unix:{}", 21 + index),
            )
            .unwrap();
        }
        db.complete_eval_run(run_id, "unix:30").unwrap();
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
