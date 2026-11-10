//! Shared test fixtures (`tests/fixtures`, also used by the end-to-end tests in `tests/cli.rs`).

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::Config;
use crate::db::connect;

pub const CLAUDE_FILE: &str = "-work-app/11111111-aaaa-4bbb-8ccc-000000000001.jsonl";
pub const CODEX_FILE: &str = "2026/10/02/rollout-2026-10-02T08-00-00-22222222-bbbb-4ccc-8ddd-000000000002.jsonl";
pub const CLAUDE_KEY: &str = "claude:11111111-aaaa-4bbb-8ccc-000000000001";
pub const CODEX_KEY: &str = "codex:22222222-bbbb-4ccc-8ddd-000000000002";
/// Text placed only inside tool calls/results, reasoning, system/meta events. Never indexed or shown.
pub const FORBIDDEN: [&str; 5] = ["TOOLSECRET", "REASONINGSECRET", "SYSTEMSECRET", "HOOKSECRET", "SIDECHAINSECRET"];

/// Serializes tests that change process-wide environment variables (config path, PATH, HERDR_ENV).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An executable shell script at `path`.
pub fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

pub fn pairs(result: &crate::model::ParseResult) -> Vec<(&str, &str)> {
    result.messages.iter().map(|m| (m.role.as_str(), m.text.as_str())).collect()
}

pub fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn json_line(uuid: &str, ts: &str, cwd: &str, session: &str, content: &str) -> String {
    serde_json::json!({
        "type": "user", "uuid": uuid, "timestamp": ts, "cwd": cwd, "sessionId": session,
        "message": {"role": "user", "content": content},
    })
    .to_string()
        + "\n"
}

pub struct Env {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub logs: PathBuf,
    pub config: Config,
    pub db: Connection,
}

/// Writable copy of the fixture logs; only claude + codex are indexed unless a test adds the others.
pub fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let logs = root.join("logs");
    copy_dir(&fixtures(), &logs);
    let mut config = Config::default();
    config.paths.claude = vec![lossy(&logs.join("claude"))];
    config.paths.codex = vec![lossy(&logs.join("codex/sessions"))];
    // Never the real ~/.copilot etc.
    config.paths.opencode = vec![];
    config.paths.copilot = vec![];
    config.paths.pi = vec![];
    config.index.database = Some(lossy(&root.join("index.sqlite3")));
    config.index.auto_refresh = false;
    let db = connect(&config.index.database_path()).unwrap();
    Env { _dir: dir, root, logs, config, db }
}
