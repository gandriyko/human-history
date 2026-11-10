//! OpenAI Codex CLI rollouts: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`.
//!
//! Handles the wrapped format (`{"timestamp", "type", "payload"}`) and the early bare format
//! (first line is raw meta, then bare ResponseItems). Prompts and answers are written twice in
//! the wrapped format (`event_msg` + `response_item`); see `emit_user` for de-duplication.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::model::{
    get_obj, get_str, parse_time, read_records, sanitize, text_blocks, truthy, ParseResult, Role, State, Ts,
};

/// Text Codex injects as role=user that the human never typed.
const INJECTED_PREFIXES: [&str; 8] = [
    "# AGENTS.md instructions for ",
    "<environment_context>",
    "<user_instructions>",
    "<INSTRUCTIONS>",
    "<turn_aborted>",
    "<user_shell_command>",
    "<subagent_notification>",
    "<skill>",
];
const IDE_REQUEST_MARKER: &str = "## My request for Codex:";
static ENV_CWD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<environment_context>.*?<cwd>(.*?)</cwd>").unwrap());

/// Strip IDE context wrappers; `None` if the text is an injection, not a prompt.
pub fn human_prompt(text: &str) -> Option<String> {
    let mut stripped = text.trim();
    if stripped.is_empty() || INJECTED_PREFIXES.iter().any(|p| stripped.starts_with(p)) {
        return None;
    }
    if stripped.starts_with("# Context from my IDE setup:")
        && let Some((_, request)) = stripped.rsplit_once(IDE_REQUEST_MARKER)
    {
        stripped = request.trim();
    }
    (!stripped.is_empty()).then(|| stripped.to_string())
}

/// Short thread names from `$CODEX_HOME/session_index.jsonl` (latest entry wins).
pub fn titles(roots: &[PathBuf]) -> BTreeMap<String, String> {
    let indexes: BTreeSet<PathBuf> =
        roots.iter().filter_map(|r| r.parent()).map(|p| p.join("session_index.jsonl")).collect();
    let mut best: BTreeMap<String, (String, String)> = BTreeMap::new();
    for index in indexes {
        let Ok(bytes) = std::fs::read(&index) else { continue };
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) else { continue };
            let updated = match row.get("updated_at") {
                Some(Value::String(s)) => s.clone(),
                None | Some(Value::Null) => String::new(),
                Some(other) => other.to_string(),
            };
            let (Some(sid), Some(name)) = (get_str(&row, "id"), get_str(&row, "thread_name")) else { continue };
            let fresh = best.get(sid).is_none_or(|(prev, _)| updated >= *prev);
            if !name.trim().is_empty() && fresh {
                best.insert(sid.to_string(), (updated, sanitize(name.trim())));
            }
        }
    }
    best.into_iter().map(|(sid, (_, name))| (sid, name)).collect()
}

pub fn parse(path: &Path, offset: u64, state: State) -> io::Result<ParseResult> {
    let mut result = ParseResult::with_state(state);
    read_records(path, offset, &mut result, |line_offset, record, result| {
        if record.contains_key("record_type") {
            return;
        }
        let ts = parse_time(record.get("timestamp"));
        let mut kind = record.get("type").and_then(Value::as_str).map(str::to_string);
        let payload: &Map<String, Value> = match get_obj(&record, "payload") {
            Some(p) => p,
            None if matches!(record.get("type"), None | Some(Value::Null)) && record.contains_key("id") => {
                kind = Some("session_meta".into()); // early bare format: raw meta line
                &record
            }
            None if kind.is_some() => {
                kind = Some("response_item".into()); // early bare format: bare ResponseItem
                &record
            }
            None => return,
        };
        if ts.is_some() {
            result.session.updated_at = ts;
        }
        let uid = format!("o{line_offset}");
        match (kind.as_deref(), get_str(payload, "type")) {
            (Some("session_meta"), _) => meta(payload, result),
            (Some("event_msg"), Some("user_message")) => {
                result.state.insert("events".into(), true.into());
                if let Some(text) = get_str(payload, "message") {
                    emit_user(text, ts, uid, result);
                }
            }
            (Some("response_item"), Some("message")) => match get_str(payload, "role") {
                Some("user") if !truthy(result.state.get("events")) => {
                    let text = text_blocks(payload.get("content"), "input_text").join("\n\n");
                    if result.session.cwd.is_none()
                        && !truthy(result.state.get("cwd"))
                        && let Some(m) = ENV_CWD.captures(&text)
                    {
                        // early rollouts: cwd only lives here
                        result.session.cwd = Some(m[1].trim().to_string());
                    }
                    emit_user(&text, ts, uid, result);
                }
                Some("assistant") => {
                    let text = text_blocks(payload.get("content"), "output_text").join("\n\n");
                    let text = text.trim();
                    if !text.is_empty() {
                        result.push(Role::Assistant, sanitize(text), ts, uid);
                        result.state.insert("last_user".into(), Value::Null);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    })?;
    Ok(result)
}

fn meta(payload: &Map<String, Value>, result: &mut ParseResult) {
    let s = &mut result.session;
    if let Some(id) = get_str(payload, "id") {
        s.session_id = Some(id.to_string());
    }
    if let Some(cwd) = get_str(payload, "cwd") {
        s.cwd = Some(cwd.to_string());
        result.state.insert("cwd".into(), true.into());
    }
    if let Some(branch) = get_obj(payload, "git").and_then(|g| get_str(g, "branch")) {
        s.branch = Some(branch.to_string());
    }
    if let Some(created) = parse_time(payload.get("timestamp")) {
        s.created_at = Some(created);
    }
}

fn emit_user(raw: &str, ts: Option<Ts>, uid: String, result: &mut ParseResult) {
    let Some(text) = human_prompt(raw) else { return };
    let text = sanitize(&text);
    // The same prompt arrives as response_item then event_msg with nothing in between.
    if get_str(&result.state, "last_user") == Some(text.as_str()) {
        return;
    }
    result.state.insert("last_user".into(), text.clone().into());
    result.push(Role::User, text, ts, uid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn sessions() -> PathBuf {
        fixtures().join("codex/sessions")
    }

    #[test]
    fn wrapped_format_dedupes_and_filters() {
        let result = parse(&sessions().join(CODEX_FILE), 0, State::new()).unwrap();
        assert_eq!(
            pairs(&result),
            [
                ("user", "Explain the approval workflow"),
                ("assistant", "Let me read the service first."),
                ("assistant", "The approval workflow has three levels."),
                ("user", "Add an index on approval_level"),
                ("assistant", "Added a composite index."),
            ]
        );
        for m in &result.messages {
            assert!(FORBIDDEN.iter().all(|f| !m.text.contains(f)));
        }
        let s = &result.session;
        assert_eq!(s.session_id.as_deref(), Some("22222222-bbbb-4ccc-8ddd-000000000002"));
        assert_eq!((s.cwd.as_deref(), s.branch.as_deref()), (Some("/work/svc"), Some("main")));
        assert_eq!(result.skipped_lines, 1);
    }

    #[test]
    fn legacy_bare_format() {
        let path =
            sessions().join("2025/07/01/rollout-2025-07-01T10-00-00-33333333-cccc-4ddd-8eee-000000000003.jsonl");
        let result = parse(&path, 0, State::new()).unwrap();
        assert_eq!(pairs(&result), [("user", "Rename the legacy module"), ("assistant", "Renamed it.")]);
        assert_eq!(result.session.session_id.as_deref(), Some("33333333-cccc-4ddd-8eee-000000000003"));
        assert_eq!(result.session.cwd.as_deref(), Some("/work/old"));
        assert_eq!(result.session.branch.as_deref(), Some("legacy"));
    }

    #[test]
    fn titles_latest_wins() {
        let titles = titles(&[sessions()]);
        assert_eq!(
            titles.into_iter().collect::<Vec<_>>(),
            [("22222222-bbbb-4ccc-8ddd-000000000002".to_string(), "Approval workflow".to_string())]
        );
    }

    #[test]
    fn incremental() {
        let src = std::fs::read(sessions().join(CODEX_FILE)).unwrap();
        let lines: Vec<&[u8]> = src.split_inclusive(|&b| b == b'\n').collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, lines[..8].concat()).unwrap();
        let first = parse(&path, 0, State::new()).unwrap();
        assert_eq!(first.messages.iter().map(|m| m.role).collect::<Vec<_>>(), [Role::User]);
        std::fs::write(&path, &src).unwrap();
        let rest = parse(&path, first.offset, first.state.clone()).unwrap();
        assert_eq!(rest.messages[0].text, "Let me read the service first.");
        assert_eq!(first.messages.len() + rest.messages.len(), 5);
    }
}
