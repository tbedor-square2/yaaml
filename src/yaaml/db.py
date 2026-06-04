"""SQLite database management for YAAML."""

import sqlite3
from pathlib import Path

# Global connection cache
_connections: dict[str, sqlite3.Connection] = {}

SCHEMA_VERSION = 1

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

MIGRATIONS: list[tuple[int, list[str]]] = [
    (1, [
        CREATE_SCHEMA_VERSION,
        CREATE_SESSIONS,
        CREATE_TURNS,
        CREATE_MEMORIES,
        CREATE_FILE_CURSORS,
        CREATE_RECALL_STATE,
    ]),
]


def _get_schema_version(conn: sqlite3.Connection) -> int:
    """Get current schema version from DB, 0 if not initialized."""
    try:
        cursor = conn.execute("SELECT version FROM schema_version LIMIT 1")
        row = cursor.fetchone()
        if row is None:
            return 0
        return row[0]
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
            # Update or insert schema version
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
