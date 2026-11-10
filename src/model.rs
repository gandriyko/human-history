//! Normalized data shared by parsers, storage and the CLI, plus shared JSONL reading.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Ts = DateTime<Utc>;
/// Parser-private carry-over between incremental runs, stored as JSON in `files.state`.
pub type State = Map<String, Value>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Opencode,
    Copilot,
    Pi,
    Hermes,
}

impl Agent {
    pub const ALL: [Agent; 6] =
        [Agent::Claude, Agent::Codex, Agent::Opencode, Agent::Copilot, Agent::Pi, Agent::Hermes];

    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Opencode => "opencode",
            Agent::Copilot => "copilot",
            Agent::Pi => "pi",
            Agent::Hermes => "hermes",
        }
    }

    pub fn label(name: &str) -> &str {
        match name {
            "claude" => "Claude",
            "codex" => "Codex",
            "opencode" => "OpenCode",
            "copilot" => "Copilot",
            "pi" => "Pi",
            "hermes" => "Hermes",
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub timestamp: Option<Ts>,
    /// Stable per-session identity used to drop duplicates (streaming chunks, replays, resumed copies).
    pub uid: String,
}

/// Session metadata discovered while parsing; `None` means "unknown / keep previous".
#[derive(Clone, Debug, Default)]
pub struct SessionPatch {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub created_at: Option<Ts>,
    pub updated_at: Option<Ts>,
}

#[derive(Clone, Debug, Default)]
pub struct ParseResult {
    pub session: SessionPatch,
    pub messages: Vec<Message>,
    pub offset: u64,
    pub state: State,
    pub skipped_lines: u64,
}

impl ParseResult {
    pub fn with_state(state: State) -> Self {
        ParseResult { state, ..Default::default() }
    }

    pub fn push(&mut self, role: Role, text: String, timestamp: Option<Ts>, uid: String) {
        self.messages.push(Message { role, text, timestamp, uid });
    }
}

/// Make untrusted session text safe to render in a terminal: drops C0/C1 controls except `\t`, `\n`.
pub fn sanitize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| !matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f'..='\u{9f}'))
        .collect()
}

/// ISO-8601 text -> UTC. Naive timestamps are treated as UTC.
pub fn parse_time(value: Option<&Value>) -> Option<Ts> {
    let s = value?.as_str()?.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.to_utc());
    }
    if let Ok(t) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z") {
        return Some(t.to_utc());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(t.and_utc());
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

/// Sortable UTC text, identical to Python's `isoformat(timespec="milliseconds")` of a UTC datetime.
pub fn iso(value: Option<Ts>) -> Option<String> {
    value.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string())
}

pub fn first_line_title(text: &str) -> String {
    const LIMIT: usize = 90;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '>', '*', '-', ' ']).trim();
    if line.chars().count() <= LIMIT {
        return line.to_string();
    }
    let cut: String = line.chars().take(LIMIT - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Calls `f(line_offset, record, result)` for every complete JSON-object line after `offset`.
///
/// A trailing line without `\n` is still being written: it is left for the next run.
/// Malformed lines are counted and skipped. `result.offset` ends after the last complete line.
pub fn read_records(
    path: &Path,
    offset: u64,
    result: &mut ParseResult,
    mut f: impl FnMut(u64, Map<String, Value>, &mut ParseResult),
) -> io::Result<()> {
    result.offset = offset;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut position = offset;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if reader.read_until(b'\n', &mut raw)? == 0 || raw.last() != Some(&b'\n') {
            return Ok(());
        }
        let line_offset = position;
        position += raw.len() as u64;
        result.offset = position;
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Value>(&raw) {
            Ok(Value::Object(record)) => f(line_offset, record, result),
            _ => result.skipped_lines += 1,
        }
    }
}

// ---- small helpers for loosely typed JSON

pub fn get_str<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

pub fn get_obj<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key).and_then(Value::as_object)
}

/// Python truthiness of an optional JSON value.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The `text` strings of `{"type": block_type, "text": ...}` blocks (or the content itself if a string).
pub fn text_blocks(content: Option<&Value>, block_type: &str) -> Vec<String> {
    match content {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|b| get_str(b, "type") == Some(block_type))
            .filter_map(|b| get_str(b, "text").map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

/// `"\n\n".join(t.strip() for t in texts if t.strip())`
pub fn join_stripped(texts: &[String]) -> String {
    texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_terminal_escapes() {
        assert_eq!(sanitize("a\x1b[31mb\r\nc\x07\u{9b}\td"), "a[31mb\nc\td");
    }

    #[test]
    fn iso_matches_python_format() {
        let t = parse_time(Some(&Value::from("2026-10-01T10:00:25.123456Z")));
        assert_eq!(iso(t).unwrap(), "2026-10-01T10:00:25.123+00:00");
        let t = parse_time(Some(&Value::from("2026-10-01T12:00:00+02:00")));
        assert_eq!(iso(t).unwrap(), "2026-10-01T10:00:00.000+00:00");
        assert!(parse_time(Some(&Value::from("nope"))).is_none());
    }

    #[test]
    fn titles_are_trimmed() {
        assert_eq!(first_line_title("\n\n## Fix it\nmore"), "Fix it");
        let long = "x".repeat(100);
        assert_eq!(first_line_title(&long).chars().count(), 90);
    }
}
