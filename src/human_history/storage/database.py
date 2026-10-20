"""SQLite schema (WAL + FTS5) with ``user_version``-based migrations.

Only human-readable conversation text is stored: no tool payloads, reasoning or raw events.
"""

from __future__ import annotations

import sqlite3
from datetime import UTC, datetime
from pathlib import Path

MIGRATIONS: list[str] = [
    # v1
    """
    CREATE TABLE files (
        path TEXT PRIMARY KEY,
        agent TEXT NOT NULL,
        session_key TEXT,
        size INTEGER NOT NULL DEFAULT 0,
        mtime_ns INTEGER NOT NULL DEFAULT 0,
        inode INTEGER NOT NULL DEFAULT 0,
        offset INTEGER NOT NULL DEFAULT 0,
        head BLOB,
        state TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX files_session ON files(session_key);

    CREATE TABLE sessions (
        key TEXT PRIMARY KEY,
        agent TEXT NOT NULL,
        session_id TEXT NOT NULL,
        path TEXT NOT NULL,
        cwd TEXT,
        worktree TEXT,
        repo TEXT,
        branch TEXT,
        agent_title TEXT,
        title TEXT,
        created_at TEXT,
        updated_at TEXT,
        user_count INTEGER NOT NULL DEFAULT 0,
        assistant_count INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX sessions_worktree ON sessions(worktree);
    CREATE INDEX sessions_repo ON sessions(repo);
    CREATE INDEX sessions_updated ON sessions(updated_at);

    CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        session_key TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
        ts TEXT,
        uid TEXT NOT NULL,
        text TEXT NOT NULL,
        UNIQUE (session_key, uid)
    );
    CREATE INDEX messages_order ON messages(session_key, ts, id);

    CREATE VIRTUAL TABLE messages_fts USING fts5(
        text, content='messages', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER messages_au AFTER UPDATE OF text ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
        INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;

    -- User state: survives re-indexing.
    CREATE TABLE favorites (session_key TEXT PRIMARY KEY);
    CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
    """,
]


def connect(path: Path | str) -> sqlite3.Connection:
    if str(path) != ":memory:":
        Path(path).parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, timeout=10, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA foreign_keys=ON")
    migrate(conn)
    return conn


def migrate(conn: sqlite3.Connection) -> None:
    version = conn.execute("PRAGMA user_version").fetchone()[0]
    for number, script in enumerate(MIGRATIONS[version:], start=version + 1):
        # executescript commits first; the version bump is part of the same script.
        conn.executescript(f"BEGIN;\n{script}\nPRAGMA user_version = {number};\nCOMMIT;")


def iso(value: datetime | None) -> str | None:
    """Sortable UTC ISO-8601 text; naive timestamps are treated as UTC."""
    if value is None:
        return None
    value = value.astimezone(UTC) if value.tzinfo else value.replace(tzinfo=UTC)
    return value.isoformat(timespec="milliseconds")


def get_kv(conn: sqlite3.Connection, key: str) -> str | None:
    row = conn.execute("SELECT value FROM kv WHERE key = ?", (key,)).fetchone()
    return row[0] if row else None


def set_kv(conn: sqlite3.Connection, key: str, value: str) -> None:
    with conn:
        conn.execute("INSERT OR REPLACE INTO kv(key, value) VALUES (?, ?)", (key, value))
