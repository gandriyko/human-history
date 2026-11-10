//! OpenCode (1.2+): every session lives in one SQLite database, `~/.local/share/opencode/opencode.db`.
//!
//! Tables `session` (`directory`, `title`, `parent_id`), `message` and `part` (JSON in `data`).
//! Only `text` parts are kept: in `user` messages unless flagged `synthetic` or `ignored`, in
//! `assistant` messages unless the message is a compaction summary. Child (subagent) sessions
//! are skipped: their prompts were written by an agent.
//!
//! There is no byte offset: `parse_store` returns the sessions changed since the `state`
//! watermark, read whole (messages are de-duplicated by id when stored). Assistant text is taken
//! only from completed messages, since parts are rewritten while streaming.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, Row};
use serde_json::{Map, Value};

use crate::model::{get_obj, get_str, join_stripped, sanitize, truthy, ParseResult, Role, SessionPatch, State, Ts};

const DEFAULT_TITLE_PREFIXES: [&str; 2] = ["New session - ", "Child session - "];
const STALE_MS: i64 = 10 * 60 * 1000;

pub fn parse_store(path: &Path, state: State) -> (Vec<ParseResult>, State) {
    let since = state.get("since").and_then(Value::as_i64).unwrap_or(0);
    match read_changed(path, since) {
        Ok((results, newest)) => {
            // An unfinished answer is not stored yet: start from before it next time.
            let watermark = results
                .iter()
                .flat_map(|(_, pending)| pending)
                .copied()
                .fold(newest.filter(|&n| n != 0).unwrap_or(since), i64::min);
            let mut state = State::new();
            state.insert("since".into(), watermark.into());
            (results.into_iter().map(|(r, _)| r).collect(), state)
        }
        Err(_) => (vec![], state), // locked, or a schema this version does not know: retry next run
    }
}

type Changed = (Vec<(ParseResult, Vec<i64>)>, Option<i64>);

fn read_changed(path: &Path, since: i64) -> rusqlite::Result<Changed> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    // ponytail: sessions deleted inside OpenCode stay in the index until `human-history index --rebuild`.
    let mut stmt = conn.prepare(
        "SELECT id, directory, title, time_created, time_updated FROM session
         WHERE parent_id IS NULL AND (time_updated > ?1
           OR id IN (SELECT session_id FROM message WHERE time_updated > ?1)
           OR id IN (SELECT session_id FROM part WHERE time_updated > ?1))",
    )?;
    let sessions: Vec<[Sql; 5]> = stmt
        .query_map([since], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))?
        .collect::<Result<_, _>>()?;
    let results = sessions.iter().map(|row| session(&conn, row)).collect::<rusqlite::Result<Vec<_>>>()?;
    let newest = conn.query_row(
        "SELECT MAX(t) FROM (SELECT MAX(time_updated) t FROM session UNION ALL
         SELECT MAX(time_updated) FROM message UNION ALL SELECT MAX(time_updated) FROM part)",
        [],
        |r| r.get::<_, Sql>(0),
    )?;
    Ok((results, as_ms(&newest)))
}

fn text(v: &Sql) -> Option<String> {
    match v {
        Sql::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_ms(v: &Sql) -> Option<i64> {
    match *v {
        Sql::Integer(i) => Some(i),
        Sql::Real(f) => Some(f as i64),
        _ => None,
    }
}

fn ms(v: &Sql) -> Option<Ts> {
    match *v {
        Sql::Integer(i) => DateTime::from_timestamp_millis(i),
        Sql::Real(f) => DateTime::from_timestamp_micros((f * 1000.0).round() as i64),
        _ => None,
    }
}

fn json(row: &Row, idx: usize) -> Map<String, Value> {
    match row.get::<_, Sql>(idx) {
        Ok(Sql::Text(raw)) => serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    }
}

/// One session read whole, plus the `time_updated - 1` of each answer still streaming.
fn session(conn: &Connection, row: &[Sql; 5]) -> rusqlite::Result<(ParseResult, Vec<i64>)> {
    let [id, directory, title, created, updated] = row;
    let id = text(id).unwrap_or_default();
    let title = text(title).unwrap_or_default();
    let mut result = ParseResult {
        session: SessionPatch {
            session_id: Some(id.clone()),
            cwd: text(directory).filter(|d| !d.is_empty()),
            title: (!title.is_empty() && !DEFAULT_TITLE_PREFIXES.iter().any(|p| title.starts_with(p)))
                .then(|| sanitize(&title)),
            created_at: ms(created),
            updated_at: ms(updated),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pending = vec![];

    let mut parts: HashMap<String, Vec<Map<String, Value>>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT message_id, data FROM part WHERE session_id = ? ORDER BY id")?;
    let mut rows = stmt.query([&id])?;
    while let Some(p) = rows.next()? {
        let message_id = text(&p.get(0)?).unwrap_or_default();
        parts.entry(message_id).or_default().push(json(p, 1));
    }

    let mut stmt = conn.prepare(
        "SELECT id, time_created, time_updated, data FROM message WHERE session_id = ? ORDER BY time_created, id",
    )?;
    let mut rows = stmt.query([&id])?;
    let now_ms = Utc::now().timestamp_millis();
    while let Some(m) = rows.next()? {
        let (mid, created, updated) = (text(&m.get(0)?).unwrap_or_default(), m.get::<_, Sql>(1)?, m.get::<_, Sql>(2)?);
        let data = json(m, 3);
        let role = match get_str(&data, "role") {
            Some("user") => Role::User,
            Some("assistant") => {
                let compaction = [get_str(&data, "mode"), get_str(&data, "agent")].contains(&Some("compaction"));
                if data.get("summary") == Some(&Value::Bool(true)) || compaction {
                    continue;
                }
                let completed = get_obj(&data, "time").is_some_and(|t| truthy(t.get("completed")));
                if !completed && !truthy(data.get("error")) {
                    // An abandoned answer never completes: only recent ones hold the watermark back.
                    if let Some(u) = as_ms(&updated).filter(|&u| u > now_ms - STALE_MS) {
                        pending.push(u - 1);
                    }
                    continue;
                }
                Role::Assistant
            }
            _ => continue,
        };
        let texts: Vec<String> = parts
            .get(&mid)
            .into_iter()
            .flatten()
            .filter(|p| get_str(p, "type") == Some("text") && !truthy(p.get("synthetic")) && !truthy(p.get("ignored")))
            .filter_map(|p| get_str(p, "text").map(str::to_string))
            .collect();
        let text = join_stripped(&texts);
        if !text.is_empty() {
            result.push(role, sanitize(&text), ms(&created), mid);
        }
    }
    Ok((result, pending))
}
