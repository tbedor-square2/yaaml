use std::collections::BTreeSet;

use yaaml_store::Database;

#[test]
fn migration_creates_expected_tables_and_is_idempotent() {
    let mut db = Database::in_memory().unwrap();

    db.migrate().unwrap();
    db.migrate().unwrap();

    assert_eq!(db.schema_version().unwrap(), 2);
    let tables = db
        .table_names()
        .unwrap()
        .into_iter()
        .collect::<BTreeSet<_>>();
    for expected in [
        "backlog_progress",
        "context_metadata",
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
}
