use std::fs;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;
use yaaml_core::{AgentType, SessionRecord, TaskRecord, TaskStatus, TurnRecord, TurnStatus};
use yaaml_store::database::EvalRunMetadata;
use yaaml_store::Database;

#[test]
fn stats_json_reports_recall_rates_volume_and_usefulness() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
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
        last_seen_at: Some("2026-06-08T00:01:00Z".to_string()),
    })
    .unwrap();
    for ordinal in 0..3 {
        db.insert_turn(&TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal * 10,
            byte_end: ordinal * 10 + 5,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: Some("turn text".to_string()),
            cwd: None,
            context: None,
        })
        .unwrap();
    }

    enqueue_task(
        &db,
        "recall",
        json!({"session_id": "session-1", "turn_ordinal": 0}).to_string(),
    );
    enqueue_task(
        &db,
        "recall",
        json!({"session_id": "session-1", "turn_ordinal": 1}).to_string(),
    );
    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 0,
            "recall_text": "remember this",
            "memory_ids": [11, 12],
            "recall_origin": "session_background",
            "filter_telemetry": {
                "candidate_count": 4,
                "deterministic_selected_count": 3,
                "final_selected_count": 2,
                "llm_attempted": true,
                "llm_applied": true,
                "llm_error": null
            }
        })
        .to_string(),
    );
    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 1,
            "recall_text": "replay recall",
            "memory_ids": [31],
            "recall_origin": "replay"
        })
        .to_string(),
    );
    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 1,
            "recall_text": "",
            "memory_ids": [],
            "recall_origin": "session_background"
        })
        .to_string(),
    );
    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 2,
            "recall_text": "",
            "memory_ids": [],
            "recall_origin": "session_background"
        })
        .to_string(),
    );

    let run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:100",
            &json!({"session_id": "session-1", "turn_ordinal": 0}).to_string(),
            recall_metadata(0),
        )
        .unwrap();
    let turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 0)
        .unwrap()
        .unwrap();
    db.insert_eval_result(run_id, turn_row_id, Some(11), "5", "useful", "unix:101")
        .unwrap();
    db.insert_eval_result(run_id, turn_row_id, Some(12), "2", "noisy", "unix:101")
        .unwrap();
    db.complete_eval_run(run_id, "unix:102").unwrap();
    let clean_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:110",
            &json!({"session_id": "session-1", "turn_ordinal": 1}).to_string(),
            recall_metadata(1),
        )
        .unwrap();
    let clean_turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 1)
        .unwrap()
        .unwrap();
    db.insert_eval_result(
        clean_run_id,
        clean_turn_row_id,
        Some(21),
        "clean_abstention",
        "no useful recall was missed",
        "unix:111",
    )
    .unwrap();
    db.complete_eval_run(clean_run_id, "unix:112").unwrap();
    let missed_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:120",
            &json!({"session_id": "session-1", "turn_ordinal": 2}).to_string(),
            recall_metadata(2),
        )
        .unwrap();
    let missed_turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 2)
        .unwrap()
        .unwrap();
    db.insert_eval_result(
        missed_run_id,
        missed_turn_row_id,
        Some(31),
        "missed_useful_abstention",
        "a useful memory existed but recall abstained",
        "unix:121",
    )
    .unwrap();
    db.complete_eval_run(missed_run_id, "unix:122").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("stats")
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
    let stats: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stats["eligible_turns"], 3);
    assert_eq!(stats["turns_with_recall"], 3);
    assert_eq!(stats["recall_runs"], 3);
    assert_eq!(stats["recall_runs_per_eligible_turn"], 1.0);
    assert_eq!(stats["non_empty_recall_runs"], 1);
    assert_eq!(stats["non_empty_recall_runs_per_eligible_turn"], 1.0 / 3.0);
    assert!(stats.get("recall_rate").is_none());
    assert!(stats.get("non_empty_recall_rate").is_none());
    assert_eq!(stats["volume"]["average_memories_per_non_empty_run"], 2.0);
    assert_eq!(stats["abstention"]["empty_recall_runs"], 2);
    assert_eq!(stats["abstention"]["evaluated_empty_recall_runs"], 2);
    assert_eq!(stats["abstention"]["clean_abstention_runs"], 1);
    assert_eq!(stats["abstention"]["missed_useful_abstention_runs"], 1);
    assert_eq!(stats["abstention"]["unjudged_empty_recall_runs"], 0);
    assert_eq!(
        stats["abstention"]["missed_useful_abstention_rate_per_evaluated_empty_recall"],
        0.5
    );
    assert_eq!(stats["useful"]["evaluated_recall_runs"], 1);
    assert_eq!(stats["useful"]["useful_recall_runs"], 1);
    assert_eq!(stats["useful"]["good_memory_results"], 1);
    assert_eq!(stats["useful"]["low_memory_results"], 1);
    assert_eq!(stats["llm_filter"]["llm_applied_runs"], 1);
    let removed_key = ["llm_empty", "fallback_runs"].join("_");
    assert!(!stats["llm_filter"]
        .as_object()
        .unwrap()
        .contains_key(&removed_key));
    assert_eq!(
        stats["llm_filter"]["average_dropped_memories_per_applied_run"],
        1.0
    );
}

#[test]
fn stats_json_filters_by_recall_origin() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    let db_path = home.join(".yaaml").join("yaaml.db");
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"db_path = "{}""#, db_path.display()),
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
        last_seen_at: Some("2026-06-08T00:01:00Z".to_string()),
    })
    .unwrap();
    for ordinal in 0..2 {
        db.insert_turn(&TurnRecord {
            session_id: "session-1".to_string(),
            turn_id: Some(format!("turn-{ordinal}")),
            ordinal,
            byte_start: ordinal * 10,
            byte_end: ordinal * 10 + 5,
            observed_at: Some(format!("2026-06-08T00:00:0{ordinal}Z")),
            status: TurnStatus::Completed,
            display_text: Some("turn text".to_string()),
            cwd: None,
            context: None,
        })
        .unwrap();
    }

    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 0,
            "recall_text": "background recall",
            "memory_ids": [11],
            "recall_at": "unix:100",
            "recall_origin": "session_background"
        })
        .to_string(),
    );
    enqueue_task(
        &db,
        "recall_eval",
        json!({
            "session_id": "session-1",
            "turn_ordinal": 1,
            "recall_text": "tool recall",
            "memory_ids": [21],
            "recall_at": "unix:110",
            "recall_origin": "tool_pre_use",
            "tool_name": "Bash"
        })
        .to_string(),
    );

    let background_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:100",
            &json!({"session_id": "session-1", "turn_ordinal": 0}).to_string(),
            recall_metadata_with_origin(0, "session_background"),
        )
        .unwrap();
    let background_turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 0)
        .unwrap()
        .unwrap();
    db.insert_eval_result(
        background_run_id,
        background_turn_row_id,
        Some(11),
        "5",
        "useful",
        "unix:101",
    )
    .unwrap();
    db.complete_eval_run(background_run_id, "unix:102").unwrap();

    let tool_run_id = db
        .insert_eval_run_with_metadata(
            "recall_1_to_5",
            "unix:110",
            &json!({"session_id": "session-1", "turn_ordinal": 1}).to_string(),
            recall_metadata_with_origin(1, "tool_pre_use"),
        )
        .unwrap();
    let tool_turn_row_id = db
        .turn_row_id_for_session_ordinal("session-1", 1)
        .unwrap()
        .unwrap();
    db.insert_eval_result(
        tool_run_id,
        tool_turn_row_id,
        Some(21),
        "1",
        "wrong context",
        "unix:111",
    )
    .unwrap();
    db.complete_eval_run(tool_run_id, "unix:112").unwrap();
    let replay_run_id = db
        .insert_eval_run_with_metadata(
            "default",
            "unix:120",
            &json!({"session_id": "session-1", "turn_ordinal": 1}).to_string(),
            recall_metadata_with_origin(1, "replay"),
        )
        .unwrap();
    db.insert_eval_result(
        replay_run_id,
        tool_turn_row_id,
        Some(31),
        "2",
        "weak backtest recall",
        "unix:121",
    )
    .unwrap();
    db.complete_eval_run(replay_run_id, "unix:122").unwrap();

    let default = stats_json(&home, &project, []);
    assert_eq!(default["filters"]["excluded_origins"], json!(["replay"]));
    assert_eq!(default["recall_runs"], 2);
    assert!(!default["by_origin"]
        .as_array()
        .unwrap()
        .iter()
        .any(|origin| origin["name"] == "replay"));

    let included = stats_json(&home, &project, ["--origin", "session_background"]);
    assert_eq!(
        included["filters"]["origins"],
        json!(["session_background"])
    );
    assert_eq!(included["recall_runs"], 1);
    assert_eq!(included["non_empty_recall_runs"], 1);
    assert_eq!(included["useful"]["evaluated_recall_runs"], 1);
    assert_eq!(included["useful"]["useful_recall_runs"], 1);
    assert_eq!(included["by_origin"].as_array().unwrap().len(), 1);
    assert_eq!(included["by_origin"][0]["name"], "session_background");

    let excluded = stats_json(&home, &project, ["--exclude-origin", "tool_pre_use"]);
    assert_eq!(
        excluded["filters"]["excluded_origins"],
        json!(["tool_pre_use", "replay"])
    );
    assert_eq!(excluded["recall_runs"], 1);
    assert_eq!(excluded["useful"]["low_memory_results"], 0);
    assert_eq!(excluded["by_tool"].as_array().unwrap().len(), 0);

    let recent = stats_json(&home, &project, ["--since", "unix:105"]);
    assert_eq!(recent["filters"]["since_unix"], 105);
    assert_eq!(recent["recall_runs"], 1);
    assert_eq!(recent["non_empty_recall_runs"], 1);
    assert_eq!(recent["by_origin"][0]["name"], "tool_pre_use");
    assert_eq!(recent["by_tool"][0]["name"], "Bash");
    assert_eq!(recent["useful"]["evaluated_recall_runs"], 1);
    assert_eq!(recent["useful"]["low_memory_results"], 1);

    let replay = stats_json(&home, &project, ["--origin", "replay"]);
    assert_eq!(replay["filters"]["origins"], json!(["replay"]));
    assert_eq!(replay["recall_runs"], 1);
    assert_eq!(replay["non_empty_recall_runs"], 0);
    assert_eq!(replay["useful"]["low_memory_results"], 1);
    assert_eq!(replay["by_origin"][0]["name"], "replay");
    assert_eq!(replay["by_origin"][0]["recall_runs"], 1);
    assert_eq!(replay["by_origin"][0]["low_memory_results"], 1);
}

fn stats_json<const N: usize>(
    home: &std::path::Path,
    project: &std::path::Path,
    args: [&str; N],
) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("stats")
        .arg("--json")
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn recall_metadata(turn_ordinal: u64) -> EvalRunMetadata {
    recall_metadata_with_origin(turn_ordinal, "session_background")
}

fn recall_metadata_with_origin(turn_ordinal: u64, origin: &str) -> EvalRunMetadata {
    EvalRunMetadata {
        session_id: Some("session-1".to_string()),
        turn_ordinal: Some(turn_ordinal),
        recall_origin: origin.to_string(),
        ..EvalRunMetadata::default()
    }
}

fn enqueue_task(db: &Database, kind: &str, payload_json: String) {
    db.enqueue_task(&TaskRecord {
        id: None,
        kind: kind.to_string(),
        status: TaskStatus::Completed,
        priority: 0,
        payload_json,
        attempts: 1,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "unix:100".to_string(),
        updated_at: "unix:100".to_string(),
    })
    .unwrap();
}
