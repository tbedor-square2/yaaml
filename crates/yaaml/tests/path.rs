use std::fs;
use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{recall_file_path, session_recall_file_path, AgentType, SessionRecord};
use yaaml_store::Database;

#[test]
fn path_prints_current_project_recall_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let recall_dir = home.join(".yaaml").join("recall");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"recall_dir = "{}""#, recall_dir.display()),
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("path")
        .current_dir(&project)
        .env("HOME", &home)
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = recall_file_path(&recall_dir, &project.canonicalize().unwrap());

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected.display().to_string()
    );
}

#[test]
fn path_prefers_current_session_recall_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let recall_dir = home.join(".yaaml").join("recall");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"recall_dir = "{}""#, recall_dir.display()),
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("path")
        .current_dir(&project)
        .env("HOME", &home)
        .env("CODEX_THREAD_ID", "session-1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected =
        session_recall_file_path(&recall_dir, &project.canonicalize().unwrap(), "session-1");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected.display().to_string()
    );
}

#[test]
fn path_falls_back_to_latest_project_session_without_codex_thread_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let recall_dir = home.join(".yaaml").join("recall");
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
"#,
            db_path.display(),
            recall_dir.display()
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    for session in [
        SessionRecord {
            id: "old-session".to_string(),
            agent_type: AgentType::Codex,
            project_id: project_id.clone(),
            transcript_file_path: "/tmp/old.jsonl".to_string(),
            started_at: Some("2026-06-08T00:00:00Z".to_string()),
            last_seen_at: Some("2026-06-08T00:01:00Z".to_string()),
        },
        SessionRecord {
            id: "new-session".to_string(),
            agent_type: AgentType::Codex,
            project_id,
            transcript_file_path: "/tmp/new.jsonl".to_string(),
            started_at: Some("2026-06-08T00:00:00Z".to_string()),
            last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
        },
    ] {
        db.upsert_session(&session).unwrap();
    }

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("path")
        .current_dir(&project)
        .env("HOME", &home)
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected =
        session_recall_file_path(&recall_dir, &project.canonicalize().unwrap(), "new-session");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected.display().to_string()
    );
}
