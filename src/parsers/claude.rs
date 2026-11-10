//! Claude Code transcripts: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//!
//! Tool results are logged as `type=user` lines and every assistant content block (text,
//! thinking, tool_use) gets its own line, so classification is by block type and metadata,
//! never by role alone.

use std::io;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::model::{
    get_obj, get_str, join_stripped, parse_time, read_records, sanitize, truthy, ParseResult, Role, State, Ts,
};

/// User-role text the human did not type as a conversational prompt.
const NOT_HUMAN_PREFIXES: [&str; 9] = [
    "<local-command-",
    "<bash-stdout",
    "<bash-stderr",
    "<task-notification",
    "<system-reminder",
    "<user-prompt-submit-hook",
    "[Request interrupted by user",
    "This session is being continued from a previous conversation",
    "Caveat: The messages below were generated",
];

fn tag(name: &str) -> Regex {
    Regex::new(&format!("(?s)<{name}>(.*?)</{name}>")).unwrap()
}

static COMMAND_NAME: LazyLock<Regex> = LazyLock::new(|| tag("command-name"));
static COMMAND_ARGS: LazyLock<Regex> = LazyLock::new(|| tag("command-args"));
static BASH_INPUT: LazyLock<Regex> = LazyLock::new(|| tag("bash-input"));
static PASTED_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"</?pasted_content(?: id="[^"]*")?>"#).unwrap());

fn group1<'a>(re: &Regex, text: &'a str) -> Option<&'a str> {
    re.captures(text).map(|c| c.get(1).unwrap().as_str().trim())
}

/// Readable form of a user-line text, or `None` if it is not a human prompt.
pub fn human_prompt(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || NOT_HUMAN_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return None;
    }
    if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
        let name = group1(&COMMAND_NAME, text)?;
        let args = group1(&COMMAND_ARGS, text).unwrap_or("");
        return Some(format!("{name} {args}").trim().to_string());
    }
    if text.starts_with("<bash-input>") {
        return group1(&BASH_INPUT, text).map(|cmd| format!("`! {cmd}`"));
    }
    let cleaned = PASTED_TAG.replace_all(text, "");
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Text blocks of a message; `None` if it carries a tool result (not human input).
fn texts(content: Option<&Value>) -> Option<Vec<String>> {
    match content {
        Some(Value::String(s)) => Some(vec![s.clone()]),
        Some(Value::Array(blocks)) => {
            let mut out = Vec::new();
            for block in blocks.iter().filter_map(Value::as_object) {
                match get_str(block, "type") {
                    Some("tool_result") => return None,
                    Some("text") => out.extend(get_str(block, "text").map(str::to_string)),
                    _ => {}
                }
            }
            Some(out)
        }
        _ => Some(vec![]),
    }
}

fn human_origin(origin: Option<&Value>) -> bool {
    match origin.and_then(Value::as_object) {
        Some(o) => matches!(o.get("kind"), None | Some(Value::Null)) || get_str(o, "kind") == Some("human"),
        None => true,
    }
}

pub fn parse(path: &Path, offset: u64, state: State) -> io::Result<ParseResult> {
    let mut result = ParseResult::with_state(state);
    read_records(path, offset, &mut result, |_, rec, result| {
        let kind = get_str(&rec, "type");
        let (st, s) = (&mut result.state, &mut result.session);
        if kind == Some("custom-title")
            && let Some(title) = get_str(&rec, "customTitle")
        {
            st.insert("custom_title".into(), true.into());
            let title = sanitize(title.trim());
            if !title.is_empty() {
                s.title = Some(title);
            }
            return;
        }
        if kind == Some("ai-title")
            && !truthy(st.get("custom_title"))
            && let Some(title) = get_str(&rec, "aiTitle")
        {
            let title = sanitize(title.trim());
            if !title.is_empty() {
                s.title = Some(title);
            }
            return;
        }
        if !matches!(kind, Some("user" | "assistant" | "attachment")) || truthy(rec.get("isSidechain")) {
            return;
        }

        let ts = parse_time(rec.get("timestamp"));
        if ts.is_some() {
            s.updated_at = ts;
            if !truthy(st.get("created")) {
                st.insert("created".into(), true.into());
                s.created_at = ts;
            }
        }
        if let Some(id) = get_str(&rec, "sessionId") {
            s.session_id = Some(id.to_string());
        }
        if let Some(cwd) = get_str(&rec, "cwd")
            && !truthy(st.get("cwd"))
        {
            st.insert("cwd".into(), true.into()); // the first cwd is where the session lives (and resumes)
            s.cwd = Some(cwd.to_string());
        }
        if let Some(branch) = get_str(&rec, "gitBranch").filter(|b| !b.is_empty() && *b != "HEAD") {
            s.branch = Some(branch.to_string());
        }

        let Some(uid) = get_str(&rec, "uuid") else { return };
        match kind {
            Some("assistant") => assistant(&rec, ts, uid, result),
            Some("user") => user(&rec, ts, uid, result),
            _ => queued(&rec, ts, uid, result),
        }
    })?;
    Ok(result)
}

fn assistant(rec: &Map<String, Value>, ts: Option<Ts>, uid: &str, result: &mut ParseResult) {
    let Some(message) = get_obj(rec, "message") else { return };
    if get_str(message, "model") == Some("<synthetic>") || truthy(rec.get("isApiErrorMessage")) {
        return;
    }
    let text = join_stripped(&texts(message.get("content")).unwrap_or_default());
    if !text.is_empty() {
        result.push(Role::Assistant, sanitize(&text), ts, uid.to_string());
        result.state.insert("last_user".into(), Value::Null);
    }
}

fn user(rec: &Map<String, Value>, ts: Option<Ts>, uid: &str, result: &mut ParseResult) {
    if ["isMeta", "isCompactSummary", "isVisibleInTranscriptOnly"].iter().any(|k| truthy(rec.get(*k))) {
        return;
    }
    if !human_origin(rec.get("origin")) {
        return;
    }
    let Some(texts) = get_obj(rec, "message").and_then(|m| texts(m.get("content"))) else { return };
    if texts.is_empty() {
        return;
    }
    let text = texts.join("\n\n");
    if get_str(rec, "promptSource") == Some("queued") && seen_queued(&text, result) {
        return;
    }
    emit_user(&text, ts, uid, result);
}

/// A prompt typed while the agent was busy is logged only as an attachment.
fn queued(rec: &Map<String, Value>, ts: Option<Ts>, uid: &str, result: &mut ParseResult) {
    let Some(att) = get_obj(rec, "attachment") else { return };
    if get_str(att, "type") != Some("queued_command") || truthy(rec.get("isMeta")) {
        return;
    }
    if att.contains_key("commandMode") && get_str(att, "commandMode") != Some("prompt") {
        return;
    }
    if !human_origin(att.get("origin")) {
        return;
    }
    if let Some(prompt) = get_str(att, "prompt")
        && !seen_queued(prompt, result)
    {
        // source_uuid is the user line written if the prompt is later replayed: same uid -> no duplicate.
        let uid = get_str(att, "source_uuid").filter(|u| !u.is_empty()).unwrap_or(uid);
        emit_user(prompt, ts, uid, result);
    }
}

/// A queued prompt may be logged twice (queued user line + delivery attachment), in either order.
///
/// Returns true for the second copy. Each first copy can absorb exactly one later copy.
fn seen_queued(text: &str, result: &mut ParseResult) -> bool {
    let entry = result.state.entry("queued").or_insert_with(|| Value::Array(vec![]));
    if !entry.is_array() {
        *entry = Value::Array(vec![]);
    }
    let pending = entry.as_array_mut().unwrap();
    let key = text.trim();
    if let Some(i) = pending.iter().position(|v| v.as_str() == Some(key)) {
        pending.remove(i);
        return true;
    }
    pending.push(key.into());
    let excess = pending.len().saturating_sub(20);
    pending.drain(..excess);
    false
}

fn emit_user(raw: &str, ts: Option<Ts>, uid: &str, result: &mut ParseResult) {
    let Some(text) = human_prompt(raw) else { return };
    let text = sanitize(&text);
    // A slash command is logged as a wrapper and as raw "/cmd" text back to back.
    if get_str(&result.state, "last_user") == Some(text.as_str()) {
        return;
    }
    result.state.insert("last_user".into(), text.clone().into());
    result.push(Role::User, text, ts, uid.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn fixture() -> std::path::PathBuf {
        fixtures().join("claude").join(CLAUDE_FILE)
    }

    #[test]
    fn keeps_only_human_conversation() {
        let result = parse(&fixture(), 0, State::new()).unwrap();
        assert_eq!(
            pairs(&result),
            [
                ("user", "Optimize SQLAlchemy queries and fix the N+1 problem in the approval service."),
                ("assistant", "I found three inefficient queries in the approval service."),
                (
                    "assistant",
                    "I updated the queries to use `joinedload` and added regression tests.]52;c;ZXZpbA==[31m"
                ),
                ("user", "/review PR 42"),
                ("user", "`! git status`"),
                ("user", "Are all tests passing?"),
                ("assistant", "Yes. All 24 approval service tests passed successfully."),
            ]
        );
        let blob: String = result.messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>().join("\n");
        for marker in FORBIDDEN {
            assert!(!blob.contains(marker), "{marker}");
        }
        assert!(!blob.contains('\x1b') && !blob.contains('\x07'));
    }

    #[test]
    fn metadata_and_titles() {
        let result = parse(&fixture(), 0, State::new()).unwrap();
        let s = &result.session;
        assert_eq!(s.session_id.as_deref(), Some("11111111-aaaa-4bbb-8ccc-000000000001"));
        assert_eq!(s.cwd.as_deref(), Some("/work/app"));
        assert_eq!(s.branch.as_deref(), Some("feature/approvals"));
        assert_eq!(s.title.as_deref(), Some("Approvals perf")); // custom title beats earlier and later ai-title
        assert!(s.created_at.unwrap().to_rfc3339().starts_with("2026-10-01T09:59"));
        assert!(s.updated_at.unwrap().to_rfc3339().starts_with("2026-10-01T10:00:25"));
        assert_eq!(result.skipped_lines, 1);
        let uids: Vec<_> =
            result.messages.iter().filter(|m| m.text == "Are all tests passing?").map(|m| m.uid.as_str()).collect();
        assert_eq!(uids, ["u-queued-src"]);
    }

    #[test]
    fn incremental_parse_and_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let data = std::fs::read(fixture()).unwrap();
        assert!(!data.ends_with(b"\n"), "fixture must end with an unfinished line");
        std::fs::write(&path, &data).unwrap();
        let first = parse(&path, 0, State::new()).unwrap();
        assert!(first.messages.iter().all(|m| m.text != "still being written"));
        let last_line = data.rsplit(|&b| b == b'\n').next().unwrap();
        assert_eq!(first.offset as usize, data.len() - last_line.len());

        std::fs::write(&path, [data.as_slice(), b"\n"].concat()).unwrap();
        let second = parse(&path, first.offset, first.state).unwrap();
        assert_eq!(pairs(&second), [("user", "still being written")]);
        assert!(second.session.title.is_none());
    }

    #[test]
    fn prompt_classifier() {
        assert_eq!(human_prompt("<command-name>/clear</command-name>").as_deref(), Some("/clear"));
        assert_eq!(human_prompt("[Request interrupted by user]"), None);
        assert_eq!(
            human_prompt(r#" <pasted_content id="ab12">big log</pasted_content id="ab12">"#).as_deref(),
            Some("big log")
        );
        assert_eq!(human_prompt("<local-command-stdout>x</local-command-stdout>"), None);
        assert_eq!(human_prompt("<b>html is fine</b>").as_deref(), Some("<b>html is fine</b>"));
    }

    #[test]
    fn discovery_skips_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("proj/sess/subagents")).unwrap();
        std::fs::write(root.join("proj/sess.jsonl"), "").unwrap();
        std::fs::write(root.join("proj/sess/subagents/agent-1.jsonl"), "").unwrap();
        let found = crate::parsers::discover(crate::model::Agent::Claude, &[root.to_path_buf()]);
        assert_eq!(found, [root.join("proj/sess.jsonl")]);
    }
}
