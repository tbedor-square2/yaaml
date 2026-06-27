use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use yaaml::daemon::TASK_KIND_RECALL_EVAL;
use yaaml_core::{
    recall_file_path, session_recall_file_path, AgentType, ConversationSegmentRecord,
    ConversationSegmentStatus, EmbeddingRecord, MemoryKind, MemoryRecord, MemoryScope,
    SessionRecord, TaskRecord, TaskStatus, TurnRecord, TurnStatus,
};
use yaaml_store::database::{encode_f32_embedding, EvalRunMetadata};
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
recall_llm_filter_enabled = false
recall_memory_cooldown_seconds = 1200
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
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id.clone()),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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
fn recall_query_json_can_include_debug_ranking() {
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
recall_llm_filter_enabled = false
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory = MemoryRecord {
        id: None,
        title: "Task-key recall".to_string(),
        body: "PR 481245 should use task-key aware ranking.".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::TaskState,
        task_keys: vec!["pr:481245".to_string()],
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--query")
        .arg("what should PR 481245 recall")
        .arg("--json")
        .arg("--debug-ranking")
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
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["selected_memory_ids"][0], memory_id);
    assert_eq!(
        value["ranking"][0]["rank"]["matched_task_keys"][0],
        "pr:481245"
    );
    assert!(
        value["ranking"][0]["rank"]["task_key_bonus"]
            .as_f64()
            .unwrap()
            > 0.0
    );
}

#[test]
fn recall_query_defaults_to_two_selected_memories() {
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
recall_llm_filter_enabled = false
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory_specs = [
        (
            "Default limit lesson",
            "Recall default limits should keep the strongest durable lesson without returning too much context.",
            MemoryKind::Lesson,
        ),
        (
            "Default limit workflow",
            "Recall default limits should include a relevant workflow but avoid bloating the response.",
            MemoryKind::Workflow,
        ),
        (
            "Default limit preference",
            "Recall default limits should not surface every relevant memory when the top two are enough.",
            MemoryKind::Preference,
        ),
    ];
    let memory_ids = memory_specs
        .into_iter()
        .map(|(title, body, kind)| {
            insert_memory_with_embedding(
                &mut db,
                MemoryRecord {
                    id: None,
                    title: title.to_string(),
                    body: body.to_string(),
                    scope: MemoryScope::Project,
                    kind,
                    task_keys: Vec::new(),
                    source_turn_refs: Vec::new(),
                    created_at: "2026-06-08T00:00:00Z".to_string(),
                    updated_at: "2026-06-08T00:00:00Z".to_string(),
                    is_active: true,
                    session_id: None,
                    project_id: Some(project_id.clone()),
                    project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
                    lineage_refs: Vec::new(),
                    origin_segment_id: None,
                    origin_segment_status: None,
                    validity: yaaml_core::MemoryValidity::Durable,
                },
            )
        })
        .collect::<Vec<_>>();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--query")
        .arg("default recall result limit")
        .arg("--json")
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
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let selected = value["selected_memory_ids"].as_array().unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected
        .iter()
        .all(|id| memory_ids.contains(&id.as_i64().unwrap())));
}

#[test]
fn recall_query_suppresses_recently_recalled_memory_in_same_session() {
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
recall_llm_filter_enabled = false
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "cooldown-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/cooldown-session.jsonl".to_string(),
        started_at: Some("unix:1".to_string()),
        last_seen_at: Some("unix:2".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "cooldown-session".to_string(),
        turn_id: Some("turn-0".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 1,
        observed_at: Some("unix:2".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("cooldown recall query".to_string()),
        cwd: Some(project_id.clone()),
        context: None,
    })
    .unwrap();
    let memory_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Cooldown memory".to_string(),
            body: "This memory should be suppressed when it was just recalled in this session."
                .to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        },
    );
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: TASK_KIND_RECALL_EVAL.to_string(),
        status: TaskStatus::Queued,
        priority: 0,
        payload_json: format!(
            r#"{{"session_id":"cooldown-session","memory_ids":[{memory_id}],"recall_at":"unix:{now_unix}"}}"#
        ),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: format!("unix:{now_unix}"),
        updated_at: format!("unix:{now_unix}"),
    })
    .unwrap();
    assert!(db
        .recent_recalled_memory_ids("cooldown-session", now_unix as i64 - 1200)
        .unwrap()
        .contains(&memory_id));
    let run_id = db
        .insert_eval_run_with_metadata(
            "recall",
            &format!("unix:{now_unix}"),
            "{}",
            EvalRunMetadata {
                session_id: Some("cooldown-session".to_string()),
                turn_ordinal: Some(0),
                recall_origin: "manual_query".to_string(),
                ..EvalRunMetadata::default()
            },
        )
        .unwrap();
    db.insert_eval_result(
        run_id,
        1,
        Some(memory_id),
        "4",
        "recently recalled",
        &format!("unix:{now_unix}"),
    )
    .unwrap();
    drop(db);
    let readonly_db = Database::open(&db_path).unwrap();
    assert!(readonly_db
        .recent_recalled_memory_ids("cooldown-session", now_unix as i64 - 1200)
        .unwrap()
        .contains(&memory_id));
    drop(readonly_db);

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--query")
        .arg("cooldown recall query")
        .arg("--session")
        .arg("cooldown-session")
        .arg("--json")
        .arg("--debug-ranking")
        .current_dir(&project)
        .env("HOME", &home)
        .env("OPENAI_API_KEY", "test-key")
        .env("CODEX_THREAD_ID", "cooldown-session")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["selected_memory_ids"].as_array().unwrap().is_empty(),
        "{}",
        serde_json::to_string_pretty(&value).unwrap()
    );
    assert_eq!(value["ranking"][0]["memory_id"], memory_id);
    assert!(value["ranking"][0]["filter_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason == "drop:recent_recall_cooldown"));
}

#[test]
fn recall_query_debug_shows_dropped_task_state_without_task_key_match() {
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
recall_llm_filter_enabled = false
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let stale_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Stale PR state".to_string(),
            body: "PR 111111 was an abandoned strategy.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: vec!["pr:111111".to_string()],
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id.clone()),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        },
    );
    let target_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Target PR state".to_string(),
            body: "PR 481245 should be recalled for this task.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: vec!["pr:481245".to_string()],
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        },
    );

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--query")
        .arg("what should PR 481245 recall")
        .arg("--json")
        .arg("--debug-ranking")
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
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["selected_memory_ids"], serde_json::json!([target_id]));
    let ranking = value["ranking"].as_array().unwrap();
    let stale = ranking
        .iter()
        .find(|entry| entry["memory_id"] == stale_id)
        .unwrap();
    assert_eq!(stale["selected"], false);
    assert_eq!(
        stale["filter_reasons"][0],
        "drop:stale_task_state_semantic_context_only"
    );
}

#[test]
fn recall_query_reranks_with_memory_health() {
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
recall_llm_filter_enabled = false
recall_result_limit = 1
"#,
            db_path.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "health-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/health-session.jsonl".to_string(),
        started_at: Some("unix:1".to_string()),
        last_seen_at: Some("unix:2".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "health-session".to_string(),
        turn_id: Some("turn-0".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 1,
        observed_at: Some("unix:2".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("recall health ranking".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let turn_id = db
        .turn_row_id_for_session_ordinal("health-session", 0)
        .unwrap()
        .unwrap();
    let low_memory_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Repeatedly low memory".to_string(),
            body: "This memory has historically been a poor fit for recall, despite looking semantically close to the current query. It includes enough durable-looking text that the health classifier treats the bad outcomes as a low-value memory problem rather than simply a short or vague memory. The reranker should suppress it after repeated low eval scores.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id.clone()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
        },
    );
    let useful_memory_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Proven useful memory".to_string(),
            body: "Use memory health evidence to rerank recall candidates when prior evals repeatedly show that a memory was helpful. This durable guidance should surface ahead of similarly ranked memories with poor eval history.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Workflow,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
        },
    );
    let run_id = db.insert_eval_run("recall", "unix:3", "{}").unwrap();
    for (index, score) in ["1", "2", "1", "2", "1"].iter().enumerate() {
        db.insert_eval_result(
            run_id,
            turn_id,
            Some(low_memory_id),
            score,
            "The memory was not useful in this recall context.",
            &format!("unix:{}", 4 + index),
        )
        .unwrap();
    }
    for (index, score) in ["5", "4", "5", "4", "5"].iter().enumerate() {
        db.insert_eval_result(
            run_id,
            turn_id,
            Some(useful_memory_id),
            score,
            "The memory was useful and actionable.",
            &format!("unix:{}", 10 + index),
        )
        .unwrap();
    }
    db.complete_eval_run(run_id, "unix:20").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--query")
        .arg("recall health ranking")
        .arg("--json")
        .arg("--debug-ranking")
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
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["selected_memory_ids"][0], useful_memory_id);
    let ranking = value["ranking"].as_array().unwrap();
    assert_eq!(ranking[0]["memory_id"], useful_memory_id);
    assert!(ranking
        .iter()
        .find(|candidate| candidate["memory_id"] == low_memory_id)
        .unwrap()["rank"]["penalties"]
        .as_array()
        .unwrap()
        .iter()
        .any(|penalty| penalty.as_str().unwrap().contains("health_action_rerank")));
}

#[test]
fn bare_recall_invalidates_file_with_inactive_memory_ids() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
embedding_api_key_env = "YAAML_TEST_MISSING_OPENAI_KEY"
"#,
            db_path.display(),
            recall_dir.display()
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Inactive recall memory".to_string(),
            body: "This memory should not be printed after deactivation.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id.display().to_string()),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    db.deactivate_memory(memory_id, "2026-06-08T00:00:01Z")
        .unwrap();
    let recall_path = recall_file_path(&recall_dir, &project_id);
    fs::create_dir_all(recall_path.parent().unwrap()).unwrap();
    fs::write(
        &recall_path,
        format!(
            "# YAAML Recall\n\nmemory_count: 1\nmemory_ids: {memory_id}\n\n## Inactive recall memory\n\nThis memory should not be printed.\n"
        ),
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
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
    assert!(!stdout.contains("Inactive recall memory"));
    assert!(stdout.contains("no recall file"));
    assert!(!recall_path.exists());
}

#[test]
fn bare_recall_invalidates_file_that_exceeds_result_limit() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    let recall_dir = home.join(".yaaml").join("recall");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(
            r#"
db_path = "{}"
recall_dir = "{}"
recall_result_limit = 2
embedding_api_key_env = "YAAML_TEST_MISSING_OPENAI_KEY"
"#,
            db_path.display(),
            recall_dir.display()
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    let memory_ids = ["Old first", "Old second", "Old third"]
        .into_iter()
        .map(|title| {
            db.insert_memory(&MemoryRecord {
                id: None,
                title: title.to_string(),
                body: "This cached recall file has more memories than the current limit."
                    .to_string(),
                scope: MemoryScope::Project,
                kind: MemoryKind::Lesson,
                task_keys: Vec::new(),
                source_turn_refs: Vec::new(),
                created_at: "2026-06-08T00:00:00Z".to_string(),
                updated_at: "2026-06-08T00:00:00Z".to_string(),
                is_active: true,
                session_id: None,
                project_id: Some(project_id.display().to_string()),
                project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
                lineage_refs: Vec::new(),
                origin_segment_id: None,
                origin_segment_status: None,
                validity: yaaml_core::MemoryValidity::Durable,
            })
            .unwrap()
        })
        .collect::<Vec<_>>();
    let recall_path = recall_file_path(&recall_dir, &project_id);
    fs::create_dir_all(recall_path.parent().unwrap()).unwrap();
    fs::write(
        &recall_path,
        format!(
            "# YAAML Recall\n\nmemory_count: 3\nmemory_ids: {},{},{}\n\n## Old first\n\nstale\n\n## Old second\n\nstale\n\n## Old third\n\nstale\n",
            memory_ids[0], memory_ids[1], memory_ids[2]
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
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
    assert!(!stdout.contains("Old first"));
    assert!(stdout.contains("no recall file"));
    assert!(!recall_path.exists());
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
recall_llm_filter_enabled = false
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
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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
    let recall_path = session_recall_file_path(&recall_dir, "session-1");
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
fn tool_pre_use_recall_emits_codex_hook_context_and_eval_metadata() {
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
recall_llm_filter_enabled = false
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
        id: "tool-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: "/tmp/tool-session.jsonl".to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:03Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "tool-session".to_string(),
        turn_id: Some("turn-0".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: 1,
        observed_at: Some("2026-06-08T00:00:01Z".to_string()),
        status: TurnStatus::Completed,
        display_text: None,
        cwd: Some(project_id.clone()),
        context: Some(yaaml_core::infer_context_from_path(&project)),
    })
    .unwrap();
    let memory_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Bazel tool guidance".to_string(),
            body: "Before running Bazel tests, prefer the repo-local bin/bazel wrapper."
                .to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Workflow,
            task_keys: vec!["target://foo:bar".to_string()],
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        },
    );

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("recall")
        .arg("--origin")
        .arg("tool-pre-use")
        .arg("--tool-name")
        .arg("Bash")
        .arg("--tool-use-id")
        .arg("toolu-1")
        .arg("--tool-input-json")
        .arg(r#"{"command":"bazel test //foo:bar"}"#)
        .arg("--session")
        .arg("tool-session")
        .arg("--turn")
        .arg("0")
        .arg("--codex-hook-output")
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
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let additional_context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(additional_context.contains("## Bazel tool guidance"));
    assert!(additional_context.contains(&format!("memory_ids: {memory_id}")));
    assert!(!session_recall_file_path(&recall_dir, "tool-session").exists());

    let db = Database::open(&db_path).unwrap();
    let tasks = db.list_tasks_by_kind(TASK_KIND_RECALL_EVAL).unwrap();
    assert_eq!(tasks.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&tasks[0].payload_json).unwrap();
    assert_eq!(payload["session_id"], "tool-session");
    assert_eq!(payload["turn_ordinal"], 0);
    assert_eq!(payload["memory_ids"][0], memory_id);
    assert_eq!(payload["recall_origin"], "tool_pre_use");
    assert_eq!(payload["tool_name"], "Bash");
    assert_eq!(payload["tool_use_id"], "toolu-1");
    assert_eq!(payload["tool_input_summary"], "bazel test //foo:bar");
    assert_eq!(payload["injected"], true);
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
recall_llm_filter_enabled = false
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let (transcript_path, turn_ranges) = write_codex_transcript(
        tmp.path(),
        "session-without-recall-file.jsonl",
        "session-without-recall-file",
        &project,
        &["agent should recall missing session files"],
    );
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "session-without-recall-file".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: transcript_path.display().to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:03Z".to_string()),
    })
    .unwrap();
    let (byte_start, byte_end) = turn_ranges[0];
    db.insert_turn(&TurnRecord {
        session_id: "session-without-recall-file".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 1,
        byte_start,
        byte_end,
        observed_at: Some("2026-06-08T00:00:03Z".to_string()),
        status: TurnStatus::Completed,
        display_text: None,
        cwd: Some(project_id.clone()),
        context: Some(yaaml_core::infer_context_from_path(&project)),
    })
    .unwrap();
    let memory = MemoryRecord {
        id: None,
        title: "On-demand recall".to_string(),
        body: "Bare recall should generate a missing session recall file from recent turns."
            .to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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

    let recall_path = session_recall_file_path(&recall_dir, "session-without-recall-file");
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
recall_llm_filter_enabled = false
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
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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
    let recall_path = session_recall_file_path(&recall_dir, "new-session");
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
recall_llm_filter_enabled = false
recall_live_turn_window = 2
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let (transcript_path, turn_ranges) = write_codex_transcript(
        tmp.path(),
        "replay.jsonl",
        "replay-session",
        &project,
        &[
            "completed context turn 0",
            "completed context turn 1",
            "completed context turn 2",
        ],
    );
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "replay-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: transcript_path.display().to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
    })
    .unwrap();
    for ordinal in 0..3 {
        let (byte_start, byte_end) = turn_ranges[ordinal as usize];
        db.insert_turn(&TurnRecord {
            session_id: "replay-session".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start,
            byte_end,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: None,
            cwd: Some(project_id.clone()),
            context: Some(yaaml_core::infer_context_from_path(&project)),
        })
        .unwrap();
    }
    let memory = MemoryRecord {
        id: None,
        title: "Historical recall".to_string(),
        body: "Backtests can replay recall for a specific session turn.".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project_id),
        project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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
        .contains("active segment turns 1..=2"));

    let recall_path = session_recall_file_path(&recall_dir, "replay-session");
    assert!(!recall_path.exists());
    let db = Database::open(&db_path).unwrap();
    assert_eq!(
        db.count_tasks_by_status(TASK_KIND_RECALL_EVAL, TaskStatus::Queued)
            .unwrap(),
        0
    );
}

#[test]
fn historical_recall_uses_stored_segment_keys_for_task_state() {
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
recall_llm_filter_enabled = false
recall_live_turn_window = 2
"#,
            db_path.display(),
            recall_dir.display(),
            server.base_url
        ),
    )
    .unwrap();

    let project_id = project.canonicalize().unwrap().display().to_string();
    let (transcript_path, turn_ranges) = write_codex_transcript(
        tmp.path(),
        "weak-task-state.jsonl",
        "segment-key-session",
        &project,
        &["continue the active task", "ok, proceed with the next step"],
    );
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    db.upsert_session(&SessionRecord {
        id: "segment-key-session".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: transcript_path.display().to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:02:00Z".to_string()),
    })
    .unwrap();
    for ordinal in 0..2 {
        let (byte_start, byte_end) = turn_ranges[ordinal as usize];
        db.insert_turn(&TurnRecord {
            session_id: "segment-key-session".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start,
            byte_end,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: None,
            cwd: Some(project_id.clone()),
            context: Some(yaaml_core::infer_context_from_path(&project)),
        })
        .unwrap();
    }
    db.replace_conversation_segments_for_session(
        "segment-key-session",
        &[ConversationSegmentRecord {
            id: None,
            session_id: "segment-key-session".to_string(),
            start_turn_ordinal: 0,
            end_turn_ordinal: 1,
            summary: "Turns 0..=1 continue work on PR 481583.".to_string(),
            task_keys: vec!["pr:481583".to_string()],
            context: Some(yaaml_core::infer_context_from_path(&project)),
            status: ConversationSegmentStatus::Active,
            created_at: "unix:1".to_string(),
            updated_at: "unix:1".to_string(),
        }],
    )
    .unwrap();
    let memory_id = insert_memory_with_embedding(
        &mut db,
        MemoryRecord {
            id: None,
            title: "Current PR state".to_string(),
            body: "PR 481583 needs the segment-key regression test before continuing.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: vec!["pr:481583".to_string()],
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project_id),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        },
    );

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("recall")
        .arg("--session")
        .arg("segment-key-session")
        .arg("--turn")
        .arg("1")
        .arg("--json")
        .arg("--debug-ranking")
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
    assert_eq!(value["selected_memory_ids"][0], memory_id);
    assert!(value["markdown"]
        .as_str()
        .unwrap()
        .contains("## Current PR state"));
    assert!(value["ranking"][0]["rank"]["matched_task_keys"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pr:481583")));
}

struct FakeServer {
    base_url: String,
    handle: thread::JoinHandle<()>,
}

fn insert_memory_with_embedding(db: &mut Database, memory: MemoryRecord) -> i64 {
    let memory_id = db.insert_memory(&memory).unwrap();
    db.upsert_embedding(&EmbeddingRecord {
        memory_id,
        embedding_model: "text-embedding-3-small".to_string(),
        dimensions: 2,
        embedding_blob: encode_f32_embedding(&[1.0, 0.0]),
        embedded_text_hash: format!("hash-{memory_id}"),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    })
    .unwrap();
    memory_id
}

fn write_codex_transcript(
    root: &Path,
    file_name: &str,
    session_id: &str,
    project: &Path,
    turn_texts: &[&str],
) -> (PathBuf, Vec<(u64, u64)>) {
    let transcript_path = root.join(file_name);
    let project_id = project.display().to_string();
    let mut transcript = String::new();
    append_jsonl(
        &mut transcript,
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-06-08T00:00:00Z",
                "cwd": project_id,
            },
        }),
    );

    let mut ranges = Vec::new();
    for (index, text) in turn_texts.iter().enumerate() {
        let start = transcript.len() as u64;
        let turn_id = format!("turn-{index}");
        append_jsonl(
            &mut transcript,
            serde_json::json!({
                "timestamp": format!("2026-06-08T00:00:0{}Z", index + 1),
                "type": "event_msg",
                "payload": {
                    "type": "task_started",
                    "turn_id": turn_id,
                },
            }),
        );
        append_jsonl(
            &mut transcript,
            serde_json::json!({
                "timestamp": format!("2026-06-08T00:00:0{}Z", index + 1),
                "type": "turn_context",
                "payload": {
                    "turn_id": turn_id,
                    "cwd": project_id,
                },
            }),
        );
        append_jsonl(
            &mut transcript,
            serde_json::json!({
                "timestamp": format!("2026-06-08T00:00:0{}Z", index + 1),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": text,
                        },
                    ],
                },
            }),
        );
        append_jsonl(
            &mut transcript,
            serde_json::json!({
                "timestamp": format!("2026-06-08T00:00:0{}Z", index + 1),
                "type": "event_msg",
                "payload": {
                    "type": "task_complete",
                    "turn_id": turn_id,
                },
            }),
        );
        ranges.push((start, transcript.len() as u64));
    }

    fs::write(&transcript_path, transcript).unwrap();
    (transcript_path, ranges)
}

fn append_jsonl(output: &mut String, value: serde_json::Value) {
    output.push_str(&value.to_string());
    output.push('\n');
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
