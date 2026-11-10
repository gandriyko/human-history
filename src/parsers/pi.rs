//! pi coding agent: `~/.pi/agent/sessions/--<cwd>--/<timestamp>_<session-id>.jsonl`.
//!
//! The first line is the `session` header (`id`, `cwd`, `timestamp`). Conversation entries are
//! `{"type": "message", "message": {"role": ...}}`; only `user` text and the `text` blocks of
//! `assistant` are kept. Branches (`/tree`) append entries, so all messages are kept in file
//! order. `session_info.name` is the user-given title.

use std::io;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::model::{get_obj, get_str, parse_time, read_records, sanitize, text_blocks, ParseResult, Role, State};

/// Expanded `/skill:x` and `@file` arguments are prepended to the prompt the human typed.
static INJECTED_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?s)\s*(?:<skill\s[^>]*>.*?</skill>|<file\s[^>]*>.*?</file>)\s*").unwrap()
});

pub fn human_prompt(mut text: &str) -> String {
    while let Some(m) = INJECTED_BLOCK.find(text) {
        text = &text[m.end()..];
    }
    text.trim().to_string()
}

pub fn parse(path: &Path, offset: u64, state: State) -> io::Result<ParseResult> {
    let mut result = ParseResult::with_state(state);
    read_records(path, offset, &mut result, |line_offset, record, result| {
        let (kind, ts, uid) = (get_str(&record, "type"), parse_time(record.get("timestamp")), format!("o{line_offset}"));
        let s = &mut result.session;
        if kind == Some("session") {
            s.session_id = get_str(&record, "id").map(str::to_string);
            s.cwd = get_str(&record, "cwd").map(str::to_string);
            s.created_at = ts;
            return;
        }
        if ts.is_some() {
            s.updated_at = ts;
        }
        if kind == Some("session_info")
            && let Some(name) = get_str(&record, "name").map(str::trim).filter(|n| !n.is_empty())
        {
            s.title = Some(sanitize(name));
        }
        let Some(message) = get_obj(&record, "message").filter(|_| kind == Some("message")) else { return };
        let text = text_blocks(message.get("content"), "text").join("\n\n");
        let (role, text) = match get_str(message, "role") {
            Some("user") => (Role::User, human_prompt(&text)),
            Some("assistant") => (Role::Assistant, text.trim().to_string()),
            _ => return,
        };
        if !text.is_empty() {
            result.push(role, sanitize(&text), ts, uid);
        }
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_injected_blocks() {
        let text = "<skill name=\"x\">body\n</skill>\n<file path=\"a\">A</file> fix it";
        assert_eq!(super::human_prompt(text), "fix it");
        assert_eq!(super::human_prompt("<skill>no attrs</skill> hi"), "<skill>no attrs</skill> hi");
    }
}
