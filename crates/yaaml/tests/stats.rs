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
    assert_eq!(stats["recall_runs"], 3);
    assert_eq!(stats["non_empty_recall_runs"], 1);
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

fn recall_metadata(turn_ordinal: u64) -> EvalRunMetadata {
    EvalRunMetadata {
        session_id: Some("session-1".to_string()),
        turn_ordinal: Some(turn_ordinal),
        recall_origin: "session_background".to_string(),
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
