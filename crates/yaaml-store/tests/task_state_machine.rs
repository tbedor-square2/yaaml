use yaaml_core::{TaskRecord, TaskStatus};
use yaaml_store::Database;

fn task(kind: &str, priority: i64) -> TaskRecord {
    TaskRecord {
        id: None,
        kind: kind.to_string(),
        status: TaskStatus::Queued,
        priority,
        payload_json: "{}".to_string(),
        attempts: 0,
        max_attempts: 5,
        next_run_at: None,
        last_error: None,
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
    }
}

#[test]
fn task_lifecycle_supports_running_completion_parking_and_requeue() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    let complete_id = db.enqueue_task(&task("memory_formulation", 10)).unwrap();
    let parked_id = db.enqueue_task(&task("recall_eval", 0)).unwrap();
    let requeued_id = db.enqueue_task(&task("recall", 5)).unwrap();

    let next = db.next_queued_task(0).unwrap().unwrap();
    assert_eq!(next.id, Some(complete_id));

    db.mark_task_running(complete_id, "2026-06-08T00:00:01Z")
        .unwrap();
    let running_task = db
        .list_tasks(Some("running"), 10)
        .unwrap()
        .into_iter()
        .find(|task| task.id == complete_id)
        .unwrap();
    assert_eq!(running_task.next_run_at, None);
    assert_eq!(running_task.last_error, None);
    assert_eq!(
        db.count_tasks_by_status("memory_formulation", TaskStatus::Running)
            .unwrap(),
        1
    );
    db.complete_task(complete_id, "2026-06-08T00:00:02Z")
        .unwrap();
    assert_eq!(
        db.count_tasks_by_status("memory_formulation", TaskStatus::Completed)
            .unwrap(),
        1
    );
    let completed_task = db
        .list_tasks(Some("completed"), 10)
        .unwrap()
        .into_iter()
        .find(|task| task.id == complete_id)
        .unwrap();
    assert_eq!(completed_task.next_run_at, None);
    assert_eq!(completed_task.last_error, None);

    db.park_task(parked_id, "missing provider key", "2026-06-08T00:00:03Z")
        .unwrap();
    let status = db.status().unwrap();
    assert_eq!(status.parked_jobs, 1);
    assert_eq!(status.recent_failures.len(), 1);
    assert_eq!(status.recent_failures[0].error, "missing provider key");

    db.mark_task_running(requeued_id, "2026-06-08T00:00:04Z")
        .unwrap();
    assert_eq!(db.requeue_running_tasks("2026-06-08T00:00:05Z").unwrap(), 1);
    assert_eq!(
        db.count_tasks_by_status("recall", TaskStatus::Queued)
            .unwrap(),
        1
    );
}

#[test]
fn future_queued_tasks_are_not_returned_until_due() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    let mut future = task("recall", 0);
    future.next_run_at = Some("unix:200".to_string());
    db.enqueue_task(&future).unwrap();

    assert!(db.next_queued_task(199).unwrap().is_none());
    assert!(db.next_queued_task(200).unwrap().is_some());
}

#[test]
fn task_running_and_completed_transitions_clear_stale_retry_metadata() {
    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();

    let mut stale = task("recall_eval", 0);
    stale.next_run_at = Some("unix:200".to_string());
    stale.last_error = Some("waiting for subsequent turns before recall eval".to_string());
    let task_id = db.enqueue_task(&stale).unwrap();

    db.mark_task_running(task_id, "2026-06-08T00:00:01Z")
        .unwrap();
    let running_task = db
        .list_tasks(Some("running"), 10)
        .unwrap()
        .into_iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(running_task.next_run_at, None);
    assert_eq!(running_task.last_error, None);

    db.reschedule_task(
        task_id,
        1,
        "unix:300",
        "temporary provider failure",
        "2026-06-08T00:00:02Z",
    )
    .unwrap();
    db.complete_task(task_id, "2026-06-08T00:00:03Z").unwrap();
    let completed_task = db
        .list_tasks(Some("completed"), 10)
        .unwrap()
        .into_iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(completed_task.next_run_at, None);
    assert_eq!(completed_task.last_error, None);
}
