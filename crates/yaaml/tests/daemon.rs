use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use yaaml::daemon::{
    dedupe_active_memories, ingest_codex_file, process_codex_backlog, process_codex_changes,
    queue_memory_consolidation_if_due, queue_memory_formulation_if_due,
    queue_missing_memory_formulation_tasks, queue_stale_recall_eval_tasks, recover_running_tasks,
    refresh_recall_with_embedding, run_queued_tasks, start_signal_socket, DaemonShutdown,
    PartialBatchPolicy, TASK_KIND_MEMORY_CONSOLIDATION, TASK_KIND_MEMORY_FORMULATION,
    TASK_KIND_RECALL, TASK_KIND_RECALL_EVAL,
};
use yaaml::turn_hydration::hydrate_turns;
use yaaml_core::{
    session_recall_file_path, AgentType, Config, EmbeddingRecord, MemoryRecord, MemoryScope,
    SessionRecord, SourceTurnRef, TaskRecord, TaskStatus, TurnRecord,
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
fn codex_change_processing_queues_and_runs_background_recall() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let recall_dir = tmp.path().join("recall");
    let dated = root.join("2026").join("06").join("08");
    fs::create_dir_all(&dated).unwrap();
    let transcript = dated.join("session.jsonl");
    fs::write(&transcript, session_meta()).unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();

    let server = fake_embedding_server();
    let mut config = Config {
        recall_dir: recall_dir.display().to_string(),
        embedding_base_url: Some(server.base_url.clone()),
        embedding_api_key_env: "YAAML_TEST_DAEMON_RECALL_KEY".to_string(),
        ..Config::default()
    };
    config.recall_candidate_pool = 5;
    config.turns_between_memory = 100;
    std::env::set_var("YAAML_TEST_DAEMON_RECALL_KEY", "test-key");
    let memory_id = db
        .insert_memory(&memory(
            "Background recall",
            "Completed transcript turns should refresh the session recall file.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: config.embedding_model.clone(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: "hash".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    append(&transcript, &completed_turn(1));

    let report = process_codex_changes(&db, &config, &root).unwrap();

    assert_eq!(report.processed_turns, 1);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL, TaskStatus::Queued)
            .unwrap(),
        1
    );

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    let path = session_recall_file_path(&recall_dir, "session-1");
    let markdown = fs::read_to_string(path).unwrap();

    assert!(markdown.contains("## Background recall"));
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
    server.join();
    std::env::remove_var("YAAML_TEST_DAEMON_RECALL_KEY");
}

#[test]
fn codex_cursor_waits_for_incomplete_turn_before_advancing() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    let session = session_meta();
    fs::write(
        &transcript,
        format!(
            "{}{}{}",
            session,
            r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            "\n"
        ),
    )
    .unwrap();
    append(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-06-08T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"prefer functional style"}}"#,
            "\n"
        ),
    );
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    let first = ingest_codex_file(&db, &transcript).unwrap();
    assert_eq!(first.inserted_turns, 0);
    assert_eq!(first.next_offset, session.len() as u64);

    append(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-06-08T00:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
            "\n"
        ),
    );
    let second = ingest_codex_file(&db, &transcript).unwrap();

    assert_eq!(second.inserted_turns, 1);
    let turns = db
        .completed_turns_for_session_range("session-1", 0, u64::MAX)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].display_text.is_none());
    let hydrated = hydrate_turns(&db, &turns).unwrap();
    assert!(hydrated[0]
        .display_text
        .as_ref()
        .unwrap()
        .contains("prefer functional style"));
}

#[test]
fn incremental_codex_ingest_assigns_continuing_ordinals() {
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    fs::write(
        &transcript,
        format!("{}{}", session_meta(), completed_turn(0)),
    )
    .unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    ingest_codex_file(&db, &transcript).unwrap();
    append(&transcript, &completed_turn(1));

    let report = ingest_codex_file(&db, &transcript).unwrap();

    assert_eq!(report.inserted_turns, 1);
    let turns = db
        .completed_turns_for_session_range("session-1", 0, u64::MAX)
        .unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].ordinal, 0);
    assert_eq!(turns[1].ordinal, 1);
}

#[test]
fn forked_codex_transcript_does_not_repoint_parent_session_path() {
    let tmp = TempDir::new().unwrap();
    let parent = tmp.path().join("parent.jsonl");
    let child = tmp.path().join("child.jsonl");
    fs::write(&parent, session_meta_with("parent-session", "/tmp/parent")).unwrap();
    fs::write(
        &child,
        format!(
            "{}{}",
            session_meta_with("child-session", "/tmp/child"),
            session_meta_with("parent-session", "/tmp/parent")
        ),
    )
    .unwrap();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    ingest_codex_file(&db, &parent).unwrap();
    ingest_codex_file(&db, &child).unwrap();

    assert_eq!(
        db.session_by_id("parent-session")
            .unwrap()
            .unwrap()
            .transcript_file_path,
        parent.display().to_string()
    );
    assert_eq!(
        db.session_by_id("child-session")
            .unwrap()
            .unwrap()
            .transcript_file_path,
        child.display().to_string()
    );
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
fn dedupe_deactivates_obvious_same_project_duplicate_memory() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.insert_memory(&memory(
        "Elroy uses just as command runner",
        "The elroy Python project uses just for build, test, lint, and format commands. Always use just test, just build, just lint, and just format.",
        Some("/tmp/elroy"),
    ))
    .unwrap();
    db.insert_memory(&memory(
        "Elroy project uses just command runner",
        "This project uses just instead of running tools directly. Always use just test, just build, just lint, just format, and just typecheck. Run just --list for commands. Required before code review: just lint, just typecheck, and just test must pass.",
        Some("/tmp/elroy"),
    ))
    .unwrap();

    let deactivated = dedupe_active_memories(&db, "2026-06-08T00:00:01Z").unwrap();
    let memories = db.list_memories().unwrap();

    assert_eq!(deactivated, 1);
    assert_eq!(memories.iter().filter(|memory| memory.is_active).count(), 1);
    assert!(memories
        .iter()
        .find(|memory| memory.is_active)
        .unwrap()
        .body
        .contains("typecheck"));
}

#[test]
fn dedupe_keeps_related_but_distinct_same_project_memories() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.insert_memory(&memory(
        "Annotation types and generation precedence",
        "Six annotation types, in precedence order: identical serialized expression, model changed, feature signal changed, true conditions changed, and likely old version.",
        Some("/tmp/redundant-rules"),
    ))
    .unwrap();
    db.insert_memory(&memory(
        "Annotation generator patterns: vector plus AST vs lexical plus AST",
        "Two patterns exist: vector-first for model and feature-signal candidates, and lexical-first for true-condition directional candidates. AST checks reject false positives.",
        Some("/tmp/redundant-rules"),
    ))
    .unwrap();

    let deactivated = dedupe_active_memories(&db, "2026-06-08T00:00:01Z").unwrap();

    assert_eq!(deactivated, 0);
    assert_eq!(
        db.list_memories()
            .unwrap()
            .iter()
            .filter(|memory| memory.is_active)
            .count(),
        2
    );
}

#[test]
fn consolidation_scheduler_queues_one_delayed_task_for_new_memories() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.insert_memory(&memory(
        "YAAML recall task dispatch",
        "Recall tasks are dispatched by the daemon.",
        Some("/tmp/yaaml"),
    ))
    .unwrap();
    let mut config = Config::default();
    config.consolidation_dark_period_seconds = 300;

    let queued = queue_memory_consolidation_if_due(&db, &config).unwrap();
    let duplicate = queue_memory_consolidation_if_due(&db, &config).unwrap();

    assert!(queued.is_some());
    assert!(duplicate.is_none());
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn consolidation_scheduler_requeues_when_active_cluster_still_exists() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let memories = [
        (
            "Backtest turn context improves recall ranking",
            "Turn context metadata improves recall ranking for multi-project sessions.",
            vec![1.0_f32, 0.0],
        ),
        (
            "Backtest confirmed turn context recall ranking improved",
            "Backtests confirmed turn context improves recall ranking for multi-project sessions.",
            vec![0.8_f32, 0.2],
        ),
        (
            "Backtest showed improved recall ranking with turn context",
            "Known bad sessions now return project-specific memories after turn context hydration.",
            vec![0.6_f32, 0.4],
        ),
    ];
    for (title, body, vector) in memories {
        let memory_id = db
            .insert_memory(&memory(title, body, Some("/tmp/yaaml")))
            .unwrap();
        db.upsert_embedding(&EmbeddingRecord {
            memory_id,
            embedding_model: "text-embedding-3-small".to_string(),
            dimensions: vector.len() as u64,
            embedding_blob: encode_f32_embedding(&vector),
            embedded_text_hash: format!("hash-{memory_id}"),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        })
        .unwrap();
    }
    let task_id = db
        .enqueue_task(&TaskRecord {
            id: None,
            kind: TASK_KIND_MEMORY_CONSOLIDATION.to_string(),
            status: TaskStatus::Queued,
            priority: 0,
            payload_json: "{}".to_string(),
            attempts: 0,
            max_attempts: 5,
            next_run_at: None,
            last_error: None,
            created_at: "2026-06-08T00:05:00Z".to_string(),
            updated_at: "2026-06-08T00:05:00Z".to_string(),
        })
        .unwrap();
    db.complete_task(task_id, "2026-06-08T00:05:01Z").unwrap();
    let mut config = Config::default();
    config.consolidation_dark_period_seconds = 0;

    let queued = queue_memory_consolidation_if_due(&db, &config).unwrap();

    assert!(queued.is_some());
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn consolidation_task_merges_top_cluster_and_preserves_lineage() {
    std::env::set_var("YAAML_TEST_CONSOLIDATION_KEY", "test-key");
    std::env::set_var("YAAML_TEST_OPENAI_KEY", "test-key");
    let anthropic = fake_anthropic_server(
        r#"{"memories":[{"title":"YAAML background recall consolidation","body":"Background recall now enqueues and dispatches recall tasks, falls back gracefully when session files are missing, and schedules delayed recall evals after transcript catch-up.","scope":"project","project_descriptor":"yaaml, Rust"}]}"#,
    );
    let embedding = fake_embedding_server();
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let first = db
        .insert_memory(&memory(
            "YAAML background recall file generation gap",
            "Session-specific recall files were not auto-generated because recall tasks were not enqueued or dispatched.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    let second = db
        .insert_memory(&memory(
            "YAAML background recall task dispatch and fallback",
            "The daemon now dispatches TASK_KIND_RECALL and bare recall falls back to a project recall file.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    let third = db
        .insert_memory(&memory(
            "YAAML bare recall degrades gracefully",
            "Bare yaaml recall falls back to project recall and new completed Codex turns enqueue background recall tasks.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    for (memory_id, vector) in [
        (first, vec![1.0_f32, 0.0]),
        (second, vec![0.99_f32, 0.01]),
        (third, vec![0.98_f32, 0.02]),
    ] {
        db.upsert_embedding(&EmbeddingRecord {
            memory_id,
            embedding_model: "text-embedding-3-small".to_string(),
            dimensions: vector.len() as u64,
            embedding_blob: encode_f32_embedding(&vector),
            embedded_text_hash: format!("hash-{memory_id}"),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
        })
        .unwrap();
    }
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_MEMORY_CONSOLIDATION.to_string(),
        status: TaskStatus::Queued,
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
    let mut config = Config::default();
    config.consolidation_api_key_env = "YAAML_TEST_CONSOLIDATION_KEY".to_string();
    config.consolidation_base_url = Some(anthropic.base_url.clone());
    config.embedding_api_key_env = "YAAML_TEST_OPENAI_KEY".to_string();
    config.embedding_base_url = Some(embedding.base_url.clone());

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    anthropic.join();
    embedding.join();

    let memories = db.list_memories().unwrap();
    let active = memories
        .iter()
        .filter(|memory| memory.is_active)
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].title, "YAAML background recall consolidation");
    assert_eq!(active[0].lineage_refs, vec![first, second, third]);
    assert!(db.get_embedding(active[0].id.unwrap()).unwrap().is_some());
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_MEMORY_CONSOLIDATION, TaskStatus::Completed)
            .unwrap(),
        1
    );
}

#[test]
fn stale_insufficient_context_eval_is_queued_for_rerun() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: "/tmp/yaaml".to_string(),
        transcript_file_path: "/tmp/session.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:01Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-0".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("recall anchor".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start: 10,
        byte_end: 20,
        observed_at: Some("2026-06-08T00:10:02Z".to_string()),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("later turn".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let memory_id = db
        .insert_memory(&memory(
            "Recall eval memory",
            "This memory can be reconstructed for a stale eval rerun.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 0)
        .unwrap()
        .unwrap();
    let run_id = db
        .insert_eval_run(
            "recall_1_to_5",
            "2026-06-08T00:00:03Z",
            &serde_json::json!({
                "session_id": "session-1",
                "turn_ordinal": 0,
                "memory_ids": [memory_id],
            })
            .to_string(),
        )
        .unwrap();
    db.insert_eval_result(
        run_id,
        turn_row_id,
        Some(memory_id),
        "insufficient_context",
        "not enough later turns",
        "2026-06-08T00:00:04Z",
    )
    .unwrap();
    db.complete_eval_run(run_id, "2026-06-08T00:00:05Z")
        .unwrap();

    assert_eq!(queue_stale_recall_eval_tasks(&db, 10).unwrap(), 1);
    assert_eq!(queue_stale_recall_eval_tasks(&db, 10).unwrap(), 0);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn recall_eval_scores_each_recalled_memory() {
    std::env::set_var("YAAML_TEST_EVAL_KEY", "test-key");
    let judge =
        fake_anthropic_server_with_requests(r#"{"score":"5","rationale":"directly relevant"}"#, 2);
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: "/tmp/yaaml".to_string(),
        transcript_file_path: "/tmp/session.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:01Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-0".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("recall anchor".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start: 10,
        byte_end: 20,
        observed_at: Some("2026-06-08T00:10:02Z".to_string()),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("later turn used both memories".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let first = db
        .insert_memory(&memory(
            "First recall memory",
            "First memory body.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    let second = db
        .insert_memory(&memory(
            "Second recall memory",
            "Second memory body.",
            Some("/tmp/yaaml"),
        ))
        .unwrap();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority: 0,
        payload_json: serde_json::json!({
            "session_id": "session-1",
            "turn_ordinal": 0,
            "recall_text": "full recall text",
            "memory_ids": [first, second],
        })
        .to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    let mut config = Config::default();
    config.eval_judge_api_key_env = "YAAML_TEST_EVAL_KEY".to_string();
    config.eval_judge_base_url = Some(judge.base_url.clone());

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    judge.join();

    let runs = db.list_eval_runs(1).unwrap();
    assert_eq!(runs[0].result_count, 2);
    let results = db.eval_results_for_run(runs[0].id).unwrap();
    let memory_ids = results
        .iter()
        .filter_map(|result| result.memory_id)
        .collect::<Vec<_>>();
    assert_eq!(memory_ids, vec![first, second]);
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
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/session-1.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:01Z".to_string()),
    })
    .unwrap();
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
        cwd: None,
        context: None,
    };
    db.insert_turn(&turn).unwrap();

    refresh_recall_with_embedding(
        &db,
        &config,
        &project.canonicalize().unwrap(),
        std::slice::from_ref(&turn),
        &[1.0, 0.0],
        "new completed turn",
    )
    .unwrap();
    let path = session_recall_file_path(&config.recall_dir().unwrap(), "session-1");
    let markdown = fs::read_to_string(path).unwrap();

    assert!(markdown.contains("## Recall file location"));
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, yaaml_core::TaskStatus::Queued)
            .unwrap(),
        1
    );
    refresh_recall_with_embedding(
        &db,
        &config,
        &project.canonicalize().unwrap(),
        std::slice::from_ref(&turn),
        &[1.0, 0.0],
        "unchanged completed turn",
    )
    .unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, yaaml_core::TaskStatus::Queued)
            .unwrap(),
        1
    );
    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 0);
}

#[test]
fn recall_eval_task_defers_until_anchor_turn_is_ingested() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let config = Config::default();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority: 0,
        payload_json: r##"{
            "session_id":"session-1",
            "turn_ordinal":0,
            "recall_text":"# YAAML Recall\n\nmemory_ids: 1\n\n## Relevant memory\n\nUse YAAML recall.",
            "memory_ids":[1],
            "recall_at":"unix:1",
            "eval_after":"unix:1"
        }"##
        .to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "unix:1".to_string(),
        updated_at: "unix:1".to_string(),
    })
    .unwrap();

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Completed)
            .unwrap(),
        1
    );
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Parked)
            .unwrap(),
        0
    );
}

#[test]
fn recall_eval_records_insufficient_context_without_later_turns() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: "/tmp/project".to_string(),
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
        observed_at: Some("2026-06-08T00:00:01Z".to_string()),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let config = Config::default();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority: 0,
        payload_json: r##"{
            "session_id":"session-1",
            "turn_ordinal":0,
            "recall_text":"# YAAML Recall\n\nmemory_ids: 1\n\n## Relevant memory\n\nUse YAAML recall.",
            "memory_ids":[1],
            "recall_at":"unix:1",
            "eval_after":"unix:1"
        }"##
        .to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "unix:1".to_string(),
        updated_at: "unix:1".to_string(),
    })
    .unwrap();

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    let runs = db.list_eval_runs(1).unwrap();
    assert_eq!(runs.len(), 1);
    let results = db.eval_results_for_run(runs[0].id).unwrap();

    assert_eq!(
        results[0].judge_score.as_deref(),
        Some("insufficient_context")
    );
    assert!(results[0]
        .rationale
        .as_deref()
        .unwrap()
        .contains("No subsequent completed turns"));
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Parked)
            .unwrap(),
        0
    );
}

#[test]
fn recall_eval_defers_without_later_turns_while_session_is_active() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let now_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: "/tmp/project".to_string(),
        transcript_file_path: "/tmp/session.jsonl".to_string(),
        started_at: Some(format!("unix:{now_seconds}")),
        last_seen_at: Some(format!("unix:{now_seconds}")),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some(format!("unix:{now_seconds}")),
        status: yaaml_core::TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let config = Config::default();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority: 0,
        payload_json: r##"{
            "session_id":"session-1",
            "turn_ordinal":0,
            "recall_text":"# YAAML Recall\n\nmemory_ids: 1\n\n## Relevant memory\n\nUse YAAML recall.",
            "memory_ids":[1],
            "recall_at":"unix:1",
            "eval_after":"unix:1"
        }"##
        .to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "unix:1".to_string(),
        updated_at: "unix:1".to_string(),
    })
    .unwrap();

    assert_eq!(run_queued_tasks(&db, &config, 1).unwrap(), 1);
    assert_eq!(db.list_eval_runs(1).unwrap().len(), 0);
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Completed)
            .unwrap(),
        1
    );
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        1
    );
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
    stream.flush().unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
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

struct FakeEmbeddingServer {
    base_url: String,
    handle: thread::JoinHandle<()>,
}

impl FakeEmbeddingServer {
    fn join(self) {
        self.handle.join().unwrap();
    }
}

struct FakeAnthropicServer {
    base_url: String,
    handle: thread::JoinHandle<()>,
}

impl FakeAnthropicServer {
    fn join(self) {
        self.handle.join().unwrap();
    }
}

fn fake_anthropic_server(json_text: &'static str) -> FakeAnthropicServer {
    fake_anthropic_server_with_requests(json_text, 1)
}

fn fake_anthropic_server_with_requests(
    json_text: &'static str,
    request_count: usize,
) -> FakeAnthropicServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        for _ in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let _ = stream.read(&mut buffer).unwrap();
            let body = format!(
                r#"{{"content":[{{"type":"text","text":{}}}]}}"#,
                serde_json::to_string(json_text).unwrap()
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    FakeAnthropicServer { base_url, handle }
}

fn fake_embedding_server() -> FakeEmbeddingServer {
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

    FakeEmbeddingServer { base_url, handle }
}

fn session_meta_with(id: &str, cwd: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-06-08T00:00:00Z","cwd":"{cwd}"}}}}"#
    ) + "\n"
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

fn memory(title: &str, body: &str, project_id: Option<&str>) -> MemoryRecord {
    MemoryRecord {
        id: None,
        title: title.to_string(),
        body: body.to_string(),
        scope: MemoryScope::Project,
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: project_id.map(str::to_string),
        project_descriptor: project_id.map(str::to_string),
        lineage_refs: Vec::new(),
    }
}
