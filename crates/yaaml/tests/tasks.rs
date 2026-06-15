use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{TaskRecord, TaskStatus};
use yaaml_store::Database;

#[test]
fn tasks_list_uses_scheduled_display_status_for_future_queued_tasks() {
    let fixture = TaskFixture::new();
    fixture.insert_task(task(
        "recall_eval",
        TaskStatus::Queued,
        Some("unix:9999999999"),
        None,
    ));

    let output = fixture.command(["tasks", "list", "--status", "scheduled", "--json"]);

    assert_success(&output);
    let tasks: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["status"], "queued");
    assert_eq!(tasks[0]["display_status"], "scheduled");
}

#[test]
fn tasks_retry_requeues_parked_task() {
    let fixture = TaskFixture::new();
    let task_id = fixture.insert_task(task(
        "recall",
        TaskStatus::Parked,
        None,
        Some("provider transport failure"),
    ));

    let output = fixture.command(["tasks", "retry", &task_id.to_string()]);

    assert_success(&output);
    let task = fixture.task(task_id);
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(task.attempts, 0);
    assert_eq!(task.next_run_at, None);
    assert_eq!(task.last_error, None);
}

#[test]
fn tasks_clear_deletes_parked_tasks_when_confirmed() {
    let fixture = TaskFixture::new();
    fixture.insert_task(task(
        "recall_eval",
        TaskStatus::Parked,
        None,
        Some("old eval failure"),
    ));
    fixture.insert_task(task(
        "recall_eval",
        TaskStatus::Queued,
        Some("unix:9999999999"),
        None,
    ));

    let output = fixture.command(["tasks", "clear", "--status", "parked", "--yes"]);

    assert_success(&output);
    assert_eq!(fixture.tasks("parked").as_array().unwrap().len(), 0);
    assert_eq!(fixture.tasks("scheduled").as_array().unwrap().len(), 1);
}

struct TaskFixture {
    _tmp: TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    db_path: std::path::PathBuf,
}

impl TaskFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        let db_path = home.join(".yaaml").join("yaaml.db");
        std::fs::create_dir_all(home.join(".yaaml")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join(".yaaml").join("config.toml"),
            format!(r#"db_path = "{}""#, db_path.display()),
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

    fn insert_task(&self, task: TaskRecord) -> i64 {
        Database::open(&self.db_path)
            .unwrap()
            .enqueue_task(&task)
            .unwrap()
    }

    fn task(&self, task_id: i64) -> TaskRecord {
        Database::open(&self.db_path)
            .unwrap()
            .list_tasks(None, 100)
            .unwrap()
            .into_iter()
            .find(|task| task.id == task_id)
            .map(|task| TaskRecord {
                id: Some(task.id),
                kind: task.kind,
                status: match task.status.as_str() {
                    "running" => TaskStatus::Running,
                    "parked" => TaskStatus::Parked,
                    "completed" => TaskStatus::Completed,
                    _ => TaskStatus::Queued,
                },
                priority: task.priority,
                payload_json: task.payload_json,
                attempts: task.attempts,
                max_attempts: task.max_attempts,
                next_run_at: task.next_run_at,
                last_error: task.last_error,
                created_at: task.created_at,
                updated_at: task.updated_at,
            })
            .unwrap()
    }

    fn tasks(&self, status: &str) -> serde_json::Value {
        let output = self.command(["tasks", "list", "--status", status, "--json"]);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn command<const N: usize>(&self, args: [&str; N]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_yaaml"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .output()
            .unwrap()
    }
}

fn task(
    kind: &str,
    status: TaskStatus,
    next_run_at: Option<&str>,
    last_error: Option<&str>,
) -> TaskRecord {
    TaskRecord {
        id: None,
        kind: kind.to_string(),
        status,
        priority: 0,
        payload_json: "{}".to_string(),
        attempts: u64::from(status == TaskStatus::Parked),
        max_attempts: 5,
        next_run_at: next_run_at.map(str::to_string),
        last_error: last_error.map(str::to_string),
        created_at: "unix:1".to_string(),
        updated_at: "unix:1".to_string(),
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
