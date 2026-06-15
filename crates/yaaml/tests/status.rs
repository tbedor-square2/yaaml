use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{MemoryKind, MemoryRecord, MemoryScope};
use yaaml_store::Database;

#[test]
fn status_json_emits_valid_json_for_empty_db() {
    let tmp = TempDir::new().unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("status")
        .arg("--json")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["memory_count"], 0);
    assert_eq!(value["backlog"]["discovered_files"], 0);
    assert_eq!(value["backlog"]["transcript_files"], 0);
    assert_eq!(value["backlog"]["stored_turns"], 0);
}

#[test]
fn status_human_output_reflects_database_state() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".yaaml")).unwrap();
    let db_path = home.join(".yaaml/yaaml.db");
    std::fs::write(
        home.join(".yaaml/config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
    )
    .unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "Status memory".to_string(),
        body: "Status should report active memories.".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some("/tmp/yaaml".to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("status")
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("YAAML status"));
    assert!(stdout.contains("memories: 1 active / 1 total"));
    assert!(stdout.contains("transcripts: 0 files tracked, 0 sessions, 0 stored turns"));
    assert!(
        stdout.contains("ingestion totals: 0 files discovered, 0 file passes, 0 turns inserted")
    );
}
