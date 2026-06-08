use std::fs;
use std::process::Command;

use tempfile::TempDir;
use yaaml_core::{AgentType, MemoryRecord, MemoryScope, SessionRecord, TurnRecord, TurnStatus};
use yaaml_store::Database;

#[test]
fn eval_recall_json_emits_valid_result() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
    )
    .unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: project.display().to_string(),
        transcript_file_path: "/tmp/session.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:01Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
    })
    .unwrap();
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "Earlier memory".to_string(),
        body: "Useful context".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        updated_at: "2026-06-08T00:00:01Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project.display().to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("recall")
        .arg("--limit")
        .arg("1")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["evaluated_turns"], 1);
    assert_eq!(value["evaluated_memories"], 1);
}
