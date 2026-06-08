use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use yaaml_core::{recall_file_path, EmbeddingRecord, MemoryRecord, MemoryScope};
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
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
