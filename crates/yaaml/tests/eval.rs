use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use yaaml_core::{
    AgentType, MemoryKind, MemoryRecord, MemoryScope, SessionRecord, TaskRecord, TaskStatus,
    TurnRecord, TurnStatus,
};
use yaaml_store::database::EvalRunMetadata;
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
    insert_transcript_backed_turn(&db, tmp.path(), &project, "use recall");
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "Earlier memory".to_string(),
        body: "Useful context".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        updated_at: "2026-06-08T00:00:01Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project.display().to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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
    assert_eq!(runs[0]["score"], "unjudged");
    let started_at_human = runs[0]["started_at_human"].as_str().unwrap();
    assert!(!started_at_human.starts_with("unix:"));
    assert!(started_at_human.contains("2026-"));

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
    assert!(shown["recalled_memories"].as_array().unwrap().is_empty());
    assert_eq!(shown["later_completed_turns"], serde_json::Value::Null);

    let summary = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        summary.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&summary.stderr)
    );
    let summary_value: serde_json::Value = serde_json::from_slice(&summary.stdout).unwrap();
    assert_eq!(summary_value["runs_considered"], 1);
    assert_eq!(summary_value["results_considered"], 1);
    assert_eq!(summary_value["judged_results"], 0);
    assert_eq!(summary_value["average_score"], serde_json::Value::Null);
    assert_eq!(summary_value["score_counts"]["unjudged"], 1);
    assert_eq!(summary_value["session_breakdown"][0]["runs"], 1);
    assert_eq!(
        summary_value["stale_insufficient_context"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn eval_recall_can_target_specific_session_turn() {
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
    insert_transcript_backed_turn(&db, tmp.path(), &project, "use recall for targeted replay");
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "Later eligible preference".to_string(),
        body: "use recall for targeted replay".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Preference,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:04Z".to_string(),
        updated_at: "2026-06-08T00:00:04Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project.display().to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-2".to_string()),
        ordinal: 1,
        byte_start: 0,
        byte_end: 0,
        observed_at: Some("2026-06-08T00:00:05Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("use recall for targeted replay".to_string()),
        cwd: Some(project.display().to_string()),
        context: Some(yaaml_core::infer_context_from_path(&project)),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("recall")
        .arg("--session")
        .arg("session-1")
        .arg("--turn")
        .arg("0")
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
    let first_run_id = value["run_id"].as_i64().unwrap();

    let output = Command::new(binary)
        .arg("eval")
        .arg("recall")
        .arg("--session")
        .arg("session-1")
        .arg("--turn")
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
    let second_run_id = value["run_id"].as_i64().unwrap();

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
    let first = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["id"] == first_run_id)
        .unwrap();
    assert_eq!(first["session_id"], "session-1");
    assert_eq!(first["turn_ordinal"], 0);
    let second = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["id"] == second_run_id)
        .unwrap();
    assert_eq!(second["session_id"], "session-1");
    assert_eq!(second["turn_ordinal"], 1);
}

#[test]
fn eval_recall_turn_requires_session() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");

    let output = Command::new(binary)
        .arg("eval")
        .arg("recall")
        .arg("--turn")
        .arg("0")
        .current_dir(&project)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--turn requires --session"));
}

#[test]
fn eval_list_includes_session_turn_score_and_human_timestamps() {
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
        turn_id: Some("turn-7".to_string()),
        ordinal: 7,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 7)
        .unwrap()
        .unwrap();
    let run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:1781205326",
            r#"{"session_id":"session-1","turn_ordinal":7,"memory_ids":[1]}"#,
            eval_metadata(7, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(run_id, turn_row_id, None, "5", "great", "unix:1781205330")
        .unwrap();
    db.complete_eval_run(run_id, "unix:1781205331").unwrap();
    let insufficient_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:1781205400",
            r#"{"session_id":"session-1","turn_ordinal":8,"memory_ids":[1]}"#,
            eval_metadata(8, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        insufficient_run_id,
        turn_row_id,
        None,
        "insufficient_context",
        "not enough later turns",
        "unix:1781205400",
    )
    .unwrap();
    db.complete_eval_run(insufficient_run_id, "unix:1781205400")
        .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let json_output = Command::new(binary)
        .arg("eval")
        .arg("list")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        json_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    let runs: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(runs[0]["id"], insufficient_run_id);
    assert_eq!(runs[0]["session_id"], "session-1");
    assert_eq!(runs[0]["turn_ordinal"], 8);
    assert_eq!(runs[0]["score"], "n/a");
    assert_eq!(runs[1]["id"], run_id);
    assert_eq!(runs[1]["score"], "5");
    let started_at_human = runs[1]["started_at_human"].as_str().unwrap();
    let completed_at_human = runs[1]["completed_at_human"].as_str().unwrap();
    assert!(!started_at_human.starts_with("unix:"));
    assert!(!completed_at_human.starts_with("unix:"));
    assert!(started_at_human.contains(":26 "));
    assert!(completed_at_human.contains(":31 "));

    let text_output = Command::new(binary)
        .arg("eval")
        .arg("list")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        text_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&text_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&text_output.stdout);
    assert!(stdout.contains("score=n/a"));
    assert!(stdout.contains("score=5"));
    assert!(stdout.contains("session=session-1"));
    assert!(stdout.contains("turn=7"));
    assert!(stdout.contains("started=20"));
    assert!(!stdout.contains("recall_1_to_5"));

    let filtered = Command::new(binary)
        .arg("eval")
        .arg("list")
        .arg("--since-run")
        .arg(insufficient_run_id.to_string())
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        filtered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered_runs: serde_json::Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(filtered_runs.as_array().unwrap().len(), 1);
    assert_eq!(filtered_runs[0]["id"], insufficient_run_id);
}

#[test]
fn eval_summary_omits_insufficient_context_run_after_scored_rerun() {
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
    for ordinal in 0..=1 {
        db.insert_turn(&TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal * 10,
            byte_end: ordinal * 10 + 10,
            observed_at: Some("2026-06-08T00:10:02Z".to_string()),
            status: TurnStatus::Completed,
            display_text: Some(format!("turn {ordinal}")),
            cwd: Some(project.display().to_string()),
            context: None,
        })
        .unwrap();
    }
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 0)
        .unwrap()
        .unwrap();
    let memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Useful rerun memory".to_string(),
            body: "This memory is useful once enough later context exists.".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1781205300".to_string(),
            updated_at: "unix:1781205300".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let source_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:1781205400",
            &serde_json::json!({
                "session_id": "session-1",
                "turn_ordinal": 0,
                "memory_ids": [memory_id],
            })
            .to_string(),
            eval_metadata(0, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        source_run_id,
        turn_row_id,
        Some(memory_id),
        "insufficient_context",
        "not enough later turns",
        "unix:1781205401",
    )
    .unwrap();
    db.complete_eval_run(source_run_id, "unix:1781205402")
        .unwrap();
    let rerun_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:1781206000",
            &serde_json::json!({
                "session_id": "session-1",
                "turn_ordinal": 0,
                "memory_ids": [memory_id],
                "rerun_for_eval_run_id": source_run_id,
            })
            .to_string(),
            eval_metadata(0, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        rerun_id,
        turn_row_id,
        Some(memory_id),
        "5",
        "relevant",
        "unix:1781206001",
    )
    .unwrap();
    db.complete_eval_run(rerun_id, "unix:1781206002").unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("summary")
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
    assert_eq!(value["runs_considered"], 1);
    assert_eq!(value["results_considered"], 1);
    assert_eq!(value["score_counts"]["5"], 1);
    assert_eq!(value["score_counts"]["n/a"], serde_json::Value::Null);
    assert!(value["stale_insufficient_context"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn eval_requeue_stale_queues_insufficient_context_reruns() {
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
    for ordinal in 0..=1 {
        db.insert_turn(&TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal * 10,
            byte_end: ordinal * 10 + 10,
            observed_at: Some(format!("2026-06-08T00:0{ordinal}:00Z")),
            status: TurnStatus::Completed,
            display_text: Some(format!("turn {ordinal}")),
            cwd: Some(project.display().to_string()),
            context: None,
        })
        .unwrap();
    }
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 0)
        .unwrap()
        .unwrap();
    let memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Queued stale eval memory".to_string(),
            body: "This memory should be reconstructed for the stale recall eval rerun."
                .to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "unix:1781205300".to_string(),
            updated_at: "unix:1781205300".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let source_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:1781205400",
            &serde_json::json!({
                "session_id": "session-1",
                "turn_ordinal": 0,
                "memory_ids": [memory_id],
            })
            .to_string(),
            eval_metadata(0, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        source_run_id,
        turn_row_id,
        Some(memory_id),
        "insufficient_context",
        "not enough later turns",
        "unix:1781205401",
    )
    .unwrap();
    db.complete_eval_run(source_run_id, "unix:1781205402")
        .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("requeue-stale")
        .arg("--limit")
        .arg("5")
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
    assert_eq!(value["queued"], 1);
    assert_eq!(value["scanned_runs"], 1);
    assert_eq!(value["stale_runs"], 1);
    assert_eq!(value["skipped_already_pending"], 0);
    assert_eq!(value["skipped_empty_recall_text"], 0);
    let tasks = db.list_recall_eval_tasks(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].session_id.as_deref(), Some("session-1"));
    assert_eq!(tasks[0].turn_ordinal, Some(0));
    assert_eq!(
        tasks[0].recall_origin.as_deref(),
        Some("session_background")
    );
    assert_eq!(tasks[0].memory_count, 1);

    let second_output = Command::new(binary)
        .arg("eval")
        .arg("requeue-stale")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        second_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    let second_value: serde_json::Value = serde_json::from_slice(&second_output.stdout).unwrap();
    assert_eq!(second_value["queued"], 0);
    assert_eq!(second_value["stale_runs"], 1);
    assert_eq!(second_value["skipped_already_pending"], 1);
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
    insert_transcript_backed_turn(&db, tmp.path(), &project, "use recall");
    db.insert_memory(&MemoryRecord {
        id: None,
        title: "Earlier memory".to_string(),
        body: "Useful context".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        updated_at: "2026-06-08T00:00:01Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some(project.display().to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: yaaml_core::MemoryValidity::Durable,
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

#[test]
fn eval_summary_reports_distribution_and_examples() {
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
        ordinal: 7,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-8".to_string()),
        ordinal: 8,
        byte_start: 10,
        byte_end: 20,
        observed_at: Some("2026-06-08T00:00:07Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("recall had no later context yet".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-9".to_string()),
        ordinal: 9,
        byte_start: 20,
        byte_end: 30,
        observed_at: Some("2026-06-08T00:10:07Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("later context exists now".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 7)
        .unwrap()
        .unwrap();
    let turn_8_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 8)
        .unwrap()
        .unwrap();
    let low_memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Weak memory".to_string(),
            body: "Mostly unrelated context".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let high_memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Useful memory".to_string(),
            body: "Directly actionable context".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let run_id = db
        .insert_eval_run_with_metadata(
            "default",
            "2026-06-08T00:00:03Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
            eval_metadata_with_segment(
                7,
                "replay",
                7,
                8,
                "Turns 7..=8 evaluate whether recalled context helped.",
                &["pr:123"],
            ),
        )
        .unwrap();
    db.insert_eval_result(
        run_id,
        turn_row_id,
        Some(low_memory_id),
        "2",
        "weak relevance",
        "2026-06-08T00:00:04Z",
    )
    .unwrap();
    db.insert_eval_result(
        run_id,
        turn_row_id,
        Some(high_memory_id),
        "5",
        "directly relevant and actionable",
        "2026-06-08T00:00:05Z",
    )
    .unwrap();
    db.complete_eval_run(run_id, "2026-06-08T00:00:06Z")
        .unwrap();
    let stale_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "2026-06-08T00:00:08Z",
            &format!(
                r#"{{"session_id":"session-1","turn_ordinal":8,"memory_ids":[{}]}}"#,
                low_memory_id
            ),
            eval_metadata_with_segment(
                8,
                "session_background",
                7,
                8,
                "Turns 7..=8 evaluate whether recalled context helped.",
                &["pr:123"],
            ),
        )
        .unwrap();
    db.insert_eval_result(
        stale_run_id,
        turn_8_row_id,
        Some(low_memory_id),
        "insufficient_context",
        "not enough later turns",
        "2026-06-08T00:00:09Z",
    )
    .unwrap();
    db.complete_eval_run(stale_run_id, "2026-06-08T00:00:10Z")
        .unwrap();
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: "recall_eval".to_string(),
        status: TaskStatus::Queued,
        priority: 10,
        payload_json: r#"{"session_id":"session-1","turn_ordinal":9,"recall_text":"","memory_ids":[],"recall_origin":"tool_pre_use","tool_name":"Bash","injected":false,"tool_input_summary":"cargo test"}"#.to_string(),
        attempts: 1,
        max_attempts: 5,
        next_run_at: Some("unix:1781206000".to_string()),
        last_error: Some("waiting for subsequent turns before recall eval".to_string()),
        created_at: "unix:1781205400".to_string(),
        updated_at: "unix:1781205400".to_string(),
    })
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("summary")
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
    assert_eq!(value["runs_considered"], 2);
    assert_eq!(value["results_considered"], 3);
    assert_eq!(value["judged_results"], 2);
    assert_eq!(value["average_score"], 3.5);
    assert_eq!(value["score_counts"]["2"], 1);
    assert_eq!(value["score_counts"]["5"], 1);
    assert_eq!(value["score_counts"]["n/a"], 1);
    assert_eq!(
        value["low_score_examples"][0]["memory_title"],
        "Weak memory"
    );
    assert_eq!(
        value["high_score_examples"][0]["memory_title"],
        "Useful memory"
    );
    assert_eq!(value["high_score_examples"][0]["session_id"], "session-1");
    assert_eq!(value["high_score_examples"][0]["turn_ordinal"], 7);
    assert_eq!(value["session_breakdown"][0]["session_id"], "session-1");
    assert_eq!(
        value["session_breakdown"][0]["project_id"],
        project.display().to_string()
    );
    assert_eq!(value["session_breakdown"][0]["runs"], 2);
    assert_eq!(value["session_breakdown"][0]["results"], 3);
    assert_eq!(value["session_breakdown"][0]["average_score"], 3.5);
    assert_eq!(value["session_breakdown"][0]["score_counts"]["n/a"], 1);
    assert_eq!(
        value["conversation_segment_breakdown"][0]["session_id"],
        "session-1"
    );
    assert_eq!(
        value["conversation_segment_breakdown"][0]["start_turn_ordinal"],
        7
    );
    assert_eq!(
        value["conversation_segment_breakdown"][0]["end_turn_ordinal"],
        8
    );
    assert_eq!(
        value["conversation_segment_breakdown"][0]["summary"],
        "Turns 7..=8 evaluate whether recalled context helped."
    );
    assert_eq!(
        value["conversation_segment_breakdown"][0]["task_keys"][0],
        "pr:123"
    );
    assert_eq!(value["conversation_segment_breakdown"][0]["runs"], 2);
    assert_eq!(value["conversation_segment_breakdown"][0]["results"], 3);
    assert_eq!(
        value["conversation_segment_breakdown"][0]["score_counts"]["n/a"],
        1
    );
    assert_eq!(
        value["stale_insufficient_context"][0]["run_id"],
        stale_run_id
    );
    assert_eq!(
        value["stale_insufficient_context"][0]["later_completed_turns"],
        1
    );
    assert_eq!(
        value["stale_insufficient_context"][0]["requeue_status"],
        "actionable"
    );
    assert_eq!(value["queued_recall_evals"][0]["session_id"], "session-1");
    assert_eq!(value["queued_recall_evals"][0]["turn_ordinal"], 9);
    assert_eq!(
        value["queued_recall_evals"][0]["recall_origin"],
        "tool_pre_use"
    );
    assert_eq!(value["queued_recall_evals"][0]["tool_name"], "Bash");
    assert_eq!(value["queued_recall_evals"][0]["injected"], false);
    assert_eq!(value["queued_recall_evals"][0]["memory_count"], 0);
    assert_eq!(
        value["queued_recall_evals"][0]["tool_input_summary"],
        "cargo test"
    );

    let recent = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .arg("--json")
        .arg("--since")
        .arg("unix:1780876805")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        recent.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&recent.stderr)
    );
    let recent_value: serde_json::Value = serde_json::from_slice(&recent.stdout).unwrap();
    assert_eq!(recent_value["runs_considered"], 1);
    assert_eq!(recent_value["results_considered"], 1);
    assert_eq!(recent_value["judged_results"], 0);
    assert_eq!(recent_value["score_counts"]["n/a"], 1);
    assert!(recent_value["low_score_examples"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(recent_value["high_score_examples"]
        .as_array()
        .unwrap()
        .is_empty());

    let replay_only = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .arg("--json")
        .arg("--origin")
        .arg("replay")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        replay_only.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&replay_only.stderr)
    );
    let replay_value: serde_json::Value = serde_json::from_slice(&replay_only.stdout).unwrap();
    assert_eq!(replay_value["runs_considered"], 1);
    assert_eq!(replay_value["results_considered"], 2);
    assert_eq!(replay_value["judged_results"], 2);
    assert_eq!(replay_value["origin_breakdown"][0]["name"], "replay");
    assert_eq!(
        replay_value["low_score_examples"][0]["memory_title"],
        "Weak memory"
    );
    assert_eq!(
        replay_value["high_score_examples"][0]["memory_title"],
        "Useful memory"
    );

    let without_replay = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .arg("--json")
        .arg("--exclude-origin")
        .arg("replay")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        without_replay.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&without_replay.stderr)
    );
    let without_replay_value: serde_json::Value =
        serde_json::from_slice(&without_replay.stdout).unwrap();
    assert_eq!(without_replay_value["runs_considered"], 1);
    assert_eq!(without_replay_value["results_considered"], 1);
    assert_eq!(without_replay_value["score_counts"]["n/a"], 1);

    let shown = Command::new(binary)
        .arg("eval")
        .arg("show")
        .arg(stale_run_id.to_string())
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown_value: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        shown_value["recalled_memories"][0]["memory_id"],
        low_memory_id
    );
    assert_eq!(shown_value["recalled_memories"][0]["title"], "Weak memory");
    assert_eq!(shown_value["later_completed_turns"], 1);

    let human = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(human_stdout.contains("Eval summary"));
    assert!(human_stdout.contains("average score: 3.50"));
    assert!(human_stdout.contains("Conversation segment breakdown"));
    assert!(human_stdout.contains("turns=7..=8"));
    assert!(human_stdout.contains("summary: Turns 7..=8 evaluate whether recalled context helped."));
    assert!(human_stdout.contains("keys: pr:123"));
    assert!(human_stdout.contains("Session breakdown"));
    assert!(human_stdout.contains("N/a evals needing attention"));
    assert!(human_stdout.contains("status=actionable"));
    assert!(human_stdout.contains("Queued recall evals"));
    assert!(human_stdout.contains("origin=tool_pre_use"));
    assert!(human_stdout.contains("tool=Bash"));
    assert!(human_stdout.contains("memories=0"));
    assert!(human_stdout.contains("input: cargo test"));
    assert!(human_stdout.contains("Weak memory"));
    assert!(human_stdout.contains("Useful memory"));

    let filtered_summary = Command::new(binary)
        .arg("eval")
        .arg("summary")
        .arg("--since-run")
        .arg(stale_run_id.to_string())
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        filtered_summary.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered_summary.stderr)
    );
    let filtered_value: serde_json::Value =
        serde_json::from_slice(&filtered_summary.stdout).unwrap();
    assert_eq!(filtered_value["runs_considered"], 1);
    assert_eq!(filtered_value["results_considered"], 1);
    assert_eq!(filtered_value["score_counts"]["n/a"], 1);
    assert_eq!(
        filtered_value["stale_insufficient_context"][0]["run_id"],
        stale_run_id
    );
    assert_eq!(
        filtered_value["stale_insufficient_context"][0]["requeue_status"],
        "actionable"
    );
}

#[test]
fn eval_memories_reports_memory_level_mixed_scores() {
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
        ordinal: 7,
        byte_start: 0,
        byte_end: 10,
        observed_at: Some("2026-06-08T00:00:02Z".to_string()),
        status: TurnStatus::Completed,
        display_text: Some("use recall".to_string()),
        cwd: None,
        context: None,
    })
    .unwrap();
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 7)
        .unwrap()
        .unwrap();
    let mixed_memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Context-sensitive memory".to_string(),
            body: "Useful in one task and noisy in another".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let low_memory_id = db
        .insert_memory(&MemoryRecord {
            id: None,
            title: "Mostly bad memory".to_string(),
            body: "Usually unrelated".to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::TaskState,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            updated_at: "2026-06-08T00:00:01Z".to_string(),
            is_active: true,
            session_id: None,
            project_id: Some(project.display().to_string()),
            project_descriptor: Some("yaaml".to_string()),
            lineage_refs: Vec::new(),
            origin_segment_id: None,
            origin_segment_status: None,
            validity: yaaml_core::MemoryValidity::Durable,
        })
        .unwrap();
    let first_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "2026-06-08T00:00:03Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
            eval_metadata(7, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        first_run_id,
        turn_row_id,
        Some(mixed_memory_id),
        "2",
        "too broad",
        "2026-06-08T00:00:04Z",
    )
    .unwrap();
    db.insert_eval_result(
        first_run_id,
        turn_row_id,
        Some(low_memory_id),
        "1",
        "wrong task",
        "2026-06-08T00:00:04Z",
    )
    .unwrap();
    db.complete_eval_run(first_run_id, "2026-06-08T00:00:05Z")
        .unwrap();
    let second_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "2026-06-08T00:00:06Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
            eval_metadata(7, "session_background"),
        )
        .unwrap();
    db.insert_eval_result(
        second_run_id,
        turn_row_id,
        Some(mixed_memory_id),
        "5",
        "directly actionable",
        "2026-06-08T00:00:07Z",
    )
    .unwrap();
    db.complete_eval_run(second_run_id, "2026-06-08T00:00:08Z")
        .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("eval")
        .arg("memories")
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
    assert_eq!(value["eval_runs_considered"], 2);
    assert_eq!(value["result_rows_considered"], 3);
    assert_eq!(value["memories_considered"], 2);
    assert_eq!(value["memories"][0]["memory_id"], mixed_memory_id);
    assert_eq!(value["memories"][0]["selected_count"], 2);
    assert_eq!(value["memories"][0]["useful_count"], 1);
    assert_eq!(value["memories"][0]["low_count"], 1);
    assert_eq!(value["memories"][0]["average_score"], 3.5);
    assert_eq!(value["memories"][0]["mixed_useful_and_low"], true);
    assert_eq!(value["memories"][0]["latest_run_id"], second_run_id);
    assert_eq!(value["memories"][0]["latest_score"], "5");

    let filtered = Command::new(binary)
        .arg("eval")
        .arg("memories")
        .arg("--since-run")
        .arg(second_run_id.to_string())
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        filtered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered_value: serde_json::Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(filtered_value["eval_runs_considered"], 1);
    assert_eq!(filtered_value["result_rows_considered"], 1);
    assert_eq!(filtered_value["memories_considered"], 1);
    assert_eq!(filtered_value["memories"][0]["memory_id"], mixed_memory_id);
    assert_eq!(filtered_value["memories"][0]["average_score"], 5.0);

    let low_sorted = Command::new(binary)
        .arg("eval")
        .arg("memories")
        .arg("--sort")
        .arg("low")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        low_sorted.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&low_sorted.stderr)
    );
    let low_value: serde_json::Value = serde_json::from_slice(&low_sorted.stdout).unwrap();
    assert_eq!(low_value["sort"], "low");
    assert_eq!(low_value["memories"][0]["memory_id"], mixed_memory_id);

    let human = Command::new(binary)
        .arg("eval")
        .arg("memories")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("Eval memory diagnostics"));
    assert!(stdout.contains("Context-sensitive memory"));
    assert!(stdout.contains("mixed=true"));
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

fn insert_transcript_backed_turn(db: &Database, root: &Path, project: &Path, text: &str) {
    let transcript_path = root.join("session.jsonl");
    let project_id = project.display().to_string();
    let lines = [
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "session-1",
                "timestamp": "2026-06-08T00:00:00Z",
                "cwd": project_id,
            },
        }),
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "task_started",
                "turn_id": "turn-1",
            },
        }),
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:01Z",
            "type": "turn_context",
            "payload": {
                "turn_id": "turn-1",
                "cwd": project_id,
            },
        }),
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:02Z",
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
        serde_json::json!({
            "timestamp": "2026-06-08T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "turn_id": "turn-1",
            },
        }),
    ];
    let transcript = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&transcript_path, &transcript).unwrap();

    db.upsert_session(&SessionRecord {
        id: "session-1".to_string(),
        agent_type: AgentType::Codex,
        project_id: project_id.clone(),
        transcript_file_path: transcript_path.display().to_string(),
        started_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_seen_at: Some("2026-06-08T00:00:03Z".to_string()),
    })
    .unwrap();
    db.insert_turn(&TurnRecord {
        session_id: "session-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        ordinal: 0,
        byte_start: 0,
        byte_end: transcript.len() as u64,
        observed_at: Some("2026-06-08T00:00:03Z".to_string()),
        status: TurnStatus::Completed,
        display_text: None,
        cwd: Some(project_id),
        context: Some(yaaml_core::infer_context_from_path(project)),
    })
    .unwrap();
}

fn eval_metadata(turn_ordinal: u64, recall_origin: &str) -> EvalRunMetadata {
    EvalRunMetadata {
        session_id: Some("session-1".to_string()),
        turn_ordinal: Some(turn_ordinal),
        recall_origin: recall_origin.to_string(),
        ..EvalRunMetadata::default()
    }
}

fn eval_metadata_with_segment(
    turn_ordinal: u64,
    recall_origin: &str,
    segment_start_turn_ordinal: u64,
    segment_end_turn_ordinal: u64,
    segment_summary: &str,
    segment_task_keys: &[&str],
) -> EvalRunMetadata {
    EvalRunMetadata {
        segment_start_turn_ordinal: Some(segment_start_turn_ordinal),
        segment_end_turn_ordinal: Some(segment_end_turn_ordinal),
        segment_summary: Some(segment_summary.to_string()),
        segment_task_keys: segment_task_keys
            .iter()
            .map(|task_key| (*task_key).to_string())
            .collect(),
        ..eval_metadata(turn_ordinal, recall_origin)
    }
}
