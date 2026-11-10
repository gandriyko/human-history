//! Session listing, scoped FTS5 search and fuzzy title matching.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::types::Value as Sql;
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};

use crate::config::{absolute, expand, ProjectFilter, Scope, Sort};
use crate::git::locate;

/// Snippet markers; control chars never survive `sanitize()`.
pub const HL_START: char = '\x02';
pub const HL_END: char = '\x03';

fn order(sort: Sort) -> &'static str {
    match sort {
        Sort::LastActivityDesc => "s.updated_at DESC",
        Sort::CreatedDesc => "s.created_at DESC",
        Sort::TitleAsc => "s.title COLLATE NOCASE ASC",
    }
}

#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub project: ProjectFilter,
    /// Where the user is; resolves current worktree / repository.
    pub cwd: Option<String>,
    /// Sessions started in exactly this directory.
    pub exact_cwd: Option<String>,
    pub agent: Option<String>,
    pub branch: Option<String>,
    /// ISO date/time, compared with last activity.
    pub since: Option<String>,
    pub until: Option<String>,
    pub favorites_only: bool,
}

impl Filters {
    fn where_clause(&self) -> (String, Vec<Sql>) {
        let mut clauses: Vec<String> = vec![];
        let mut params: Vec<Sql> = vec![];
        if let (false, Some(cwd)) = (self.project == ProjectFilter::All, &self.cwd) {
            let loc = locate(cwd);
            match (self.project, loc.repo) {
                (ProjectFilter::Repository, Some(repo)) => {
                    clauses.push("s.repo = ?".into());
                    params.push(Sql::Text(repo));
                }
                _ => {
                    // Exact worktree. Sessions whose directory no longer exists match by path prefix.
                    let root = loc.worktree.unwrap_or_else(|| cwd.trim_end_matches('/').to_string());
                    clauses.push(
                        "(s.worktree = ? OR (s.worktree IS NULL AND (s.cwd = ? OR substr(s.cwd, 1, ?) = ?)))".into(),
                    );
                    params.push(Sql::Text(root.clone()));
                    params.push(Sql::Text(root.clone()));
                    params.push(Sql::Integer(root.chars().count() as i64 + 1));
                    params.push(Sql::Text(format!("{root}/")));
                }
            }
        }
        if let Some(cwd) = &self.exact_cwd {
            clauses.push("s.cwd = ?".into());
            params.push(Sql::Text(cwd.trim_end_matches('/').to_string()));
        }
        for (column, value) in [
            ("s.agent = ?", &self.agent),
            ("s.branch = ?", &self.branch),
            ("s.updated_at >= ?", &self.since),
            ("s.updated_at < ?", &self.until),
        ] {
            if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                clauses.push(column.into());
                params.push(Sql::Text(value.clone()));
            }
        }
        if self.favorites_only {
            clauses.push("f.session_key IS NOT NULL".into());
        }
        let clause = if clauses.is_empty() { "1".to_string() } else { clauses.join(" AND ") };
        (clause, params)
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the full row is for the TUI port; the CLI reads a subset
pub struct SessionRow {
    pub key: String,
    pub agent: String,
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub worktree: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub user_count: i64,
    pub assistant_count: i64,
    pub path: String,
    pub favorite: bool,
    // Set for search results: best matching message and its snippet.
    pub hit_message_id: Option<i64>,
    pub hit_role: Option<String>,
    pub snippet: Option<String>,
    pub hits: usize,
    pub match_ids: Vec<i64>,
}

const SESSION_COLUMNS: &str = "s.key, s.agent, s.session_id, COALESCE(s.title, '(untitled)') AS title, s.cwd,
    s.worktree, s.repo, s.branch, s.created_at, s.updated_at, s.user_count, s.assistant_count, s.path,
    f.session_key IS NOT NULL AS favorite";
const FROM: &str = "sessions s LEFT JOIN favorites f ON f.session_key = s.key";

fn session_row(r: &Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        key: r.get(0)?,
        agent: r.get(1)?,
        session_id: r.get(2)?,
        title: r.get(3)?,
        cwd: r.get(4)?,
        worktree: r.get(5)?,
        repo: r.get(6)?,
        branch: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        user_count: r.get(10)?,
        assistant_count: r.get(11)?,
        path: r.get(12)?,
        favorite: r.get(13)?,
        hit_message_id: None,
        hit_role: None,
        snippet: None,
        hits: 0,
        match_ids: vec![],
    })
}

pub fn list_sessions(conn: &Connection, filters: &Filters, sort: Sort, limit: usize) -> rusqlite::Result<Vec<SessionRow>> {
    let (clause, mut params) = filters.where_clause();
    params.push(Sql::Integer(limit as i64));
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM {FROM} WHERE {clause} AND (s.user_count + s.assistant_count) > 0
         ORDER BY favorite DESC, {} LIMIT ?",
        order(sort)
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params_from_iter(params), session_row)?.collect()
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]+)"|(\S+)"#).unwrap());

/// User text -> safe FTS5 query: AND of quoted terms; the last bare word is a prefix.
pub fn fts_query(text: &str) -> Option<String> {
    let parts: Vec<(String, bool)> = TOKEN
        .captures_iter(text)
        .filter_map(|c| {
            let (term, phrase) = match c.get(1) {
                Some(p) => (p.as_str(), true),
                None => (&c[2], false),
            };
            let term = term.replace('"', "");
            let term = term.trim();
            (!term.is_empty()).then(|| (term.to_string(), phrase))
        })
        .collect();
    let (_, last_is_phrase) = parts.last()?;
    let mut out: Vec<String> = parts.iter().map(|(t, _)| format!("\"{t}\"")).collect();
    if !last_is_phrase {
        out.last_mut().unwrap().push('*');
    }
    Some(out.join(" "))
}

/// Sessions matching `query` in the given scope, best hit first.
pub fn search(
    conn: &Connection,
    query: &str,
    scope: Scope,
    filters: &Filters,
    sort: Sort,
    limit: usize,
) -> rusqlite::Result<Vec<SessionRow>> {
    let roles: &[&str] = match scope {
        Scope::Title => return search_titles(conn, query, filters, sort, limit),
        Scope::User => &["user"],
        Scope::Assistant => &["assistant"],
        Scope::Both => &["user", "assistant"],
    };
    let Some(matcher) = fts_query(query) else { return list_sessions(conn, filters, sort, limit) };
    let (clause, filter_params) = filters.where_clause();
    let placeholders = vec!["?"; roles.len()].join(",");
    let sql = format!(
        "SELECT m.session_key, m.id, m.role, snippet(messages_fts, 0, '{HL_START}', '{HL_END}', '…', 16)
         FROM messages_fts
         JOIN messages m ON m.id = messages_fts.rowid
         JOIN sessions s ON s.key = m.session_key
         LEFT JOIN favorites f ON f.session_key = s.key
         WHERE messages_fts MATCH ? AND m.role IN ({placeholders}) AND {clause}
         ORDER BY bm25(messages_fts)
         LIMIT 5000"
    );
    let mut params = vec![Sql::Text(matcher)];
    params.extend(roles.iter().map(|r| Sql::Text(r.to_string())));
    params.extend(filter_params);

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;
    let mut keys: Vec<String> = vec![]; // best hit first
    let mut best: HashMap<String, (i64, String, Option<String>)> = HashMap::new();
    let mut ids: HashMap<String, Vec<i64>> = HashMap::new();
    while let Some(h) = rows.next()? {
        let (key, id): (String, i64) = (h.get(0)?, h.get(1)?);
        if !best.contains_key(&key) {
            best.insert(key.clone(), (id, h.get(2)?, h.get(3)?));
            keys.push(key.clone());
        }
        ids.entry(key).or_default().push(id);
    }
    keys.truncate(limit);
    if keys.is_empty() {
        return Ok(vec![]);
    }

    let sql = format!("SELECT {SESSION_COLUMNS} FROM {FROM} WHERE s.key IN ({})", vec!["?"; keys.len()].join(","));
    let mut stmt = conn.prepare(&sql)?;
    let mut by_key: HashMap<String, SessionRow> = stmt
        .query_map(params_from_iter(&keys), session_row)?
        .map(|r| r.map(|row| (row.key.clone(), row)))
        .collect::<Result<_, _>>()?;
    Ok(keys
        .iter()
        .filter_map(|key| {
            let mut row = by_key.remove(key)?;
            let (id, role, snippet) = best.remove(key)?;
            (row.hit_message_id, row.hit_role, row.snippet) = (Some(id), Some(role), snippet);
            row.match_ids = ids.remove(key).unwrap_or_default();
            row.match_ids.sort_unstable();
            row.hits = row.match_ids.len();
            Some(row)
        })
        .collect())
}

/// Higher is better; `None` = no match. Substrings beat in-order subsequences.
pub fn fuzzy_score(query: &str, title: &str) -> Option<i64> {
    let (q, t) = (query.to_lowercase(), title.to_lowercase());
    let q = q.trim();
    if q.is_empty() {
        return Some(0);
    }
    if let Some(byte) = t.find(q) {
        return Some(10_000 - t[..byte].chars().count() as i64);
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.len() > 1 && words.iter().all(|w| t.contains(w)) {
        return Some(5_000);
    }
    let t: Vec<char> = t.chars().collect();
    let (mut next, mut gaps) = (0usize, 0i64);
    for ch in q.chars().filter(|&c| c != ' ') {
        let i = (next..t.len()).find(|&i| t[i] == ch)?;
        gaps += (i - next) as i64;
        next = i + 1;
    }
    Some(1_000 - gaps)
}

pub fn search_titles(
    conn: &Connection,
    query: &str,
    filters: &Filters,
    sort: Sort,
    limit: usize,
) -> rusqlite::Result<Vec<SessionRow>> {
    // ponytail: scores every filtered title in memory; fine for tens of thousands of sessions.
    let rows = list_sessions(conn, filters, sort, 100_000)?;
    let mut scored: Vec<(i64, SessionRow)> =
        rows.into_iter().filter_map(|r| fuzzy_score(query, &r.title).map(|s| (s, r))).collect();
    scored.sort_by_key(|(score, _)| -score); // stable: ties keep the list order
    Ok(scored.into_iter().take(limit).map(|(_, r)| r).collect())
}

static FILTER_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\s)(agent|branch|since|until|cwd):(\S+)").unwrap());

/// Pull `agent: branch: since: until: cwd:` tokens out of a search string.
pub fn split_filters(text: &str, filters: &Filters) -> (String, Filters) {
    let mut out = filters.clone();
    let mut found = false;
    for c in FILTER_TOKEN.captures_iter(text) {
        found = true;
        let value = Some(c[3].to_string());
        match &c[2] {
            "agent" => out.agent = value,
            "branch" => out.branch = value,
            "since" => out.since = value,
            "until" => out.until = value,
            _ => {
                let base = PathBuf::from(filters.cwd.as_deref().unwrap_or("."));
                let path = base.join(expand(&c[3]));
                let resolved = std::fs::canonicalize(&path).unwrap_or_else(|_| absolute(&path));
                out.exact_cwd = Some(resolved.to_string_lossy().into_owned());
            }
        }
    }
    if !found {
        return (text.to_string(), out);
    }
    (FILTER_TOKEN.replace_all(text, "$1").trim().to_string(), out)
}

/// Distinct project roots (worktree, else cwd) that have sessions, most recently active first.
pub fn projects(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(worktree, cwd) AS root FROM sessions
         WHERE root IS NOT NULL AND user_count + assistant_count > 0
         GROUP BY root ORDER BY MAX(updated_at) DESC",
    )?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

/// Pin or unpin a session; returns whether it is now a favorite.
pub fn toggle_favorite(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    if conn.execute("DELETE FROM favorites WHERE session_key = ?", [key])? > 0 {
        return Ok(false);
    }
    conn.execute("INSERT INTO favorites(session_key) VALUES (?)", [key])?;
    Ok(true)
}

pub struct MessageRow {
    pub role: String,
    pub ts: Option<String>,
    pub text: String,
}

pub fn messages(conn: &Connection, key: &str) -> rusqlite::Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare("SELECT role, ts, text FROM messages WHERE session_key = ? ORDER BY ts, id")?;
    stmt.query_map([key], |r| Ok(MessageRow { role: r.get(0)?, ts: r.get(1)?, text: r.get(2)? }))?.collect()
}

pub fn get_session(conn: &Connection, key: &str) -> rusqlite::Result<Option<SessionRow>> {
    conn.query_row(&format!("SELECT {SESSION_COLUMNS} FROM {FROM} WHERE s.key = ?"), [key], session_row).optional()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::indexer::index_all;
    use crate::testutil::*;

    const ALL: Filters = Filters {
        project: ProjectFilter::All,
        cwd: None,
        exact_cwd: None,
        agent: None,
        branch: None,
        since: None,
        until: None,
        favorites_only: false,
    };

    fn keys(rows: &[SessionRow]) -> Vec<&str> {
        rows.iter().map(|r| r.key.as_str()).collect()
    }

    fn find(db: &Connection, q: &str, scope: Scope, filters: &Filters) -> Vec<SessionRow> {
        search(db, q, scope, filters, Sort::LastActivityDesc, 200).unwrap()
    }

    #[test]
    fn index_and_scoped_search() {
        let e = env();
        let stats = index_all(&e.db, &e.config).unwrap();
        assert_eq!((stats.parsed, stats.skipped_lines), (3, 2));

        assert_eq!(keys(&find(&e.db, "SQLAlchemy", Scope::User, &ALL)), [CLAUDE_KEY]);
        assert!(find(&e.db, "SQLAlchemy", Scope::Assistant, &ALL).is_empty());
        assert_eq!(keys(&find(&e.db, "joinedload", Scope::Assistant, &ALL)), [CLAUDE_KEY]);
        assert!(find(&e.db, "joinedload", Scope::User, &ALL).is_empty());
        assert_eq!(find(&e.db, "approval", Scope::Both, &ALL).len(), 2);
        let hit = &find(&e.db, "joinedload", Scope::Both, &ALL)[0];
        assert_eq!(hit.hit_role.as_deref(), Some("assistant"));
        assert!(hit.snippet.as_ref().unwrap().contains(HL_START));
        assert_eq!(hit.hits, 1);

        // prefix matching of the last word, phrase search, AND semantics
        assert!(!find(&e.db, "joinedl", Scope::Both, &ALL).is_empty());
        assert!(!find(&e.db, "\"composite index\"", Scope::Assistant, &ALL).is_empty());
        assert!(find(&e.db, "composite SQLAlchemy", Scope::Both, &ALL).is_empty());
    }

    #[test]
    fn forbidden_content_never_indexed() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        for marker in FORBIDDEN {
            for scope in [Scope::User, Scope::Assistant, Scope::Both] {
                assert!(find(&e.db, marker, scope, &ALL).is_empty(), "{marker}");
            }
            let n: i64 = e
                .db
                .query_row("SELECT COUNT(*) FROM messages WHERE text LIKE ?", [format!("%{marker}%")], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }
        let mut stmt = e.db.prepare("SELECT state FROM files").unwrap();
        for state in stmt.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            let state = state.unwrap();
            assert!(FORBIDDEN.iter().all(|m| !state.contains(m)));
        }
    }

    #[test]
    fn titles_counts_and_fuzzy_title_search() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        let rows = list_sessions(&e.db, &ALL, Sort::LastActivityDesc, 200).unwrap();
        let by = |k: &str| rows.iter().find(|r| r.key == k).unwrap().clone();
        let claude = by(CLAUDE_KEY);
        assert_eq!((claude.title.as_str(), claude.user_count, claude.assistant_count), ("Approvals perf", 4, 3));
        assert_eq!(by(CODEX_KEY).title, "Approval workflow"); // session_index
        assert_eq!(by("codex:33333333-cccc-4ddd-8eee-000000000003").title, "Rename the legacy module"); // 1st prompt
        assert_eq!(keys(&find(&e.db, "aprvl wrkflw", Scope::Title, &ALL)), [CODEX_KEY]);
        assert_eq!(find(&e.db, "perf", Scope::Title, &ALL)[0].title, "Approvals perf");
        assert_eq!(fuzzy_score("xyz", "Approvals perf"), None);
    }

    #[test]
    fn incremental_append_and_rewrite() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        let path = e.logs.join("claude").join(CLAUDE_FILE);
        let mut data = std::fs::read(&path).unwrap();
        // completes the trailing partial line + one more prompt
        data.extend(b"\n");
        data.extend(json_line("u-new", "2026-10-01T11:00:00Z", "/work/app", &CLAUDE_KEY[7..], "One more").bytes());
        std::fs::write(&path, data).unwrap();
        let stats = index_all(&e.db, &e.config).unwrap();
        assert_eq!((stats.parsed, stats.messages), (1, 2));
        assert_eq!(index_all(&e.db, &e.config).unwrap().parsed, 0); // nothing changed -> nothing parsed

        // rewrite (shorter file): old messages are dropped, not duplicated
        std::fs::write(&path, json_line("z", "2026-10-03T10:00:00Z", "/work/app", &CLAUDE_KEY[7..], "Fresh start"))
            .unwrap();
        index_all(&e.db, &e.config).unwrap();
        let texts: Vec<String> = messages(&e.db, CLAUDE_KEY).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["Fresh start"]);
        assert!(find(&e.db, "SQLAlchemy", Scope::Both, &ALL).is_empty());
    }

    #[test]
    fn deleted_log_removes_session() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        std::fs::remove_file(e.logs.join("codex/sessions").join(CODEX_FILE)).unwrap();
        assert_eq!(index_all(&e.db, &e.config).unwrap().removed_sessions, 1);
        let rows = list_sessions(&e.db, &ALL, Sort::LastActivityDesc, 200).unwrap();
        assert!(!keys(&rows).contains(&CODEX_KEY));
    }

    #[test]
    fn worktree_vs_repository_filter() {
        let e = env();
        let (main, wt, other) = crate::git::tests::make_repo(&e.root);
        let claude_dir = e.logs.join("claude");
        for (name, cwd) in [("main", main.join("src")), ("wt", wt.clone()), ("other", other), ("gone", wt.join("deleted-subdir"))] {
            std::fs::create_dir(claude_dir.join(name)).unwrap();
            let line = json_line(name, "2026-10-05T10:00:00Z", &cwd.to_string_lossy(), name, &format!("hello from {name}"));
            std::fs::write(claude_dir.join(name).join(format!("{name}.jsonl")), line).unwrap();
        }
        index_all(&e.db, &e.config).unwrap();

        let ids = |project: ProjectFilter, cwd: &Path| {
            let filters = Filters { project, cwd: Some(cwd.to_string_lossy().into_owned()), ..ALL };
            let mut ids: Vec<String> = list_sessions(&e.db, &filters, Sort::LastActivityDesc, 200)
                .unwrap()
                .into_iter()
                .map(|r| r.session_id)
                .filter(|id| !["1111", "2222", "3333"].iter().any(|p| id.starts_with(p)))
                .collect();
            ids.sort();
            ids
        };
        // A deleted subdirectory still resolves to the nearest existing ancestor checkout.
        assert_eq!(ids(ProjectFilter::CurrentWorktree, &wt), ["gone", "wt"]);
        assert_eq!(ids(ProjectFilter::CurrentWorktree, &main), ["main"]);
        assert_eq!(ids(ProjectFilter::Repository, &wt), ["gone", "main", "wt"]);
        assert_eq!(ids(ProjectFilter::Repository, &main), ["gone", "main", "wt"]);
        assert_eq!(ids(ProjectFilter::All, &wt), ["gone", "main", "other", "wt"]);
        // exact cwd filter is distinct from worktree filtering
        let exact = Filters { exact_cwd: Some(main.join("src").to_string_lossy().into_owned()), ..ALL };
        let rows = list_sessions(&e.db, &exact, Sort::LastActivityDesc, 200).unwrap();
        assert_eq!(rows.iter().map(|r| r.session_id.as_str()).collect::<Vec<_>>(), ["main"]);
    }

    #[test]
    fn inline_filters() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        let (text, f) = split_filters("approval agent:codex since:2026-10-01", &ALL);
        assert_eq!((text.as_str(), f.agent.as_deref(), f.since.as_deref()), ("approval", Some("codex"), Some("2026-10-01")));
        assert_eq!(keys(&find(&e.db, &text, Scope::Both, &f)), [CODEX_KEY]);
        let (_, f) = split_filters("branch:legacy", &ALL);
        let rows = list_sessions(&e.db, &f, Sort::LastActivityDesc, 200).unwrap();
        assert_eq!(keys(&rows), ["codex:33333333-cccc-4ddd-8eee-000000000003"]);
        assert_eq!(split_filters("xagent:codex", &ALL).1.agent, None);
    }

    #[test]
    fn favorites_pinned_first() {
        let e = env();
        index_all(&e.db, &e.config).unwrap();
        let last = list_sessions(&e.db, &ALL, Sort::LastActivityDesc, 200).unwrap().pop().unwrap();
        assert!(toggle_favorite(&e.db, &last.key).unwrap());
        let first = &list_sessions(&e.db, &ALL, Sort::LastActivityDesc, 200).unwrap()[0];
        assert!(first.key == last.key && first.favorite);
        assert!(!toggle_favorite(&e.db, &last.key).unwrap());
    }

    #[test]
    fn fts_query_is_injection_safe() {
        assert_eq!(fts_query("foo\" OR bar NEAR(").unwrap(), "\"foo\" \"OR\" \"bar\" \"NEAR(\"*");
        assert_eq!(fts_query("   "), None);
    }
}
