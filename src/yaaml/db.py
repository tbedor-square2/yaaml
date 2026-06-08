"""SQLite database management for YAAML."""

import sqlite3
from pathlib import Path

# Global connection cache
_connections: dict[str, sqlite3.Connection] = {}

SCHEMA_VERSION = 4

CREATE_SCHEMA_VERSION = """
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);
"""

CREATE_SESSIONS = """
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,
    project_id TEXT NOT NULL,
    transcript_file_path TEXT NOT NULL,
    started_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);
"""

CREATE_TURNS = """
CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content_json TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);
"""

CREATE_MEMORIES = """
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    source_turn_ids TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    session_id TEXT,
    project_id TEXT NOT NULL,
    consolidated_into TEXT
);
"""

CREATE_FILE_CURSORS = """
CREATE TABLE IF NOT EXISTS file_cursors (
    file_path TEXT PRIMARY KEY,
    last_byte_offset INTEGER NOT NULL DEFAULT 0,
    last_processed_at TEXT NOT NULL
);
"""

CREATE_RECALL_STATE = """
CREATE TABLE IF NOT EXISTS recall_state (
    project_id TEXT PRIMARY KEY,
    memory_ids TEXT NOT NULL,
    last_recall_at TEXT NOT NULL
);
"""

CREATE_EMBEDDING_JOBS = """
CREATE TABLE IF NOT EXISTS embedding_jobs (
    memory_id TEXT PRIMARY KEY,
    source_memory_ids TEXT NOT NULL DEFAULT '[]',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (memory_id) REFERENCES memories(id)
);
"""

CREATE_BACKGROUND_JOBS = """
CREATE TABLE IF NOT EXISTS background_jobs (
    id TEXT PRIMARY KEY,
    task_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL
);
"""

# V2: add turn_group_id so all rows from one logical turn share a stable key,
# and add indexes on the columns that appear in every hot query path.
_V2_STATEMENTS = [
    "ALTER TABLE turns ADD COLUMN turn_group_id TEXT",
    "CREATE INDEX IF NOT EXISTS idx_sessions_project_id ON sessions(project_id)",
    "CREATE INDEX IF NOT EXISTS idx_turns_session_id ON turns(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_turns_observed_at ON turns(observed_at)",
    "CREATE INDEX IF NOT EXISTS idx_turns_turn_group_id ON turns(turn_group_id)",
    "CREATE INDEX IF NOT EXISTS idx_memories_project_active ON memories(project_id, is_active)",
    "CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at)",
]

MIGRATIONS: list[tuple[int, list[str]]] = [
    (
        1,
        [
            CREATE_SCHEMA_VERSION,
            CREATE_SESSIONS,
            CREATE_TURNS,
            CREATE_MEMORIES,
            CREATE_FILE_CURSORS,
            CREATE_RECALL_STATE,
        ],
    ),
    (2, _V2_STATEMENTS),
    (
        3,
        [
            CREATE_EMBEDDING_JOBS,
            "CREATE INDEX IF NOT EXISTS idx_embedding_jobs_due ON embedding_jobs(next_attempt_at)",
        ],
    ),
    (
        4,
        [
            CREATE_BACKGROUND_JOBS,
            "CREATE INDEX IF NOT EXISTS idx_background_jobs_due "
            "ON background_jobs(task_type, next_attempt_at)",
        ],
    ),
]


def _get_schema_version(conn: sqlite3.Connection) -> int:
    """Get current schema version from DB, 0 if not initialized."""
    try:
        cursor = conn.execute("SELECT version FROM schema_version LIMIT 1")
        row = cursor.fetchone()
        if row is None:
            return 0
        return int(row[0])
    except sqlite3.OperationalError:
        return 0


def init_db(db_path: Path) -> sqlite3.Connection:
    """Initialize the database, running migrations as needed.

    Args:
        db_path: Path to the SQLite database file.

    Returns:
        Open sqlite3.Connection in WAL mode.
    """
    db_path.parent.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(str(db_path), check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")

    current_version = _get_schema_version(conn)

    for version, statements in MIGRATIONS:
        if current_version < version:
            for statement in statements:
                conn.execute(statement)
            existing = conn.execute("SELECT COUNT(*) FROM schema_version").fetchone()[0]
            if existing == 0:
                conn.execute("INSERT INTO schema_version (version) VALUES (?)", (version,))
            else:
                conn.execute("UPDATE schema_version SET version = ?", (version,))
            conn.commit()
            current_version = version

    _connections[str(db_path)] = conn
    return conn


def get_db(db_path: Path) -> sqlite3.Connection:
    """Return a cached database connection, initializing if needed.

    Args:
        db_path: Path to the SQLite database file.

    Returns:
        Open sqlite3.Connection.
    """
    key = str(db_path)
    if key not in _connections:
        return init_db(db_path)
    return _connections[key]
