//! Agent log parsers. A parser turns one agent log file into normalized human-readable messages.
//!
//! Parsing is incremental: callers pass the byte offset (and opaque state) returned by the previous
//! run. OpenCode keeps every session in one SQLite database and is read by `opencode::parse_store`.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod opencode;
pub mod pi;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::{Agent, ParseResult, State};

pub fn discover(agent: Agent, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        match agent {
            // Subagent transcripts (`<session>/subagents/*.jsonl`) are not human conversations.
            Agent::Claude => out.extend(grandchildren(root, |name| name.ends_with(".jsonl"))),
            Agent::Codex => walk(root, &mut out, &|name| name.starts_with("rollout-") && name.ends_with(".jsonl")),
            Agent::Copilot => out.extend(grandchildren(root, |name| name == "events.jsonl")),
            Agent::Pi => walk(root, &mut out, &|name| name.ends_with(".jsonl")),
            Agent::Opencode if root.is_file() => out.push(root.clone()),
            Agent::Opencode => out.extend(
                children(root)
                    .into_iter()
                    .filter(|p| p.is_file() && name_of(p).starts_with("opencode") && name_of(p).ends_with(".db")),
            ),
        }
    }
    out
}

/// Agent-provided session titles keyed by session id.
pub fn titles(agent: Agent, roots: &[PathBuf]) -> BTreeMap<String, String> {
    match agent {
        Agent::Codex => codex::titles(roots),
        Agent::Copilot => copilot::titles(roots),
        _ => BTreeMap::new(), // inside the transcript, or delivered with each session
    }
}

pub fn parse(agent: Agent, path: &Path, offset: u64, state: State) -> io::Result<ParseResult> {
    match agent {
        Agent::Claude => claude::parse(path, offset, state),
        Agent::Codex => codex::parse(path, offset, state),
        Agent::Copilot => copilot::parse(path, offset, state),
        Agent::Pi => pi::parse(path, offset, state),
        Agent::Opencode => unreachable!("OpenCode is a multi-session store: use opencode::parse_store"),
    }
}

pub(crate) fn name_of(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// Sorted entries of a directory (empty when it is not one).
fn children(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// `root.glob("*/<name>")` for files.
pub(crate) fn grandchildren(root: &Path, matches: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    children(root)
        .into_iter()
        .filter(|d| d.is_dir())
        .flat_map(|d| children(&d))
        .filter(|p| matches(name_of(p)) && p.is_file())
        .collect()
}

/// `root.rglob(pattern)` for files; symlinked directories are not followed.
fn walk(dir: &Path, out: &mut Vec<PathBuf>, matches: &dyn Fn(&str) -> bool) {
    for path in children(dir) {
        let is_real_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if is_real_dir {
            walk(&path, out, matches);
        } else if matches(name_of(&path)) && path.is_file() {
            out.push(path);
        }
    }
}
