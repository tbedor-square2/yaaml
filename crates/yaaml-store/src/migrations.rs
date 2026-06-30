pub const EXPECTED_SCHEMA_VERSION: i64 = 7;

pub const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

INSERT INTO schema_version (version)
SELECT 0
WHERE NOT EXISTS (SELECT 1 FROM schema_version);

CREATE TABLE IF NOT EXISTS file_cursors (
    file_path TEXT PRIMARY KEY NOT NULL,
    last_byte_offset INTEGER NOT NULL DEFAULT 0,
    last_processed_at TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY NOT NULL,
    agent_type TEXT NOT NULL,
    project_id TEXT NOT NULL,
    transcript_file_path TEXT NOT NULL UNIQUE,
    started_at TEXT,
    last_seen_at TEXT
);

CREATE TABLE IF NOT EXISTS turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    ordinal INTEGER NOT NULL,
    byte_start INTEGER NOT NULL,
    byte_end INTEGER NOT NULL,
    observed_at TEXT,
    status TEXT NOT NULL,
    display_text TEXT,
    cwd TEXT,
    context_json TEXT,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);

CREATE INDEX IF NOT EXISTS idx_turns_session_ordinal ON turns(session_id, ordinal);
CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_session_ordinal_unique ON turns(session_id, ordinal);

CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    scope TEXT NOT NULL,
    memory_kind TEXT NOT NULL DEFAULT 'lesson',
    task_keys TEXT NOT NULL DEFAULT '[]',
    source_turn_refs TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    session_id TEXT,
    project_id TEXT,
    project_descriptor TEXT,
    lineage_refs TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_memories_active ON memories(is_active);
CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);

CREATE TABLE IF NOT EXISTS context_metadata (
    entity_type TEXT NOT NULL,
    entity_key TEXT NOT NULL,
    context_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(entity_type, entity_key)
);

CREATE TABLE IF NOT EXISTS embeddings (
    memory_id INTEGER PRIMARY KEY NOT NULL,
    embedding_model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    embedding_blob BLOB NOT NULL,
    embedded_text_hash TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(memory_id) REFERENCES memories(id)
);

CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    payload_json TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    next_run_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_status_priority ON tasks(status, priority DESC, id);

CREATE TABLE IF NOT EXISTS backlog_progress (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    discovered_files INTEGER NOT NULL DEFAULT 0,
    processed_files INTEGER NOT NULL DEFAULT 0,
    processed_turns INTEGER NOT NULL DEFAULT 0,
    queued_memory_jobs INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    last_activity_at TEXT
);

INSERT INTO backlog_progress (id)
VALUES (1)
ON CONFLICT(id) DO NOTHING;

CREATE TABLE IF NOT EXISTS eval_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    config_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS eval_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    eval_run_id INTEGER NOT NULL,
    turn_id INTEGER NOT NULL,
    memory_id INTEGER,
    judge_score TEXT,
    rationale TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(eval_run_id) REFERENCES eval_runs(id)
);

UPDATE schema_version SET version = 1;
"#,
    r#"
UPDATE turns
SET display_text = NULL
WHERE display_text IS NOT NULL;

UPDATE schema_version SET version = 2;
"#,
    r#"
UPDATE schema_version SET version = 3;
"#,
    r#"
CREATE TABLE IF NOT EXISTS conversation_segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    start_turn_ordinal INTEGER NOT NULL,
    end_turn_ordinal INTEGER NOT NULL,
    summary TEXT NOT NULL,
    task_keys TEXT NOT NULL DEFAULT '[]',
    context_json TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);

UPDATE schema_version SET version = 4;
"#,
    r#"
ALTER TABLE eval_runs ADD COLUMN segment_start_turn_ordinal INTEGER;
ALTER TABLE eval_runs ADD COLUMN segment_end_turn_ordinal INTEGER;
ALTER TABLE eval_runs ADD COLUMN segment_summary TEXT;
ALTER TABLE eval_runs ADD COLUMN segment_task_keys TEXT NOT NULL DEFAULT '[]';

UPDATE schema_version SET version = 5;
"#,
    r#"
ALTER TABLE memories ADD COLUMN origin_segment_id INTEGER;
ALTER TABLE memories ADD COLUMN validity TEXT NOT NULL DEFAULT 'durable';

UPDATE schema_version SET version = 6;
"#,
    r#"
ALTER TABLE memories ADD COLUMN superseded_by_memory_id INTEGER;

UPDATE schema_version SET version = 7;
"#,
];
