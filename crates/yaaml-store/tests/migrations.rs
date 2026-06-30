use std::collections::BTreeSet;

use yaaml_store::Database;

#[test]
fn migration_creates_expected_tables_and_is_idempotent() {
    let mut db = Database::in_memory().unwrap();

    db.migrate().unwrap();
    db.migrate().unwrap();

    assert_eq!(db.schema_version().unwrap(), 7);
    let tables = db
        .table_names()
        .unwrap()
        .into_iter()
        .collect::<BTreeSet<_>>();
    for expected in [
        "backlog_progress",
        "context_metadata",
        "conversation_segments",
        "embeddings",
        "eval_results",
        "eval_runs",
        "file_cursors",
        "memories",
        "schema_version",
        "sessions",
        "tasks",
        "turns",
    ] {
        assert!(tables.contains(expected), "missing table {expected}");
    }
    let eval_run_columns = db.column_names("eval_runs").unwrap();
    for expected in [
        "session_id",
        "turn_ordinal",
        "agent_turn_id",
        "recall_origin",
        "tool_name",
        "tool_use_id",
        "tool_input_summary",
        "injected",
        "segment_start_turn_ordinal",
        "segment_end_turn_ordinal",
        "segment_summary",
        "segment_task_keys",
    ] {
        assert!(
            eval_run_columns.contains(&expected.to_string()),
            "missing eval_runs column {expected}"
        );
    }
    let memory_columns = db.column_names("memories").unwrap();
    for expected in ["origin_segment_id", "validity", "superseded_by_memory_id"] {
        assert!(
            memory_columns.contains(&expected.to_string()),
            "missing memories column {expected}"
        );
    }
}
