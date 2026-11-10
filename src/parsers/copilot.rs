//! GitHub Copilot CLI: `$COPILOT_HOME/session-state/<session-id>/events.jsonl`.
//!
//! Every line is `{type, data, id, timestamp, parentId}`. Only `user.message` (`data.content`,
//! not `transformedContent` which carries injected reminders) and the visible `content` of
//! `assistant.message` are kept. Titles live in `workspace.yaml` (`name`, or `summary` in older versions).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use crate::model::{get_obj, get_str, parse_time, read_records, sanitize, truthy, ParseResult, Role, State};
use crate::parsers::{grandchildren, name_of};

const INJECTED_PREFIXES: [&str; 3] = ["<system_reminder>", "<system-reminder>", "<current_datetime>"];

pub fn titles(roots: &[PathBuf]) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    for root in roots {
        for meta in grandchildren(root, |name| name == "workspace.yaml") {
            let fields = flat_yaml(&meta);
            let title = fields.get("name").filter(|t| !t.is_empty()).or_else(|| fields.get("summary"));
            if let Some(title) = title.filter(|t| !t.is_empty()) {
                titles.insert(name_of(meta.parent().unwrap()).to_string(), sanitize(title));
            }
        }
    }
    titles
}

pub fn parse(path: &Path, offset: u64, state: State) -> io::Result<ParseResult> {
    let mut result = ParseResult::with_state(state);
    result.session.session_id = path.parent().map(|p| name_of(p).to_string());
    read_records(path, offset, &mut result, |line_offset, record, result| {
        let Some(data) = get_obj(&record, "data") else { return };
        let (kind, ts, uid) = (get_str(&record, "type"), parse_time(record.get("timestamp")), format!("o{line_offset}"));
        let s = &mut result.session;
        if ts.is_some() {
            s.updated_at = ts;
        }
        match kind {
            Some(k @ ("session.start" | "session.resume")) => {
                if let Some(context) = get_obj(data, "context") {
                    if let Some(cwd) = get_str(context, "cwd") {
                        s.cwd = Some(cwd.to_string());
                    }
                    if let Some(branch) = get_str(context, "branch") {
                        s.branch = Some(branch.to_string());
                    }
                }
                if k == "session.start" {
                    s.created_at = parse_time(data.get("startTime")).or(ts);
                }
            }
            Some("user.message") => {
                // A `source` (e.g. "instruction-discovery") marks text the human did not type.
                let Some(text) = get_str(data, "content").filter(|_| !truthy(data.get("source"))) else { return };
                let text = text.trim();
                if !text.is_empty() && !INJECTED_PREFIXES.iter().any(|p| text.starts_with(p)) {
                    result.push(Role::User, sanitize(text), ts, uid);
                }
            }
            Some("assistant.message") => {
                if let Some(text) = get_str(data, "content").map(str::trim).filter(|t| !t.is_empty()) {
                    result.push(Role::Assistant, sanitize(text), ts, uid);
                }
            }
            _ => {}
        }
    })?;
    Ok(result)
}

/// `workspace.yaml` is flat `key: value`; enough for `name`/`summary` without a YAML dependency.
fn flat_yaml(path: &Path) -> HashMap<String, String> {
    let Ok(bytes) = std::fs::read(path) else { return HashMap::new() };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.starts_with(' '))
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches(['\'', '"']).to_string()))
        .collect()
}
