use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use tempfile::TempDir;
use yaaml::daemon::{
    ingest_codex_file, process_codex_backlog, process_codex_changes,
    queue_memory_formulation_if_due, queue_missing_memory_formulation_tasks, recover_running_tasks,
    refresh_recall_with_embedding, run_queued_tasks, start_signal_socket, DaemonShutdown,
    PartialBatchPolicy, TASK_KIND_MEMORY_FORMULATION,
};
use yaaml_core::{
    recall_file_path, Config, EmbeddingRecord, MemoryRecord, MemoryScope, SourceTurnRef, TurnRecord,
};
use yaaml_store::database::encode_f32_embedding;
use yaaml_store::Database;

#[test]
fn appending_codex_jsonl_turn_creates_turn_row_from_cursor() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    fs::write(&transcript, session_meta()).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    let first = ingest_codex_file(&db, &transcript).unwrap();
    assert_eq!(first.inserted_turns, 0);
    append(&transcript, &completed_turn(1));
    let second = ingest_codex_file(&db, &transcript).unwrap();

    assert_eq!(second.inserted_turns, 1);
    assert_eq!(db.completed_turn_count_for_session("session-1").unwrap(), 1);
    let third = ingest_codex_file(&db, &transcript).unwrap();
    assert_eq!(third.inserted_turns, 0);
    assert_eq!(db.completed_turn_count_for_session("session-1").unwrap(), 1);
}

#[test]
fn ingest_recovers_missing_session_row_for_existing_cursor() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    fs::write(&transcript, session_meta()).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.update_cursor(&transcript.display().to_string(), 10, None)
        .unwrap();

    let report = ingest_codex_file(&db, &transcript).unwrap();

    assert_eq!(report.inserted_turns, 0);
    assert!(db
        .session_by_transcript_path(&transcript.display().to_string())
        .unwrap()
        .is_some());
}

#[test]
fn codex_backlog_processing_discovers_and_ingests_uncursored_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let dated = root.join("2026").join("06").join("08");
    fs::create_dir_all(&dated).unwrap();
    fs::write(
        dated.join("session.jsonl"),
        format!("{}{}", session_meta(), completed_turn(1)),
    )
    .unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    let report = process_codex_backlog(&db, &Config::default(), &root).unwrap();

    assert_eq!(report.discovered_files, 1);
    assert_eq!(report.processed_files, 1);
    assert_eq!(report.processed_turns, 1);
    assert_eq!(db.status().unwrap().backlog.processed_files, 1);
}

#[test]
fn codex_change_processing_ingests_appended_cursored_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let dated = root.join("2026").join("06").join("08");
    fs::create_dir_all(&dated).unwrap();
    let transcript = dated.join("session.jsonl");
    fs::write(&transcript, session_meta()).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    append(&transcript, &completed_turn(1));

    let report = process_codex_changes(&db, &Config::default(), &root).unwrap();

    assert_eq!(report.scanned_files, 1);
    assert_eq!(report.changed_files, 1);
    assert_eq!(report.processed_turns, 1);
    assert_eq!(db.completed_turn_count_for_session("session-1").unwrap(), 1);
    assert_eq!(db.status().unwrap().backlog.processed_files, 1);
    assert_eq!(db.status().unwrap().backlog.processed_turns, 1);
}

#[test]
fn completing_enough_turns_queues_memory_creation() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    let mut contents = session_meta();
    for ordinal in 0..10 {
        contents.push_str(&completed_turn(ordinal));
    }
    fs::write(&transcript, contents).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    let config = Config::default();

    let task_id = queue_memory_formulation_if_due(&db, &config, "session-1", 10).unwrap();

    assert!(task_id.is_some());
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_FORMULATION, yaaml_core::TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn historical_memory_queue_batches_all_completed_turns() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    let mut contents = session_meta();
    for ordinal in 0..25 {
        contents.push_str(&completed_turn(ordinal));
    }
    fs::write(&transcript, contents).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    let mut config = Config::default();
    config.backlog_formulation_turn_window = 10;

    let queued =
        queue_missing_memory_formulation_tasks(&db, &config, 0, PartialBatchPolicy::Include)
            .unwrap();
    let queued_again =
        queue_missing_memory_formulation_tasks(&db, &config, 0, PartialBatchPolicy::Include)
            .unwrap();

    assert_eq!(queued, 3);
    assert_eq!(queued_again, 0);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_FORMULATION, yaaml_core::TaskStatus::Queued)
            .unwrap(),
        3
    );
}

#[test]
fn memory_queue_skips_already_covered_source_refs() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    let mut contents = session_meta();
    for ordinal in 0..12 {
        contents.push_str(&completed_turn(ordinal));
    }
    fs::write(&transcript, contents).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "covered".to_string(),
        body: "covered".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: (0..10)
            .map(|ordinal| SourceTurnRef {
                session_id: "session-1".to_string(),
                ordinal,
                byte_start: 0,
                byte_end: 1,
            })
            .collect(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: Some("session-1".to_string()),
        project_id: Some("/tmp/yaaml".to_string()),
        project_descriptor: Some("yaaml, Rust".to_string()),
        lineage_refs: Vec::new(),
    })
    .unwrap();
    let mut config = Config::default();
    config.backlog_formulation_turn_window = 10;

    let queued =
        queue_missing_memory_formulation_tasks(&db, &config, 0, PartialBatchPolicy::Include)
            .unwrap();

    assert_eq!(queued, 1);
}

#[test]
fn queued_memory_task_parks_when_provider_is_unavailable() {
    std::env::remove_var("YAAML_TEST_MISSING_ANTHROPIC_KEY");
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    fs::write(
        &transcript,
        format!("{}{}", session_meta(), completed_turn(1)),
    )
    .unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    db.enqueue_task(&yaaml_core::TaskRecord {
        id: None,
        kind: TASK_KIND_MEMORY_FORMULATION.to_string(),
        status: yaaml_core::TaskStatus::Queued,
        priority: 0,
        payload_json: serde_json::json!({"session_id":"session-1"}).to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    let mut config = Config::default();
    config.summary_api_key_env = "YAAML_TEST_MISSING_ANTHROPIC_KEY".to_string();

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 0);
    let status = db.status().unwrap();

    assert_eq!(status.parked_jobs, 1);
    assert_eq!(status.workers.queued_jobs, 0);
}

#[test]
fn recall_file_is_written_after_memory_exists_and_new_turn_completes() {
    let tmp = TempDir::new().unwrap();
    let mut config = Config::default();
    config.recall_dir = tmp.path().join("recall").display().to_string();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let project_id = project.canonicalize().unwrap().display().to_string();
    let memory = MemoryRecord {
        id: None,
        title: "Recall file location".to_string(),
        body: "Agents should use the YAAML skill to resolve the recall file.".to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust".to_string()),
        lineage_refs: Vec::new(),
    };
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: config.embedding_model.clone(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    let turn = TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 10,
        observed_at: None,
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("where is recall written?".to_string()),
    };

    refresh_recall_with_embedding(
        &db,
        &config,
        &project.canonicalize().unwrap(),
        &[turn],
        &[1.0, 0.0],
        "new completed turn",
    )
    .unwrap();
    let path = recall_file_path(
        &config.recall_dir().unwrap(),
        &project.canonicalize().unwrap(),
    );
    let markdown = fs::read_to_string(path).unwrap();

    assert!(markdown.contains("## Recall file location"));
}

#[test]
fn graceful_recovery_requeues_running_tasks() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let id = db
        .enqueue_task(&yaaml_core::TaskRecord {
            id: None,
            kind: TASK_KIND_MEMORY_FORMULATION.to_string(),
            status: yaaml_core::TaskStatus::Queued,
            priority: 0,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        })
        .unwrap();
    db.mark_task_running(id, "2026-06-08T00:00:01Z").unwrap();

    assert_eq!(recover_running_tasks(&db).unwrap(), 1);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_FORMULATION, yaaml_core::TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn signal_socket_requests_shutdown_and_removes_socket() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("daemon.sock");
    let shutdown = DaemonShutdown::default();
    let handle = start_signal_socket(&socket, shutdown.clone()).unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream.write_all(b"shutdown\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert_eq!(response, "ok\n");
    assert!(shutdown.is_requested());
    handle.join().unwrap().unwrap();
    assert!(!socket.exists());
}

fn session_meta() -> String {
    r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml"}}"#
        .to_string()
        + "\n"
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

fn append(path: &std::path::Path, contents: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
}
