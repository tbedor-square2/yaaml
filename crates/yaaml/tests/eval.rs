use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use yaaml_core::{
    AgentType, MemoryKind, MemoryRecord, MemoryScope, SessionRecord, TaskRecord, TaskStatus,
    TurnRecord, TurnStatus,
};
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
        cwd: None,
        context: None,
    })
    .unwrap();
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
        .insert_eval_run(
            "recall_1_to_5",
            "unix:1781205326",
            r#"{"session_id":"session-1","turn_ordinal":7,"memory_ids":[1]}"#,
        )
        .unwrap();
    db.insert_eval_result(run_id, turn_row_id, None, "5", "great", "unix:1781205330")
        .unwrap();
    db.complete_eval_run(run_id, "unix:1781205331").unwrap();
    let insufficient_run_id = db
        .insert_eval_run(
            "recall_1_to_5",
            "unix:1781205400",
            r#"{"session_id":"session-1","turn_ordinal":8,"memory_ids":[1]}"#,
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
        cwd: None,
        context: None,
    })
    .unwrap();
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
        })
        .unwrap();
    let run_id = db
        .insert_eval_run(
            "default",
            "2026-06-08T00:00:03Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
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
        .insert_eval_run(
            "recall_1_to_5",
            "2026-06-08T00:00:08Z",
            &format!(
                r#"{{"session_id":"session-1","turn_ordinal":8,"memory_ids":[{}]}}"#,
                low_memory_id
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
        payload_json: r#"{"session_id":"session-1","turn_ordinal":9}"#.to_string(),
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
        value["stale_insufficient_context"][0]["run_id"],
        stale_run_id
    );
    assert_eq!(
        value["stale_insufficient_context"][0]["later_completed_turns"],
        1
    );
    assert_eq!(value["queued_recall_evals"][0]["session_id"], "session-1");
    assert_eq!(value["queued_recall_evals"][0]["turn_ordinal"], 9);

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
    assert!(human_stdout.contains("Session breakdown"));
    assert!(human_stdout.contains("N/a evals with later turns"));
    assert!(human_stdout.contains("Queued recall evals"));
    assert!(human_stdout.contains("Weak memory"));
    assert!(human_stdout.contains("Useful memory"));
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
        })
        .unwrap();
    let first_run_id = db
        .insert_eval_run(
            "recall_1_to_5",
            "2026-06-08T00:00:03Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
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
        .insert_eval_run(
            "recall_1_to_5",
            "2026-06-08T00:00:06Z",
            r#"{"session_id":"session-1","turn_ordinal":7}"#,
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
