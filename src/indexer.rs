//! Incremental indexing of agent log files into SQLite.
//!
//! Per file we remember size, mtime, inode, the first bytes and the byte offset of the last
//! complete line. Unchanged files are skipped by `stat` alone; appended files are parsed from
//! the stored offset; truncated or rewritten files are re-parsed from scratch.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::config::{expand, Config};
use crate::git::locate;
use crate::model::{first_line_title, iso, Agent, ParseResult, State};
use crate::parsers;

const HEAD_BYTES: u64 = 256;
const BATCH_FILES: usize = 200;

#[derive(Debug, Default)]
pub struct IndexStats {
    pub scanned: u64,
    pub parsed: u64,
    pub messages: u64,
    pub skipped_lines: u64,
    pub removed_sessions: u64,
}

pub fn roots_for(config: &Config, agent: Agent) -> Vec<PathBuf> {
    let p = &config.paths;
    let roots = match agent {
        Agent::Claude => &p.claude,
        Agent::Codex => &p.codex,
        Agent::Opencode => &p.opencode,
        Agent::Copilot => &p.copilot,
        Agent::Pi => &p.pi,
        Agent::Hermes => &p.hermes,
    };
    roots.iter().map(|r| expand(r)).collect()
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `BEGIN IMMEDIATE` waits (busy_timeout) for a concurrent writer instead of failing on lock upgrade.
fn commit(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("COMMIT; BEGIN IMMEDIATE")
}

pub fn index_all(conn: &Connection, config: &Config) -> rusqlite::Result<IndexStats> {
    let mut stats = IndexStats::default();
    let mut seen = HashSet::new();
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for agent in Agent::ALL {
        let roots = roots_for(config, agent);
        let mut pending = 0;
        for path in parsers::discover(agent, &roots) {
            seen.insert(path_str(&path));
            stats.scanned += 1;
            let changed = if parsers::is_store(agent) {
                index_store(conn, agent, &path, &mut stats)?
            } else {
                index_file(conn, agent, &path, &mut stats)?
            };
            if changed {
                pending += 1;
                if pending >= BATCH_FILES {
                    commit(conn)?;
                    pending = 0;
                }
            }
        }
        apply_agent_titles(conn, agent, parsers::titles(agent, &roots))?;
        commit(conn)?;
    }
    stats.removed_sessions = forget_missing(conn, &seen)?;
    conn.execute_batch("COMMIT")?;
    Ok(stats)
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::new();
    File::open(path)?.take(HEAD_BYTES).read_to_end(&mut head)?;
    Ok(head)
}

fn load_state(raw: Option<String>) -> State {
    match raw.and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
        Some(Value::Object(map)) => map,
        _ => State::new(),
    }
}

struct FileRow {
    session_key: Option<String>,
    size: i64,
    mtime_ns: i64,
    inode: i64,
    offset: i64,
    head: Option<Vec<u8>>,
    state: Option<String>,
}

/// Index new content of one file. Returns true if anything was parsed. Caller commits.
fn index_file(conn: &Connection, agent: Agent, path: &Path, stats: &mut IndexStats) -> rusqlite::Result<bool> {
    let (Ok(meta), Ok(head)) = (path.metadata(), read_head(path)) else { return Ok(false) };
    let (size, inode) = (meta.len() as i64, meta.ino() as i64);
    let mtime_ns = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
    let p = path_str(path);
    let row = conn
        .query_row(
            "SELECT session_key, size, mtime_ns, inode, offset, head, state FROM files WHERE path = ?",
            [&p],
            |r| {
                Ok(FileRow {
                    session_key: r.get(0)?,
                    size: r.get(1)?,
                    mtime_ns: r.get(2)?,
                    inode: r.get(3)?,
                    offset: r.get(4)?,
                    head: r.get(5)?,
                    state: r.get(6)?,
                })
            },
        )
        .optional()?;
    let (mut offset, mut state, mut key) = (0, State::new(), None);
    if let Some(row) = row {
        if row.size == size && row.mtime_ns == mtime_ns && row.inode == inode {
            return Ok(false);
        }
        key = row.session_key;
        let old_head = row.head.unwrap_or_default();
        let n = old_head.len().min(head.len());
        let rewritten = size < row.offset || row.inode != inode || head[..n] != old_head[..n];
        if !rewritten {
            (offset, state) = (row.offset as u64, load_state(row.state));
        } else if let Some(key) = &key {
            reset_session(conn, key)?;
        }
    }

    // The file vanished or became unreadable since `stat`: try again next run.
    let Ok(result) = parsers::parse(agent, path, offset, state) else { return Ok(false) };
    stats.parsed += 1;
    stats.skipped_lines += result.skipped_lines;
    let key = session_key(agent, &result, key, path);
    stats.messages += store(conn, agent, &key, path, &result)?;
    conn.execute(
        "INSERT OR REPLACE INTO files(path, agent, session_key, size, mtime_ns, inode, offset, head, state)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            p,
            agent.as_str(),
            key,
            size,
            mtime_ns,
            inode,
            result.offset as i64,
            head,
            Value::Object(result.state).to_string()
        ],
    )?;
    Ok(true)
}

/// Index a database holding many sessions (OpenCode, Hermes). Its WAL makes `stat` useless for
/// change detection, so the parser is always asked, with the watermark it returned last time.
fn index_store(conn: &Connection, agent: Agent, path: &Path, stats: &mut IndexStats) -> rusqlite::Result<bool> {
    let p = path_str(path);
    let state: Option<String> =
        conn.query_row("SELECT state FROM files WHERE path = ?", [&p], |r| r.get(0)).optional()?;
    let (results, state) = parsers::parse_store(agent, path, load_state(state));
    for result in &results {
        let key = session_key(agent, result, None, path);
        stats.messages += store(conn, agent, &key, path, result)?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO files(path, agent, state) VALUES (?, ?, ?)",
        params![p, agent.as_str(), Value::Object(state).to_string()],
    )?;
    stats.parsed += u64::from(!results.is_empty());
    Ok(!results.is_empty())
}

fn session_key(agent: Agent, result: &ParseResult, previous: Option<String>, path: &Path) -> String {
    match &result.session.session_id {
        Some(id) => format!("{}:{id}", agent.as_str()),
        None => previous.unwrap_or_else(|| {
            format!("{}:{}", agent.as_str(), path.file_stem().unwrap_or_default().to_string_lossy())
        }),
    }
}

/// A file of this session was rewritten: drop its messages and re-read all its files.
fn reset_session(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM messages WHERE session_key = ?", [key])?;
    conn.execute("UPDATE files SET offset = 0, size = -1, state = '{}', head = NULL WHERE session_key = ?", [key])?;
    Ok(())
}

fn store(conn: &Connection, agent: Agent, key: &str, path: &Path, result: &ParseResult) -> rusqlite::Result<u64> {
    let s = &result.session;
    let p = path_str(path);
    let exists = conn.query_row("SELECT 1 FROM sessions WHERE key = ?", [key], |_| Ok(())).optional()?.is_some();
    if !exists {
        let session_id = key.split_once(':').map_or(key, |(_, id)| id);
        conn.execute(
            "INSERT INTO sessions(key, agent, session_id, path) VALUES (?, ?, ?, ?)",
            params![key, agent.as_str(), session_id, p],
        )?;
    }
    if let Some(cwd) = s.cwd.as_deref().filter(|c| !c.is_empty()) {
        let loc = locate(cwd);
        conn.execute(
            "UPDATE sessions SET cwd = ?, worktree = ?, repo = ? WHERE key = ?",
            params![cwd, loc.worktree, loc.repo, key],
        )?;
    }
    let created = iso(s.created_at.or_else(|| result.messages.iter().find_map(|m| m.timestamp)));
    let updated = iso(s.updated_at);
    conn.execute(
        "UPDATE sessions SET
           path = ?1,
           branch = COALESCE(?2, branch),
           agent_title = COALESCE(?3, agent_title),
           created_at = CASE WHEN created_at IS NULL OR ?4 < created_at THEN ?4 ELSE created_at END,
           updated_at = CASE WHEN updated_at IS NULL OR ?5 > updated_at THEN ?5 ELSE updated_at END
         WHERE key = ?6",
        params![p, s.branch, s.title, created, updated, key],
    )?;
    let mut inserted = 0;
    let mut insert = conn
        .prepare_cached("INSERT OR IGNORE INTO messages(session_key, role, ts, uid, text) VALUES (?, ?, ?, ?, ?)")?;
    for m in &result.messages {
        inserted += insert.execute(params![key, m.role.as_str(), iso(m.timestamp), m.uid, m.text])? as u64;
    }
    if inserted > 0 || !exists || s.title.as_deref().is_some_and(|t| !t.is_empty()) {
        refresh_derived(conn, key)?;
    }
    Ok(inserted)
}

fn refresh_derived(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached("SELECT role, COUNT(*) FROM messages WHERE session_key = ? GROUP BY role")?;
    let counts: HashMap<String, i64> =
        stmt.query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let first: Option<String> = conn
        .query_row(
            "SELECT text FROM messages WHERE session_key = ? AND role = 'user' ORDER BY ts, id LIMIT 1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute(
        "UPDATE sessions SET user_count = ?, assistant_count = ?, title = COALESCE(agent_title, ?, title) WHERE key = ?",
        params![
            counts.get("user").copied().unwrap_or(0),
            counts.get("assistant").copied().unwrap_or(0),
            first.map(|t| first_line_title(&t)),
            key
        ],
    )?;
    Ok(())
}

fn apply_agent_titles(conn: &Connection, agent: Agent, titles: BTreeMap<String, String>) -> rusqlite::Result<()> {
    for (session_id, title) in titles {
        conn.execute(
            "UPDATE sessions SET agent_title = ?1, title = ?1 WHERE key = ?2 AND agent_title IS NOT ?1",
            params![title, format!("{}:{session_id}", agent.as_str())],
        )?;
    }
    Ok(())
}

/// Drop index entries whose source files disappeared (original history was deleted).
fn forget_missing(conn: &Connection, seen: &HashSet<String>) -> rusqlite::Result<u64> {
    let mut stmt = conn.prepare("SELECT path FROM files")?;
    let paths: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let gone: Vec<String> = paths.into_iter().filter(|p| !seen.contains(p) && !Path::new(p).exists()).collect();
    if gone.is_empty() {
        return Ok(0);
    }
    for p in &gone {
        conn.execute("DELETE FROM files WHERE path = ?", [p])?;
    }
    // Sessions of a multi-session database are tied to it by path, not by files.session_key.
    let mut stmt = conn.prepare(
        "SELECT key FROM sessions WHERE key NOT IN (SELECT session_key FROM files WHERE session_key IS NOT NULL)
           AND path NOT IN (SELECT path FROM files)",
    )?;
    let orphans: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for key in &orphans {
        conn.execute("DELETE FROM messages WHERE session_key = ?", [key])?;
        conn.execute("DELETE FROM sessions WHERE key = ?", [key])?;
    }
    Ok(orphans.len() as u64)
}

#[cfg(test)]
mod tests {
    //! OpenCode, GitHub Copilot CLI and pi: only human prompts and visible answers reach the index.

    use serde_json::json;

    use super::*;
    use crate::testutil::*;

    fn opencode_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, title TEXT,
                                  time_created INTEGER, time_updated INTEGER);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER,
                                  data TEXT);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER,
                               time_updated INTEGER, data TEXT);",
        )
        .unwrap();
        conn
    }

    fn add_message(conn: &Connection, id: &str, session: &str, created: i64, updated: i64, data: Value) {
        conn.execute(
            "INSERT OR REPLACE INTO message VALUES (?, ?, ?, ?, ?)",
            params![id, session, created, updated, data.to_string()],
        )
        .unwrap();
    }

    fn add_part(conn: &Connection, id: &str, message: &str, session: &str, created: i64, updated: i64, data: Value) {
        conn.execute(
            "INSERT OR REPLACE INTO part VALUES (?, ?, ?, ?, ?, ?)",
            params![id, message, session, created, updated, data.to_string()],
        )
        .unwrap();
    }

    fn seed_opencode(conn: &Connection, t: i64) {
        conn.execute(
            "INSERT INTO session (id, parent_id, directory, title, time_created, time_updated) VALUES
               ('ses_1', NULL, '/work/app', 'Fix login', ?1, ?1),
               ('ses_child', 'ses_1', '/work/app', 'Child session - x', ?1, ?1)",
            [t],
        )
        .unwrap();
        add_message(conn, "msg_1", "ses_1", t, t, json!({"role": "user", "time": {"created": t}}));
        add_part(conn, "prt_1", "msg_1", "ses_1", t, t, json!({"type": "text", "text": "fix the login bug"}));
        let synthetic = json!({"type": "text", "text": "Called the Read tool SYSTEMSECRET", "synthetic": true});
        add_part(conn, "prt_2", "msg_1", "ses_1", t, t, synthetic);
        add_part(conn, "prt_3", "msg_1", "ses_1", t, t, json!({"type": "file", "filename": "TOOLSECRET"}));
        let done = json!({"role": "assistant", "time": {"created": t + 1, "completed": t + 2}});
        add_message(conn, "msg_2", "ses_1", t + 1, t + 2, done);
        for (i, part) in [
            json!({"type": "reasoning", "text": "REASONINGSECRET"}),
            json!({"type": "tool", "tool": "bash", "state": {"input": "TOOLSECRET", "output": "TOOLSECRET"}}),
            json!({"type": "text", "text": "The token was never refreshed; fixed."}),
        ]
        .into_iter()
        .enumerate()
        {
            add_part(conn, &format!("prt_a{i}"), "msg_2", "ses_1", t + 1, t + 1, part);
        }
        add_message(conn, "msg_c", "ses_child", t, t, json!({"role": "user", "time": {"created": t}}));
        add_part(conn, "prt_c", "msg_c", "ses_child", t, t, json!({"type": "text", "text": "SIDECHAINSECRET"}));
    }

    fn conversation(db: &Connection, key: &str) -> Vec<(String, String)> {
        let mut stmt = db.prepare("SELECT role, text FROM messages WHERE session_key = ? ORDER BY ts, id").unwrap();
        stmt.query_map([key], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn column(db: &Connection, sql: &str) -> Vec<Option<String>> {
        let mut stmt = db.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn new_agents_index_only_human_conversation() {
        let mut e = env();
        e.config.paths.copilot = vec![lossy(&e.logs.join("copilot/session-state"))];
        e.config.paths.pi = vec![lossy(&e.logs.join("pi/sessions"))];
        e.config.paths.opencode = vec![lossy(&e.root.join("opencode"))];
        std::fs::create_dir(e.root.join("opencode")).unwrap();
        let oc = opencode_db(&e.root.join("opencode/opencode.db"));
        let now = chrono::Utc::now().timestamp_millis();
        seed_opencode(&oc, now - 60_000);
        index_all(&e.db, &e.config).unwrap();

        let mut blob = column(&e.db, "SELECT text FROM messages");
        blob.extend(column(&e.db, "SELECT title FROM sessions"));
        let blob: String = blob.into_iter().flatten().collect::<Vec<_>>().join("\n");
        for marker in FORBIDDEN {
            assert!(!blob.contains(marker), "{marker}");
        }

        let copilot = "copilot:44444444-dddd-4eee-8fff-000000000004";
        let pi = "pi:55555555-eeee-4fff-8aaa-000000000005";
        assert_eq!(
            conversation(&e.db, copilot),
            pairs(&[("user", "Where do we store the SAP version?"), ("assistant", "It is stored in the `erp_config` table.")])
        );
        assert_eq!(
            conversation(&e.db, pi),
            pairs(&[
                ("user", "review the login flow"),
                ("assistant", "Reading the auth module first."),
                ("assistant", "The session token is never refreshed."),
                ("user", "fix it"),
            ])
        );
        assert_eq!(
            conversation(&e.db, "opencode:ses_1"),
            pairs(&[("user", "fix the login bug"), ("assistant", "The token was never refreshed; fixed.")])
        );
        let title = |key: &str| -> Option<String> {
            e.db.query_row("SELECT title FROM sessions WHERE key = ?", [key], |r| r.get(0)).optional().unwrap().flatten()
        };
        assert_eq!(title("opencode:ses_child"), None); // subagent prompts are not human
        assert_eq!(title(copilot).as_deref(), Some("Find SAP Version Storage"));
        assert_eq!(title(pi).as_deref(), Some("Login flow review"));
        assert_eq!(title("opencode:ses_1").as_deref(), Some("Fix login"));
        let branch: Option<String> =
            e.db.query_row("SELECT branch FROM sessions WHERE key = ?", [copilot], |r| r.get(0)).unwrap();
        assert_eq!(branch.as_deref(), Some("main"));
        assert!(column(&e.db, "SELECT cwd FROM sessions").contains(&Some("/work/app".into())));

        // OpenCode incremental: an answer still streaming is held back until it completes
        let t = now;
        add_message(&oc, "msg_3", "ses_1", t, t, json!({"role": "user", "time": {"created": t}}));
        add_part(&oc, "prt_5", "msg_3", "ses_1", t, t, json!({"type": "text", "text": "and add a test"}));
        add_message(&oc, "msg_4", "ses_1", t + 1, t + 1, json!({"role": "assistant", "time": {"created": t + 1}}));
        add_part(&oc, "prt_6", "msg_4", "ses_1", t + 1, t + 1, json!({"type": "text", "text": "Adding"}));
        index_all(&e.db, &e.config).unwrap();
        assert_eq!(conversation(&e.db, "opencode:ses_1").last().unwrap().1, "and add a test");
        add_part(&oc, "prt_6", "msg_4", "ses_1", t + 1, t + 3, json!({"type": "text", "text": "Added a regression test."}));
        let done = json!({"role": "assistant", "time": {"created": t + 1, "completed": t + 3}});
        add_message(&oc, "msg_4", "ses_1", t + 1, t + 3, done);
        index_all(&e.db, &e.config).unwrap();
        let conv = conversation(&e.db, "opencode:ses_1");
        assert_eq!(conv.last().unwrap(), &("assistant".to_string(), "Added a regression test.".to_string()));
        assert_eq!(conv.len(), 4);

        // the database stays indexed across runs (its sessions are not mistaken for orphans)
        std::fs::remove_file(e.logs.join("copilot/session-state/44444444-dddd-4eee-8fff-000000000004/events.jsonl"))
            .unwrap();
        index_all(&e.db, &e.config).unwrap();
        let count = |agent: &str| -> i64 {
            e.db.query_row("SELECT COUNT(*) FROM sessions WHERE agent = ?", [agent], |r| r.get(0)).unwrap()
        };
        assert_eq!((count("opencode"), count("copilot")), (1, 0));
    }

    #[test]
    fn hermes_databases_index_only_the_human_conversation() {
        use crate::parsers::hermes::tests::{add, hermes_db, seed, SESSION};
        let mut e = env();
        let home = e.root.join("hermes");
        std::fs::create_dir_all(home.join("profiles/coder")).unwrap();
        e.config.paths.hermes = vec![lossy(&home)];
        let main = hermes_db(&home.join("state.db"));
        seed(&main, 1_762_337_700.0);
        let coder = hermes_db(&home.join("profiles/coder/state.db"));
        crate::parsers::hermes::tests::add_session(&coder, "20261106_090000_bbbbbb", "cli", None, 1_762_419_600.0, None);
        add(&coder, "20261106_090000_bbbbbb", 1_762_419_601.0, "p1", "user", "profile prompt", "");
        index_all(&e.db, &e.config).unwrap();

        let blob = column(&e.db, "SELECT text FROM messages").into_iter().flatten().collect::<Vec<_>>().join("\n");
        for marker in FORBIDDEN {
            assert!(!blob.contains(marker), "{marker}");
        }
        let key = format!("hermes:{SESSION}");
        assert_eq!(
            conversation(&e.db, &key),
            pairs(&[
                ("user", "fix the login bug"),
                ("assistant", "Reading the auth module."),
                ("assistant", "The token was never refreshed; fixed."),
                ("user", "and add a test"),
            ])
        );
        let mut keys = column(&e.db, "SELECT key FROM sessions WHERE agent = 'hermes' ORDER BY key");
        keys.sort();
        assert_eq!(keys.len(), 3); // main, its compression continuation and the profile's session
        let path: String =
            e.db.query_row("SELECT path FROM sessions WHERE key = 'hermes:20261106_090000_bbbbbb'", [], |r| r.get(0)).unwrap();
        assert!(path.ends_with("profiles/coder/state.db"));

        // incremental: only the new answer is added, and re-running adds nothing
        add(&main, SESSION, 1_762_337_730.0, "u12", "assistant", "Added a regression test.", "");
        assert_eq!(index_all(&e.db, &e.config).unwrap().messages, 1);
        assert_eq!(index_all(&e.db, &e.config).unwrap().parsed, 0);
        let found = crate::search::search(
            &e.db,
            "regression",
            crate::config::Scope::Assistant,
            &crate::search::Filters { agent: Some("hermes".into()), ..Default::default() },
            crate::config::Sort::LastActivityDesc,
            10,
        )
        .unwrap();
        assert_eq!(found[0].key, key);
    }
}
