use std::fs;
use std::process::Command;

use tempfile::TempDir;
use yaaml_store::Database;

#[test]
fn ingest_json_reports_processed_codex_backlog() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let codex_root = tmp.path().join("sessions");
    let dated = codex_root.join("2026").join("06").join("08");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&dated).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
    )
    .unwrap();
    fs::write(dated.join("session.jsonl"), transcript()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("ingest")
        .arg("--codex-root")
        .arg(&codex_root)
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
    assert_eq!(value["discovered_files"], 1);
    assert_eq!(value["processed_files"], 1);
    assert_eq!(value["processed_turns"], 1);
}

#[test]
fn ingest_persists_display_text_without_rehydrating_transcript() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let codex_root = tmp.path().join("sessions");
    let dated = codex_root.join("2026").join("06").join("08");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&dated).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
    )
    .unwrap();
    let transcript_path = dated.join("session.jsonl");
    fs::write(&transcript_path, transcript()).unwrap();

    let output = run_ingest(&codex_root, &project, &home);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(transcript_path).unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let turns = db.turns_for_session("session-1", 1).unwrap();

    assert_eq!(
        turns[0].display_text.as_deref(),
        Some("hello"),
        "display text should be stored in SQLite, not only recoverable from transcript bytes"
    );
}

#[test]
fn ingest_json_reports_appended_cursored_codex_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let codex_root = tmp.path().join("sessions");
    let dated = codex_root.join("2026").join("06").join("08");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&dated).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
    )
    .unwrap();
    let transcript_path = dated.join("session.jsonl");
    fs::write(&transcript_path, transcript()).unwrap();

    let first = run_ingest(&codex_root, &project, &home);
    assert!(first.status.success());
    fs::write(
        &transcript_path,
        format!("{}{}", transcript(), completed_turn(2)),
    )
    .unwrap();

    let second = run_ingest(&codex_root, &project, &home);

    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(value["discovered_files"], 0);
    assert_eq!(value["changed_files"], 1);
    assert_eq!(value["processed_turns"], 1);
}

fn run_ingest(
    codex_root: &std::path::Path,
    project: &std::path::Path,
    home: &std::path::Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("ingest")
        .arg("--codex-root")
        .arg(codex_root)
        .arg("--json")
        .current_dir(project)
        .env("HOME", home)
        .output()
        .unwrap()
}

fn transcript() -> String {
    concat!(
        r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml"}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
        "\n"
    )
    .to_string()
}

fn completed_turn(ordinal: u64) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-{ordinal}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-06-08T00:00:02Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"turn {ordinal}"}}]}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-{ordinal}"}}}}"#,
            "\n"
        ),
        ordinal = ordinal
    )
}
