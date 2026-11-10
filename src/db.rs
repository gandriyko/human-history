//! SQLite schema (WAL + FTS5) with `user_version`-based migrations — the same database file the
//! Python implementation uses. Only human-readable conversation text is stored.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

const MIGRATIONS: [&str; 1] = [
    // v1
    r#"
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
    "#,
];

pub fn connect(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent); // open() reports a real problem
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, script) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        conn.execute_batch(&format!("BEGIN;\n{script}\nPRAGMA user_version = {};\nCOMMIT;", i + 1))?;
    }
    Ok(())
}
