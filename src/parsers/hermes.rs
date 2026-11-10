//! Hermes Agent (Nous Research): every session lives in one SQLite database,
//! `$HERMES_HOME/state.db` (default `~/.hermes`), plus one per profile in `profiles/<name>/state.db`.
//!
//! Tables `sessions` (`source`, `title`, `cwd`, `git_branch`, `parent_session_id`, `started_at`,
//! …) and `messages` (`role`, `content`, `timestamp`, …); times are Unix seconds (float). Only
//! `user` text and the `content` of `assistant` messages are kept. Tool calls (`tool_calls`), tool
//! results (`tool` rows), reasoning columns and system messages are ignored, and so are compression
//! summaries (`_compressed_summary`), display-only carriers (`display_kind`, e.g. `hidden`) and
//! the background-review harness prompts with the reply that followed them. Non-interactive
//! sessions (subagents, cron, kanban, tool, batch, one-shot) are skipped. Compression splits
//! (`parent_session_id`) are indexed as their own sessions, titled `name #2` by Hermes.
//!
//! Like OpenCode there is no byte offset: `parse_store` returns the sessions changed since the
//! `state` watermark, read whole (messages are de-duplicated by `message_uid` when stored).
//! Columns are read by name, so older schema versions without e.g. `cwd` still work.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::DateTime;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, Row};
use serde_json::Value;

use crate::model::{sanitize, text_blocks, ParseResult, Role, SessionPatch, State, Ts};
use crate::parsers::{grandchildren, name_of};

/// Sessions no human typed into.
const SKIPPED_SOURCES: [&str; 7] = ["subagent", "cron", "kanban", "tool", "batch", "oneshot", "recovered"];
const REVIEW_HARNESS_PREFIXES: [&str; 2] = [
    "Review the conversation above and update the skill library",
    "Review the conversation above and consider saving to memory",
];
/// Structured (JSON) message content is stored behind this prefix.
const JSON_PREFIX: &str = "\x00json:";

pub fn discover(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    let mut out: Vec<PathBuf> = Some(root.join("state.db")).filter(|p| p.is_file()).into_iter().collect();
    out.extend(grandchildren(&root.join("profiles"), |name| name == "state.db"));
    out
}

/// The profile a database belongs to (`…/profiles/<name>/state.db`); `None` for the default one.
pub fn profile(db: &Path) -> Option<String> {
    let dir = db.parent()?;
    (name_of(dir.parent()?) == "profiles").then(|| name_of(dir).to_string())
}

pub fn parse_store(path: &Path, state: State) -> (Vec<ParseResult>, State) {
    let since = state.get("since").and_then(Value::as_f64).unwrap_or(0.0);
    match read_changed(path, since) {
        Ok((results, newest)) => {
            let mut state = State::new();
            state.insert("since".into(), newest.unwrap_or(since).max(since).into());
            (results, state)
        }
        Err(_) => (vec![], state), // locked, or a schema this version does not know: retry next run
    }
}

fn read_changed(path: &Path, since: f64) -> rusqlite::Result<(Vec<ParseResult>, Option<f64>)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    // ponytail: sessions deleted inside Hermes stay in the index until `human-history index --rebuild`.
    let mut stmt = conn.prepare(
        "SELECT * FROM sessions WHERE started_at > ?1 OR ended_at > ?1
           OR id IN (SELECT session_id FROM messages WHERE timestamp > ?1)",
    )?;
    let mut rows = stmt.query([since])?;
    let mut results = vec![];
    while let Some(row) = rows.next()? {
        if let Some(result) = session(&conn, row)? {
            results.push(result);
        }
    }
    let newest = conn.query_row(
        "SELECT MAX(t) FROM (SELECT MAX(started_at) t FROM sessions UNION ALL
         SELECT MAX(ended_at) FROM sessions UNION ALL SELECT MAX(timestamp) FROM messages)",
        [],
        |r| r.get::<_, Option<f64>>(0),
    )?;
    Ok((results, newest))
}

/// A column by name; `Null` when this schema version does not have it.
fn col(row: &Row, name: &str) -> Sql {
    row.get::<_, Sql>(name).unwrap_or(Sql::Null)
}

fn text(v: Sql) -> Option<String> {
    match v {
        Sql::Text(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn seconds(v: &Sql) -> Option<Ts> {
    let secs = match *v {
        Sql::Real(f) => f,
        Sql::Integer(i) => i as f64,
        _ => return None,
    };
    DateTime::from_timestamp_micros((secs * 1e6).round() as i64)
}

fn truthy(v: &Sql) -> bool {
    match v {
        Sql::Null => false,
        Sql::Integer(i) => *i != 0,
        Sql::Real(f) => *f != 0.0,
        Sql::Text(s) => !s.is_empty(),
        Sql::Blob(b) => !b.is_empty(),
    }
}

/// Message text: plain, or the text parts of JSON content stored behind `\x00json:`.
fn content(raw: Option<String>) -> String {
    let Some(raw) = raw else { return String::new() };
    let Some(json) = raw.strip_prefix(JSON_PREFIX) else { return raw };
    match serde_json::from_str::<Value>(json) {
        Ok(Value::String(s)) => s,
        Ok(parts @ Value::Array(_)) => text_blocks(Some(&parts), "text").join("\n\n"),
        _ => String::new(),
    }
}

fn session(conn: &Connection, row: &Row) -> rusqlite::Result<Option<ParseResult>> {
    let id = text(col(row, "id")).unwrap_or_default();
    let source = text(col(row, "source")).unwrap_or_default();
    if id.is_empty() || SKIPPED_SOURCES.contains(&source.as_str()) || truthy(&col(row, "hidden")) {
        return Ok(None);
    }
    let started = seconds(&col(row, "started_at"));
    let mut result = ParseResult {
        session: SessionPatch {
            session_id: Some(id.clone()),
            cwd: text(col(row, "cwd")),
            branch: text(col(row, "git_branch")).filter(|b| b != "HEAD"),
            title: text(col(row, "title")).map(|t| sanitize(t.trim())).filter(|t| !t.is_empty()),
            created_at: started,
            updated_at: seconds(&col(row, "ended_at")).or(started),
        },
        ..Default::default()
    };

    let mut stmt = conn.prepare("SELECT * FROM messages WHERE session_id = ? ORDER BY timestamp, id")?;
    let mut rows = stmt.query([&id])?;
    let mut seen = HashSet::new();
    let mut after_harness = false;
    while let Some(m) = rows.next()? {
        let role = text(col(m, "role")).unwrap_or_default();
        let raw = content(text(col(m, "content")));
        let ts = seconds(&col(m, "timestamp"));
        let harness = REVIEW_HARNESS_PREFIXES.iter().any(|p| raw.trim_start().starts_with(p));
        if matches!(role.as_str(), "user" | "system") && harness {
            after_harness = true; // the curator's reply that follows is not part of the conversation
            continue;
        }
        let role = match role.as_str() {
            "user" => Role::User,
            "assistant" if std::mem::take(&mut after_harness) => continue,
            "assistant" => Role::Assistant,
            _ => continue,
        };
        if truthy(&col(m, "_compressed_summary")) || truthy(&col(m, "display_kind")) {
            continue;
        }
        let text = raw.trim();
        // A tool-call row may carry a bare `[tool_name]` marker instead of text.
        let marker = text.starts_with('[') && text.ends_with(']') && !text.contains(char::is_whitespace);
        if text.is_empty() || (role == Role::Assistant && marker && truthy(&col(m, "tool_calls"))) {
            continue;
        }
        let text = sanitize(text);
        // In-place compaction can keep a protected message in both generations.
        if !seen.insert((role == Role::User, text.clone(), ts)) {
            continue;
        }
        let uid = text_or_id(col(m, "message_uid"), col(m, "id"));
        if let Some(t) = ts {
            result.session.updated_at = result.session.updated_at.max(Some(t));
        }
        result.push(role, text, ts, uid);
    }
    Ok(Some(result))
}

fn text_or_id(uid: Sql, id: Sql) -> String {
    match (uid, id) {
        (Sql::Text(u), _) if !u.is_empty() => u,
        (_, Sql::Integer(i)) => format!("m{i}"),
        _ => String::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const SESSION: &str = "20261105_101500_a1b2c3";

    /// The columns this parser reads, as in Hermes' schema (a subset of it).
    pub fn hermes_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, parent_session_id TEXT,
                started_at REAL NOT NULL, ended_at REAL, cwd TEXT, git_branch TEXT, title TEXT,
                hidden INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
                timestamp REAL NOT NULL, reasoning TEXT, _compressed_summary INTEGER NOT NULL DEFAULT 0,
                active INTEGER NOT NULL DEFAULT 1, display_kind TEXT, message_uid TEXT);",
        )
        .unwrap();
        conn
    }

    pub fn add_session(conn: &Connection, id: &str, source: &str, parent: Option<&str>, t: f64, title: Option<&str>) {
        conn.execute(
            "INSERT INTO sessions (id, source, parent_session_id, started_at, cwd, git_branch, title)
             VALUES (?, ?, ?, ?, '/work/app', 'main', ?)",
            rusqlite::params![id, source, parent, t, title],
        )
        .unwrap();
    }

    /// (role, content, tool_calls, flags: "summary" | "hidden" | "archived")
    pub fn add(conn: &Connection, session: &str, t: f64, uid: &str, role: &str, content: &str, extra: &str) {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, timestamp, reasoning,
                _compressed_summary, active, display_kind, message_uid)
             VALUES (?1, ?2, ?3, ?4, ?5, 'REASONINGSECRET', ?6, ?7, ?8, ?9)",
            rusqlite::params![
                session,
                role,
                content,
                (extra == "tool_call").then_some(r#"[{"id":"call_1","function":{"name":"terminal","arguments":"TOOLSECRET"}}]"#),
                t,
                extra == "summary",
                extra != "archived",
                (extra == "hidden").then_some("hidden"),
                uid,
            ],
        )
        .unwrap();
    }

    pub fn seed(conn: &Connection, t: f64) {
        add_session(conn, SESSION, "cli", None, t, Some("Fix login"));
        add(conn, SESSION, t + 1.0, "u1", "system", "You are Hermes SYSTEMSECRET", "");
        add(conn, SESSION, t + 2.0, "u2", "user", "fix the login bug", "");
        add(conn, SESSION, t + 3.0, "u3", "assistant", "Reading the auth module.", "tool_call");
        add(conn, SESSION, t + 4.0, "u4", "assistant", "[terminal]", "tool_call");
        add(conn, SESSION, t + 5.0, "u5", "tool", "TOOLSECRET output", "");
        let parts = format!("{JSON_PREFIX}[{{\"type\":\"text\",\"text\":\"The token was never refreshed; fixed.\"}}]");
        add(conn, SESSION, t + 6.0, "u6", "assistant", &parts, "");
        add(conn, SESSION, t + 7.0, "u7", "user", "Review the conversation above and update the skill library HOOKSECRET", "");
        add(conn, SESSION, t + 8.0, "u8", "assistant", "Updated skills HOOKSECRET", "");
        add(conn, SESSION, t + 9.0, "u9", "user", "goal control SYSTEMSECRET", "hidden");
        add(conn, SESSION, t + 10.0, "u10", "user", "Summary of earlier turns SYSTEMSECRET", "summary");
        add(conn, SESSION, t + 11.0, "u11", "user", "and add a test", "archived");
        add(conn, SESSION, t + 11.0, "u11b", "user", "and add a test", ""); // protected copy, new generation
        add_session(conn, "20261105_101600_d4e5f6", "subagent", Some(SESSION), t, None);
        add(conn, "20261105_101600_d4e5f6", t + 2.0, "s1", "user", "SIDECHAINSECRET", "");
        add_session(conn, "20261105_101700_aaaaaa", "telegram", Some(SESSION), t + 20.0, Some("Fix login #2"));
        add(conn, "20261105_101700_aaaaaa", t + 21.0, "c1", "user", "continue from my phone", "");
    }

    fn pairs(r: &ParseResult) -> Vec<(&str, &str)> {
        r.messages.iter().map(|m| (m.role.as_str(), m.text.as_str())).collect()
    }

    #[test]
    fn keeps_only_the_human_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let conn = hermes_db(&db);
        seed(&conn, 1_762_337_700.0);
        let (results, state) = parse_store(&db, State::new());
        let ids: Vec<&str> = results.iter().map(|r| r.session.session_id.as_deref().unwrap()).collect();
        assert_eq!(ids, [SESSION, "20261105_101700_aaaaaa"]); // the subagent session is skipped
        let main = &results[0];
        assert_eq!(
            pairs(main),
            [
                ("user", "fix the login bug"),
                ("assistant", "Reading the auth module."),
                ("assistant", "The token was never refreshed; fixed."),
                ("user", "and add a test"),
            ]
        );
        let s = &main.session;
        assert_eq!((s.cwd.as_deref(), s.branch.as_deref(), s.title.as_deref()), (Some("/work/app"), Some("main"), Some("Fix login")));
        assert_eq!(crate::model::iso(s.created_at).unwrap(), "2025-11-05T10:15:00.000+00:00");
        assert_eq!(main.messages[0].uid, "u2");
        assert_eq!(state["since"].as_f64(), Some(1_762_337_721.0));

        // nothing new: nothing returned; a new message brings back only its session
        assert!(parse_store(&db, state.clone()).0.is_empty());
        add(&conn, SESSION, 1_762_337_730.0, "u12", "assistant", "Added a regression test.", "");
        let (results, _) = parse_store(&db, state);
        assert_eq!(results.len(), 1);
        assert_eq!(pairs(&results[0]).last(), Some(&("assistant", "Added a regression test.")));
    }

    #[test]
    fn finds_default_and_profile_databases() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("profiles/coder")).unwrap();
        std::fs::write(home.join("state.db"), "").unwrap();
        std::fs::write(home.join("profiles/coder/state.db"), "").unwrap();
        let found = discover(home);
        assert_eq!(found, [home.join("state.db"), home.join("profiles/coder/state.db")]);
        assert_eq!(profile(&found[0]), None);
        assert_eq!(profile(&found[1]).as_deref(), Some("coder"));
        assert_eq!(discover(&found[0]), [found[0].clone()]);
    }
}
