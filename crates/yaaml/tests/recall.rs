use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use yaaml::daemon::TASK_KIND_RECALL_EVAL;
use yaaml_core::{
    recall_file_path, session_recall_file_path, AgentType, EmbeddingRecord, MemoryRecord,
    MemoryScope, SessionRecord, TaskStatus, TurnRecord, TurnStatus,
};
use yaaml_store::database::encode_f32_embedding;
use yaaml_store::Database;

#[test]
fn manual_recall_query_writes_expected_markdown() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_base_url = "{}"
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory = MemoryRecord {
        id: None,
        title: "Recall files".to_string(),
        body: "Agents should read the daemon-owned recall file through the skill.".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id.clone()),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .arg("--query")
        .arg("where should agents read recall")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Recall files"));
    assert!(stdout.contains("daemon-owned recall file"));
    server.join();
    let recall_path = recall_file_path(&recall_dir, &project.canonicalize().unwrap());
    let markdown = fs::read_to_string(recall_path).unwrap();

    assert!(markdown.contains("## Recall files"));
    assert!(markdown.contains("daemon-owned recall file"));
    assert!(markdown.contains(&format!("memory_ids: {memory_id}")));

    let output = Command::new(binary)
        .arg("recall")
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Recall files"));
    assert!(stdout.contains("daemon-owned recall file"));

    let output = Command::new(binary)
        .arg("recall")
        .current_dir(&project)
        .env("HOME", &home)
        .env("CODEX_THREAD_ID", "new-session-without-file")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Recall files"));
    assert!(stdout.contains("daemon-owned recall file"));
}

#[test]
fn recall_query_writes_current_session_file_when_session_id_is_available() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_base_url = "{}"
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory = MemoryRecord {
        id: None,
        title: "Session recall".to_string(),
        body: "Recall should be triggered from the current user request.".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    fs::create_dir_all(&recall_dir).unwrap();
    fs::write(
        recall_file_path(&recall_dir, &project.canonicalize().unwrap()),
        "# Stale Project Recall\n\nold project fallback",
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .arg("--query")
        .arg("current user request")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env("CODEX_THREAD_ID", "session-1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let recall_path =
        session_recall_file_path(&recall_dir, &project.canonicalize().unwrap(), "session-1");
    let markdown = fs::read_to_string(recall_path).unwrap();

    assert!(markdown.contains("## Session recall"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("## Session recall"));

    let db = Database::open(&db_path).unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn bare_recall_refreshes_missing_current_session_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_base_url = "{}"
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-without-recall-file".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/session-without-recall-file.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:03Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-without-recall-file".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start: 10,
        byte_end: 20,
        observed_at: Some("2026-06-08T00:00:03Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("agent should recall missing session files".to_string()),
    })
    .unwrap();
    let memory = MemoryRecord {
        id: None,
        title: "On-demand recall".to_string(),
        body: "Bare recall should generate a missing session recall file from recent turns."
            .to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env("CODEX_THREAD_ID", "session-without-recall-file")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## On-demand recall"));
    assert!(!stdout.contains("Stale Project Recall"));
    assert!(!stdout.contains("no recall file"));

    let recall_path = session_recall_file_path(
        &recall_dir,
        &project.canonicalize().unwrap(),
        "session-without-recall-file",
    );
    let markdown = fs::read_to_string(recall_path).unwrap();
    assert!(markdown.contains("## On-demand recall"));
    let db = Database::open(&db_path).unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn recall_query_falls_back_to_latest_project_session() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_base_url = "{}"
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
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
            project_id: project_id.clone(),
            transcript_file_path: "/tmp/new.jsonl".to_string(),
            started_at: Some("2026-06-08T00:00:00Z".to_string()),
            last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
        },
    ] {
        db.upsert_session(&session).unwrap();
    }
    let memory = MemoryRecord {
        id: None,
        title: "Fallback session recall".to_string(),
        body: "Recall should use the newest known project session without a Codex thread id."
            .to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .arg("--query")
        .arg("current user request")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let recall_path =
        session_recall_file_path(&recall_dir, &project.canonicalize().unwrap(), "new-session");
    let markdown = fs::read_to_string(recall_path).unwrap();

    assert!(markdown.contains("## Fallback session recall"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("## Fallback session recall"));

    let db = Database::open(&db_path).unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn historical_recall_uses_session_turn_context_without_writing_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_base_url = "{}"
recall_live_turn_window = 2
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "replay-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/replay.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
    })
    .unwrap();
    for ordinal in 0..3 {
        db.insert_turn(&TurnRecord {
            session_id: "replay-session".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal * 10,
            byte_end: ordinal * 10 + 9,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: Some(format!("completed context turn {ordinal}")),
        })
        .unwrap();
    }
    let memory = MemoryRecord {
        id: None,
        title: "Historical recall".to_string(),
        body: "Backtests can replay recall for a specific session turn.".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .arg("--session")
        .arg("replay-session")
        .arg("--turn")
        .arg("2")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_id"], "replay-session");
    assert_eq!(value["turn_ordinal"], 2);
    assert_eq!(value["selected_memory_ids"][0], memory_id);
    assert!(value["markdown"]
        .as_str()
        .unwrap()
        .contains("## Historical recall"));
    assert!(value["query_source"]
        .as_str()
        .unwrap()
        .contains("completed turns 1..=2"));

    let recall_path = session_recall_file_path(
        &recall_dir,
        &project.canonicalize().unwrap(),
        "replay-session",
    );
    assert!(!recall_path.exists());
    let db = Database::open(&db_path).unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        0
    );
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

fn fake_embedding_server() -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 8192];
        let _ = stream.read(&mut buffer).unwrap();
        let body = r#"{"data":[{"embedding":[1.0,0.0]}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    FakeServer { base_url, handle }
}
