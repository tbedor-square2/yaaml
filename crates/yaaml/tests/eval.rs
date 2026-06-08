use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

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
        format!(
            r#"
db_path = "{}"
embedding_api_key_env = "YAAML_TEST_MISSING_OPENAI_KEY"
"#,
            db_path.display()
        ),
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
        .arg("--no-judge")
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
    assert_eq!(value["score_counts"]["unjudged"], 1);

    let run_id = value["run_id"].as_i64().unwrap();
    let list = Command::new(binary)
        .arg("eval")
        .arg("list")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let runs: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(runs[0]["id"], run_id);
    assert_eq!(runs[0]["result_count"], 1);

    let show = Command::new(binary)
        .arg("eval")
        .arg("show")
        .arg(run_id.to_string())
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        show.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["run"]["id"], run_id);
    assert_eq!(shown["score_counts"]["unjudged"], 1);
    assert_eq!(shown["results"][0]["memory_title"], "Earlier memory");
}

#[test]
fn eval_recall_uses_mocked_anthropic_judge() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let server = fake_anthropic_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
embedding_api_key_env = "YAAML_TEST_MISSING_OPENAI_KEY"
eval_judge_base_url = "{}"
eval_judge_api_key_env = "YAAML_TEST_ANTHROPIC_KEY"
"#,
            db_path.display(),
            server.base_url
        ),
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
        .env("YAAML_TEST_ANTHROPIC_KEY", "test-key")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["score_counts"]["5"], 1);
}

struct FakeServer {
    base_url: String,
    handle: thread::JoinHandle<()>,
}

impl FakeServer {
    fn join(self) {
        self.handle.join().unwrap();
    }
}

fn fake_anthropic_server() -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 8192];
        let _ = stream.read(&mut buffer).unwrap();
        let body = r#"{"content":[{"type":"text","text":"{\"score\":\"5\",\"rationale\":\"directly relevant\"}"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    FakeServer { base_url, handle }
}
