use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use yaaml_core::{MemoryScope, VectorIndex};
use yaaml_store::{Database, SqliteExactVectorIndex};

#[test]
fn remember_stores_memory_with_embedding_and_session_metadata() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let server = fake_embedding_server();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
embedding_base_url = "{}"
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("remember")
        .arg("--title")
        .arg("Prefer functional style")
        .arg("--body")
        .arg("When editing parser code, prefer iterator combinators over explicit loops unless the loop is clearer.")
        .arg("--scope")
        .arg("global")
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("remembered memory"));

    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memories = db.list_memories().unwrap();
    assert_eq!(memories.len(), 1);
    let memory = &memories[0];
    assert_eq!(memory.title, "Prefer functional style");
    assert_eq!(memory.scope, MemoryScope::Global);
    assert_eq!(memory.session_id.as_deref(), Some("session-1"));
    assert_eq!(
        memory.project_id.as_deref(),
        Some(project.canonicalize().unwrap().to_str().unwrap())
    );

    let index =
        SqliteExactVectorIndex::new(&db, "text-embedding-3-small".to_string(), "now".to_string());
    let hits = index.search(&[1.0, 0.0], 5, 0.0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id, memory.id.unwrap());
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
