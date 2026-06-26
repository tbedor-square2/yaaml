use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{infer_context_from_path, AgentType, SessionRecord, TurnRecord, TurnStatus};
use yaaml_store::Database;

#[test]
fn segments_backfill_json_reports_failure_details() {
    let fixture = SegmentBackfillFixture::new();

    let output = fixture.run_segments_backfill(&["--json"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["sessions_processed"], 1);
    assert_eq!(value["failures"], 1);
    assert_eq!(value["failure_details"][0]["session_id"], "bad-session");
    assert_eq!(
        value["failure_details"][0]["transcript_file_path"],
        fixture.bad_transcript_path.display().to_string()
    );
    assert!(value["failure_details"][0]["error"]
        .as_str()
        .unwrap()
        .contains("failed to hydrate segment turns"));
    assert!(value["failure_details"][0]["error"]
        .as_str()
        .unwrap()
        .contains("failed to read"));
}

#[test]
fn segments_backfill_human_output_reports_failure_details() {
    let fixture = SegmentBackfillFixture::new();

    let output = fixture.run_segments_backfill(&[]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("failures: 1"));
    assert!(stdout.contains("session=bad-session"));
    assert!(stdout.contains(&format!(
        "transcript={}",
        fixture.bad_transcript_path.display()
    )));
    assert!(stdout.contains("failed to hydrate segment turns"));
}

struct SegmentBackfillFixture {
    _tmp: TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    bad_transcript_path: std::path::PathBuf,
}

impl SegmentBackfillFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        let yaaml_dir = home.join(".yaaml");
        fs::create_dir_all(&yaaml_dir).unwrap();
        fs::create_dir_all(&project).unwrap();
        let db_path = yaaml_dir.join("yaaml.db");
        fs::write(
            yaaml_dir.join("config.toml"),
            format!(r#"db_path = "{}""#, db_path.display()),
        )
        .unwrap();
        let bad_transcript_path = tmp.path().join("missing-bad-session.jsonl");

        let mut db = Database::open(&db_path).unwrap();
        db.migrate().unwrap();
        insert_good_session(&db, &project);
        insert_bad_session(&db, &project, &bad_transcript_path);
        drop(db);

        Self {
            _tmp: tmp,
            home,
            project,
            bad_transcript_path,
        }
    }

    fn run_segments_backfill(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yaaml"));
        command
            .arg("segments")
            .arg("backfill")
            .current_dir(&self.project)
            .env("HOME", &self.home);
        for arg in args {
            command.arg(arg);
        }
        command.output().unwrap()
    }
}

fn insert_good_session(db: &Database, project: &Path) {
    db.upsert_session(&SessionRecord {
        id: "good-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project.display().to_string(),
        transcript_file_path: project
            .join("unused-good-session.jsonl")
            .display()
            .to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:01Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "good-session".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:01Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("user: continue improving YAAML segment backfill".to_string()),
        cwd: Some(project.display().to_string()),
        context: Some(infer_context_from_path(project)),
    })
    .unwrap();
}

fn insert_bad_session(db: &Database, project: &Path, bad_transcript_path: &Path) {
    db.upsert_session(&SessionRecord {
        id: "bad-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project.display().to_string(),
        transcript_file_path: bad_transcript_path.display().to_string(),
        started_at: Some("2026-06-08T00:01:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:01:01Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "bad-session".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:01:01Z".to_string()),
        status: TurnStatus::Completed,
        display_text: None,
        cwd: None,
        context: None,
    })
    .unwrap();
}
