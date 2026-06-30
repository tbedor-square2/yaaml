use std::sync::{Arc, Barrier};
use std::thread;

use tempfile::TempDir;
use yaaml_core::{MemoryKind, MemoryRecord, MemoryScope, MemoryValidity};
use yaaml_store::Database;

fn memory(title: &str) -> MemoryRecord {
    MemoryRecord {
        id: None,
        title: title.to_string(),
        body: "body".to_string(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Lesson,
        task_keys: Vec::new(),
        source_turn_refs: Vec::new(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        is_active: true,
        session_id: None,
        project_id: Some("/tmp/yaaml".to_string()),
        project_descriptor: Some("yaaml".to_string()),
        lineage_refs: Vec::new(),
        origin_segment_id: None,
        origin_segment_status: None,
        validity: MemoryValidity::Durable,
        superseded_by_memory_id: None,
    }
}

#[test]
fn concurrent_readers_and_writers_share_wal_database() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("yaaml.db");
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();
    drop(db);

    let barrier = Arc::new(Barrier::new(2));
    let writer_path = db_path.clone();
    let writer_barrier = Arc::clone(&barrier);
    let writer = thread::spawn(move || {
        let db = Database::open(writer_path).unwrap();
        writer_barrier.wait();
        for index in 0..25 {
            db.insert_memory(&memory(&format!("memory {index}")))
                .unwrap();
        }
    });

    let reader_path = db_path.clone();
    let reader_barrier = Arc::clone(&barrier);
    let reader = thread::spawn(move || {
        let db = Database::open(reader_path).unwrap();
        reader_barrier.wait();
        for _ in 0..25 {
            let status = db.status().unwrap();
            assert!(status.memory_count <= 25);
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    let db = Database::open(&db_path).unwrap();
    let status = db.status().unwrap();
    assert_eq!(status.memory_count, 25);
    assert_eq!(status.active_memory_count, 25);
}
