use std::path::Path;

use yaaml_store::Database;
use yaaml_transcript::codex::parse_codex_jsonl;

#[test]
fn parsed_codex_turn_persists_with_cursor() {
    let input = concat!(
        r#"{"timestamp":"2026-06-08T00:00:00Z","type":"session_meta","payload":{"id":"session-1","timestamp":"2026-06-08T00:00:00Z","cwd":"/tmp/yaaml"}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:02Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/tmp/yaaml"}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"build milestone two"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-06-08T00:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
        "\n",
    );
    let path = Path::new("/tmp/codex-session.jsonl");
    let parsed = parse_codex_jsonl(path, input.as_bytes(), 0).unwrap();

    let mut db = Database::in_memory().unwrap();
    db.migrate().unwrap();
    db.upsert_session(&parsed.session).unwrap();

    assert_eq!(parsed.turns.len(), 1);
    assert!(db.insert_turn(&parsed.turns[0]).unwrap());
    assert!(!db.insert_turn(&parsed.turns[0]).unwrap());

    db.update_cursor(
        &path.display().to_string(),
        parsed.next_offset,
        parsed.turns[0].observed_at.as_deref(),
    )
    .unwrap();

    assert_eq!(
        db.get_cursor(&path.display().to_string()).unwrap(),
        input.len() as u64
    );
}
