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
fn memories_list_filters_by_kind_and_active_state() {
    let fixture = MemoryFixture::new();
    let checkpoint_id = fixture.insert_memory_with(
        "PR checkpoint",
        "PR 481245 has unresolved reviewer follow-up.",
        true,
        yaaml_core::MemoryKind::TaskCheckpoint,
        vec!["pr:481245".to_string()],
    );
    fixture.insert_memory_with(
        "Inactive checkpoint",
        "PR 111111 was superseded.",
        false,
        yaaml_core::MemoryKind::TaskCheckpoint,
        vec!["pr:111111".to_string()],
    );
    fixture.insert_memory_with(
        "Durable lesson",
        "Use focused memory list filters for corpus audits.",
        true,
        yaaml_core::MemoryKind::Lesson,
        Vec::new(),
    );

    let output = fixture.command([
        "memories",
        "list",
        "--kind",
        "task_checkpoint",
        "--query",
        "reviewer",
        "--json",
    ]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let memories = value.as_array().unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0]["memory_id"], checkpoint_id);
    assert_eq!(memories[0]["kind"], "task_checkpoint");
    assert_eq!(memories[0]["active"], true);
    assert_eq!(memories[0]["task_keys"][0], "pr:481245");

    let output = fixture.command(["memories", "list", "--kind", "task_checkpoint"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1. id="));
    assert!(stdout.contains("kind=task_checkpoint"));
    assert!(!stdout.contains("Inactive checkpoint"));
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
    let mixed_wrong_context_memory_id = fixture.insert_memory_with(
        "Safe cutover validation",
        "When enabling a new generator, validate staging, use a percentage rollout, and monitor production metrics before a full cutover.",
        true,
        yaaml_core::MemoryKind::Workflow,
        Vec::new(),
    );
    let stale_task_state_memory_id = fixture.insert_memory_with(
        "Current PR status",
        "The current PR is ready for review after the final local checks finish.",
        true,
        yaaml_core::MemoryKind::TaskState,
        vec!["pr:123".to_string()],
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
    fixture.insert_eval_scores(
        mixed_wrong_context_memory_id,
        &[
            ("5", "Useful for the original cutover task."),
            (
                "2",
                "The recalled context is tangential and not directly actionable for the cleanup task.",
            ),
        ],
    );
    fixture.insert_eval_scores(
        stale_task_state_memory_id,
        &[
            ("1", "The task-state memory is stale."),
            ("1", "The current PR status is obsolete."),
            ("2", "This stale task state is unrelated."),
            ("1", "The remembered status no longer applies."),
            ("1", "The old task state is irrelevant."),
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
    let mixed_wrong_context = memories
        .iter()
        .find(|memory| memory["memory_id"] == mixed_wrong_context_memory_id)
        .unwrap();
    assert_eq!(
        mixed_wrong_context["failure_mode"],
        "context_sensitive_wrong_context"
    );
    assert_eq!(
        mixed_wrong_context["recommended_action"],
        "require_strong_task_match"
    );
    let stale_task_state = memories
        .iter()
        .find(|memory| memory["memory_id"] == stale_task_state_memory_id)
        .unwrap();
    assert_eq!(stale_task_state["failure_mode"], "stale_task_state");
    assert_eq!(stale_task_state["recommended_action"], "move_to_dormant");
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

#[test]
fn memories_apply_health_deactivates_only_high_confidence_actions() {
    let fixture = MemoryFixture::new();
    fixture.insert_session_with_turns(1);
    let stale_memory_id = fixture.insert_memory_with(
        "Current PR is ready for review",
        "The current PR is ready for review and only needs a final push before the task is done.",
        true,
        yaaml_core::MemoryKind::TaskState,
        vec!["pr:123".to_string()],
    );
    let low_value_memory_id = fixture.insert_memory_with(
        "Verbose recall quality note",
        "This memory has repeatedly failed to provide value in recall evaluations. It contains enough detail to avoid being classified as vague, but the content is generic and has not helped the agent choose a better action across many judged recall attempts. The extra sentences make this a substantive but consistently low-value memory rather than a short under-contextualized note.",
        true,
        yaaml_core::MemoryKind::Lesson,
        Vec::new(),
    );
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
    let vague_memory_id = fixture.insert_memory_with(
        "Agent tools portal",
        "go/agent-tools is useful.",
        true,
        yaaml_core::MemoryKind::Lesson,
        Vec::new(),
    );
    let useful_memory_id = fixture.insert_memory_with(
        "Use tmux for long-running jobs",
        "When a long-running process must survive beyond the current interaction, start it in tmux and inspect it with capture-pane.",
        true,
        yaaml_core::MemoryKind::Workflow,
        vec!["tool:tmux".to_string()],
    );
    fixture.insert_eval_scores(
        stale_memory_id,
        &[
            ("1", "The stale task state is unrelated."),
            ("1", "This old PR status is obsolete."),
            ("2", "The task-state memory is stale."),
            ("1", "This current PR note no longer applies."),
            ("1", "The remembered status is irrelevant."),
        ],
    );
    fixture.insert_eval_scores(
        low_value_memory_id,
        &[
            ("1", "Not useful."),
            ("2", "Not actionable."),
            ("1", "Too generic."),
            ("2", "Did not help."),
            ("1", "No useful signal."),
        ],
    );
    fixture.insert_eval_scores(
        wrong_context_memory_id,
        &[
            (
                "1",
                "The recalled context is unrelated to the active objective.",
            ),
            ("1", "This is the wrong context for the current project."),
            ("2", "The memory is from a different domain."),
            ("1", "The recalled telemetry details are irrelevant."),
            ("1", "There is a mismatch with this coding task."),
        ],
    );
    fixture.insert_eval_scores(
        vague_memory_id,
        &[
            ("1", "Too vague."),
            ("1", "Under-contextualized."),
            ("2", "Not actionable."),
            ("1", "No useful signal."),
            ("1", "Too generic."),
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

    let output = fixture.command([
        "memories",
        "apply-health",
        "--yes",
        "--json",
        "--limit",
        "10",
    ]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["applied_memories"], 4);
    let applied_ids = value["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|memory| memory["memory_id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert!(applied_ids.contains(&stale_memory_id));
    assert!(applied_ids.contains(&low_value_memory_id));
    assert!(applied_ids.contains(&wrong_context_memory_id));
    assert!(applied_ids.contains(&vague_memory_id));
    assert!(!applied_ids.contains(&useful_memory_id));

    let memories = Database::open(&fixture.db_path)
        .unwrap()
        .list_memories_by_ids(&[
            stale_memory_id,
            low_value_memory_id,
            wrong_context_memory_id,
            vague_memory_id,
            useful_memory_id,
        ])
        .unwrap();
    let active = |memory_id| {
        memories
            .iter()
            .find(|memory| memory.id == Some(memory_id))
            .unwrap()
            .is_active
    };
    assert!(!active(stale_memory_id));
    assert!(!active(low_value_memory_id));
    assert!(!active(wrong_context_memory_id));
    assert!(!active(vague_memory_id));
    assert!(active(useful_memory_id));
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
