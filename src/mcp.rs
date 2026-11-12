//! A minimal Model Context Protocol server over stdio (newline-delimited JSON-RPC 2.0).
//! Tool schemas are hand-written next to the argument structs they describe: keep them in sync.

use std::io::{BufRead, Write};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::{absolute, config_path, load_config, ProjectFilter, Scope, Sort};
use crate::model::Agent;
use crate::search::{self, Filters};
use crate::{db, indexer};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const INSTRUCTIONS: &str = "Search and read the user's past conversations with coding agents (Claude Code, Codex, OpenCode, \
Copilot, pi, Hermes). Only the user's prompts and the agents' visible answers are indexed: tool calls, tool output and \
reasoning are not. Use search_sessions to find sessions, then read_session to read one.";

fn args<T: DeserializeOwned>(params: &Value) -> Result<T, String> {
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))
}

fn in_range(name: &str, value: usize, range: std::ops::RangeInclusive<usize>) -> Result<usize, String> {
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(format!("invalid arguments: {name} must be between {} and {}", range.start(), range.end()))
    }
}

fn open() -> Result<(rusqlite::Connection, crate::config::Config), String> {
    let config = load_config(&config_path())?;
    let path = config.index.database_path();
    let conn = db::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((conn, config))
}

fn tools() -> Value {
    let annotations = |read_only: bool| {
        json!({"readOnlyHint": read_only, "idempotentHint": true, "destructiveHint": false, "openWorldHint": false})
    };
    json!([
        {
            "name": "search_sessions",
            "title": "Search past agent sessions",
            "description": "Full-text search over the user's past coding-agent conversations (prompts and visible answers \
only). Words are ANDed, the last bare word matches as a prefix, \"quoted phrases\" match exactly; case and diacritics \
are ignored. Returns sessions, best match first, with a snippet around the best hit. An empty query lists sessions by \
recency. The index is refreshed before every search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search text; empty lists sessions by recency."},
                    "scope": {"type": "string", "enum": ["both", "user", "assistant", "title"], "default": "both",
                        "description": "Search prompts and answers, only the user's prompts, only agent answers, or session titles (fuzzy)."},
                    "project_dir": {"type": "string", "description": "Absolute path of the project the user is working in; enables project_filter."},
                    "project_filter": {"type": "string", "enum": ["all", "repository", "current-worktree"],
                        "description": "Limit to the git repository (all worktrees) or the worktree of project_dir. Default: repository when project_dir is given, else all."},
                    "agent": {"type": "string", "enum": ["claude", "codex", "opencode", "copilot", "pi", "hermes"], "description": "One agent only."},
                    "branch": {"type": "string", "description": "Sessions recorded on this git branch."},
                    "since": {"type": "string", "description": "ISO date or time, e.g. 2026-10-01; last activity at or after it (UTC)."},
                    "until": {"type": "string", "description": "ISO date or time; last activity before it (UTC)."},
                    "cwd": {"type": "string", "description": "Sessions started in exactly this directory."},
                    "favorites_only": {"type": "boolean", "default": false, "description": "Only sessions marked as favorites."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20, "description": "Maximum sessions returned."}
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": annotations(true)
        },
        {
            "name": "read_session",
            "title": "Read a past agent session",
            "description": "Read the conversation of one session (user prompts and agent answers, oldest first). Conversations \
can be long: read a page at a time and continue with offset = next_offset until it is null.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string", "description": "Session id or a unique prefix of it, from search_sessions."},
                    "role": {"type": "string", "enum": ["user", "assistant"], "description": "Only messages by this role."},
                    "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "Messages to skip (after the role filter)."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50, "description": "Maximum messages returned."}
                },
                "required": ["session_id"],
                "additionalProperties": false
            },
            "annotations": annotations(true)
        },
        {
            "name": "set_favorite",
            "title": "Star or unstar a session",
            "description": "Mark a session as a favorite (starred, pinned to the top of the user's list) or remove it. \
Change favorites only when the user asks for it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string", "description": "Session id or a unique prefix of it."},
                    "favorite": {"type": "boolean", "description": "true to add to favorites, false to remove."}
                },
                "required": ["session_id", "favorite"],
                "additionalProperties": false
            },
            "annotations": annotations(false)
        }
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    scope: Scope,
    project_dir: Option<String>,
    project_filter: Option<ProjectFilter>,
    agent: Option<Agent>,
    branch: Option<String>,
    since: Option<String>,
    until: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    favorites_only: bool,
    limit: Option<usize>,
}

fn search_sessions(params: &Value) -> Result<Value, String> {
    let a: SearchArgs = args(params)?;
    let limit = in_range("limit", a.limit.unwrap_or(20), 1..=100)?;
    let project = match (a.project_filter, &a.project_dir) {
        (Some(ProjectFilter::All) | None, None) => ProjectFilter::All,
        (None, Some(_)) => ProjectFilter::Repository,
        (Some(filter), Some(_)) => filter,
        (Some(_), None) => return Err("invalid arguments: project_filter needs project_dir".into()),
    };
    let abs = |p: &str| absolute(Path::new(p)).to_string_lossy().into_owned();
    let filters = Filters {
        project,
        cwd: a.project_dir.as_deref().map(abs),
        exact_cwd: a.cwd.as_deref().map(abs),
        agent: a.agent.map(|x| x.as_str().to_string()),
        branch: a.branch,
        since: a.since,
        until: a.until,
        favorites_only: a.favorites_only,
    };
    let (conn, config) = open()?;
    if let Err(e) = indexer::index_all(&conn, &config) {
        eprintln!("human-history mcp: indexing failed, searching the existing index: {e}");
    }
    let rows = search::search(&conn, &a.query, a.scope, &filters, Sort::LastActivityDesc, limit).map_err(|e| e.to_string())?;
    Ok(json!({ "sessions": rows }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    session_id: String,
    role: Option<Role>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

fn read_session(params: &Value) -> Result<Value, String> {
    let a: ReadArgs = args(params)?;
    let limit = in_range("limit", a.limit.unwrap_or(50), 1..=200)?;
    let (conn, _) = open()?;
    let row = search::find_session(&conn, &a.session_id)?;
    let wanted = a.role.map(|r| if r == Role::User { "user" } else { "assistant" });
    let all = search::messages(&conn, &row.key).map_err(|e| e.to_string())?;
    let selected: Vec<_> = all.iter().enumerate().filter(|(_, m)| wanted.is_none_or(|w| m.role == w)).collect();
    let page: Vec<Value> = selected
        .iter()
        .skip(a.offset)
        .take(limit)
        .map(|(i, m)| json!({"index": i, "role": m.role, "ts": m.ts, "text": m.text}))
        .collect();
    let next = a.offset + page.len();
    Ok(json!({
        "session": row,
        "total_messages": selected.len(),
        "offset": a.offset,
        "messages": page,
        "next_offset": (next < selected.len()).then_some(next),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FavoriteArgs {
    session_id: String,
    favorite: bool,
}

fn set_favorite(params: &Value) -> Result<Value, String> {
    let a: FavoriteArgs = args(params)?;
    let (conn, _) = open()?;
    let row = search::find_session(&conn, &a.session_id)?;
    search::set_favorite(&conn, &row.key, a.favorite).map_err(|e| e.to_string())?;
    Ok(json!({"session_id": row.session_id, "title": row.title, "favorite": a.favorite}))
}

fn call_tool(params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let result = match name {
        "search_sessions" => search_sessions(params),
        "read_session" => read_session(params),
        "set_favorite" => set_favorite(params),
        _ => return Err((-32602, format!("unknown tool {name:?}"))),
    };
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            Ok(json!({"content": [{"type": "text", "text": text}], "structuredContent": value}))
        }
        Err(message) => Ok(json!({"content": [{"type": "text", "text": message}], "isError": true})),
    }
}

/// The response to one line, or `None` for notifications.
fn handle(line: &str) -> Option<Value> {
    let reply = |id: &Value, result: Result<Value, (i64, String)>| {
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        })
    };
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return reply(&Value::Null, Err((-32700, "parse error".into())));
    };
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return reply(&request.get("id").cloned().unwrap_or(Value::Null), Err((-32600, "invalid request".into())));
    };
    let id = request.get("id")?;
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "human-history", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => call_tool(&params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    reply(id, result)
}

/// Serves MCP on stdin/stdout until stdin closes.
pub fn serve() {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle(&line)
            && writeln!(out, "{response}").and_then(|_| out.flush()).is_err()
        {
            break;
        }
    }
}
