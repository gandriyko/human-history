//! End-to-end tests of the `human-history` binary in an isolated environment: its own config,
//! index, PATH and fake `claude` / `herdr` executables (nothing real is ever started).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    /// Indexes the shared fixtures (read-only) plus `root/claude` for sessions a test adds.
    fn new() -> Env {
        let env = Env { dir: tempfile::tempdir().unwrap() };
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for dir in ["bin", "claude", "state"] {
            std::fs::create_dir(env.path(dir)).unwrap();
        }
        let config = format!(
            "[paths]\nclaude = [{:?}, {:?}]\ncodex = [{:?}]\nopencode = []\ncopilot = []\npi = []\n\n[index]\ndatabase = {:?}\n",
            fixtures.join("claude"),
            env.path("claude"),
            fixtures.join("codex/sessions"),
            env.path("index.sqlite3"),
        );
        std::fs::write(env.path("config.toml"), config).unwrap();
        env
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_human-history"));
        cmd.args(args)
            .env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()))
            .env("HUMAN_HISTORY_CONFIG", self.path("config.toml"))
            .env("LOG", self.path("calls.log"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A Claude session recorded in `cwd`.
    fn session(&self, id: &str, cwd: &Path, ts: &str, prompt: &str) {
        let dir = self.path("claude").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({
            "type": "user", "uuid": format!("u-{id}"), "timestamp": ts, "cwd": cwd, "sessionId": id,
            "message": {"role": "user", "content": prompt},
        });
        std::fs::write(dir.join(format!("{id}.jsonl")), format!("{line}\n")).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("calls.log")).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn index_then_search_as_table_and_json() {
    let env = Env::new();
    assert!(env.ok(&["index"]).starts_with("scanned 3 files, parsed 3, +"));
    assert!(env.ok(&["index"]).starts_with("scanned 3 files, parsed 0, +0"));

    let rows: Vec<Value> = serde_json::from_str(&env.ok(&["search", "approval", "--json"])).unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let snippet = row["snippet"].as_str().unwrap();
        assert!(snippet.to_lowercase().contains("approval") && !snippet.contains(['\x02', '\x03']));
        assert!(row["hits"].as_u64().unwrap() >= 1 && row["session_id"].is_string());
    }
    let codex: Vec<Value> = serde_json::from_str(&env.ok(&["search", "approval", "--agent", "codex", "--json"])).unwrap();
    assert!(codex.iter().all(|r| r["agent"] == "codex") && !codex.is_empty());
    let user_only: Vec<Value> = serde_json::from_str(&env.ok(&["search", "joinedload", "-s", "user", "--json"])).unwrap();
    assert!(user_only.is_empty());
    let titles: Vec<Value> = serde_json::from_str(&env.ok(&["search", "aprvl wrkflw", "-s", "title", "--json"])).unwrap();
    assert_eq!(titles[0]["title"], "Approval workflow");
    assert_eq!(env.ok(&["search", "zzzqqq", "--json"]).trim(), "[]");

    let table = env.ok(&["search", "approval"]);
    assert!(table.contains("Approval workflow") && table.contains("Approvals perf"), "{table}");
    assert!(!table.contains('\x1b'), "no colours when piped");
    assert!(!table.contains('★'));
    let db = rusqlite::Connection::open(env.path("index.sqlite3")).unwrap();
    db.execute("INSERT INTO favorites SELECT key FROM sessions WHERE title = 'Approvals perf'", []).unwrap();
    let table = env.ok(&["search", "approval"]);
    assert!(table.contains("★ Approvals perf") && !table.contains("★ Approval workflow"), "{table}");
    assert!(env.ok(&["search", "zzzqqq"]).contains("no matches"));

    let bad = env.run(&["search", "x", "-s", "everything"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(env.ok(&["index", "--rebuild"]).starts_with("scanned 3 files, parsed 3"));
}

#[test]
fn show_prints_only_the_human_conversation() {
    let env = Env::new();
    env.ok(&["index"]);
    let out = env.ok(&["show", "1111"]);
    assert!(out.starts_with("Approvals perf"), "{out}");
    assert!(out.contains("USER") && out.contains("CLAUDE") && out.contains("N+1 problem"));
    for marker in ["TOOLSECRET", "REASONINGSECRET", "SYSTEMSECRET", "HOOKSECRET", "SIDECHAINSECRET", "\x1b"] {
        assert!(!out.contains(marker), "{marker}");
    }
    let json: Value = serde_json::from_str(&env.ok(&["show", "1111", "--json"])).unwrap();
    assert_eq!(json["session"]["title"], "Approvals perf");
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
    assert!(messages.iter().any(|m| m["text"].as_str().unwrap().contains("N+1 problem")));
    assert!(!json.to_string().contains("TOOLSECRET"));
    let missing = env.run(&["show", "nope"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("no session matching"));
    assert!(stderr(&env.run(&["show", ""])).contains("ambiguous"));
}

#[test]
fn config_init_validate_and_report() {
    let env = Env::new();
    let fresh = env.path("fresh/config.toml");
    let init = |force: bool| {
        let mut args = vec!["config", "--init"];
        if force {
            args.push("--force");
        }
        env.command(&args).env("HUMAN_HISTORY_CONFIG", &fresh).output().unwrap()
    };
    assert!(init(false).status.success());
    assert!(std::fs::read_to_string(&fresh).unwrap().contains("[remembered]"));
    let again = init(false);
    assert_eq!(again.status.code(), Some(1));
    assert!(stderr(&again).contains("already exists"));
    assert!(init(true).status.success());

    let out = env.ok(&["config"]);
    assert!(out.contains("(found)") && out.contains("index.sqlite3"));
    let json: Value = serde_json::from_str(&out[out.find('{').unwrap()..]).unwrap();
    assert_eq!(json["search"]["limit"], 200);
    assert_eq!(json["paths"]["opencode"], serde_json::json!([]));

    std::fs::write(env.path("config.toml"), "[search]\nlimit = 3\n[ui]\nnope = 1\n").unwrap();
    for args in [&["config"][..], &["index"], &["search", "x"]] {
        let bad = env.run(args);
        assert_eq!(bad.status.code(), Some(2), "{args:?}");
        assert!(stderr(&bad).contains("invalid configuration"), "{}", stderr(&bad));
    }
}

#[test]
fn invalid_filter_from_herdr_is_rejected_before_the_tui_starts() {
    let env = Env::new();
    let out = env.command(&["tui"]).env("HH_FILTER", "everywhere").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("invalid filter"));
}

#[test]
fn resume_last_runs_the_agent_in_the_sessions_own_directory() {
    let env = Env::new();
    let project = env.path("project");
    let other = env.path("other");
    std::fs::create_dir_all(project.join("sub")).unwrap();
    std::fs::create_dir(&other).unwrap();
    env.session("older-here", &project.join("sub"), "2026-10-01T10:00:00Z", "older");
    env.session("latest-here", &project, "2026-10-02T10:00:00Z", "latest");
    env.session("elsewhere", &other, "2026-10-03T10:00:00Z", "newest but another project");
    env.script("claude", r#"{ pwd; echo "$@"; } >> "$LOG""#);

    let out = env.command(&["resume-last"]).env("HH_CWD", &project).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let real = std::fs::canonicalize(&project).unwrap();
    assert_eq!(env.calls(), [real.to_string_lossy().as_ref(), "--resume latest-here"]);

    let none = env.command(&["resume-last"]).env("HH_CWD", env.path("bin")).output().unwrap();
    assert_eq!(none.status.code(), Some(1));
    assert!(stderr(&none).contains("no sessions recorded"));
}

/// A fake herdr: logs every call; `pane list` answers with $PANES.
const HERDR: &str = r#"echo "$*" >> "$LOG"
case "$1 $2 $3" in
  "pane list "*) echo "{\"result\":{\"panes\":${PANES:-[]}}}" ;;
  "tab create "*) echo '{"result":{"root_pane":{"pane_id":"p9"}}}' ;;
  "plugin pane open") echo '{"result":{"plugin_pane":{"pane":{"pane_id":"hp1"}}}}' ;;
  *) echo '{}' ;;
esac"#;

#[test]
fn resume_last_inside_herdr_opens_a_tab_or_focuses_the_running_pane() {
    let env = Env::new();
    let project = env.path("my project");
    std::fs::create_dir(&project).unwrap();
    env.session("sess-1", &project, "2026-10-02T10:00:00Z", "Fix the it's-broken bug");
    env.script("claude", "exit 0");
    env.script("herdr", HERDR);
    let herdr = |panes: &str| {
        env.command(&["resume-last"])
            .env("HH_CWD", &project)
            .env("HERDR_ENV", "1")
            .env("HERDR_BIN_PATH", env.path("bin/herdr"))
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("PANES", panes)
            .output()
            .unwrap()
    };

    let out = herdr("[]");
    assert!(out.status.success(), "{}", stderr(&out));
    let real = std::fs::canonicalize(&project).unwrap();
    assert_eq!(
        env.calls(),
        [
            "pane list".to_string(),
            format!("tab create --cwd {} --label Fix the it's-broken bug --focus --workspace w1", real.display()),
            "pane run p9 claude --resume sess-1".to_string(),
        ]
    );

    std::fs::remove_file(env.path("calls.log")).unwrap();
    let running = r#"[{"pane_id":"p3","agent":"claude","agent_session":{"agent":"claude","value":"sess-1"}}]"#;
    assert!(herdr(running).status.success());
    assert_eq!(env.calls(), ["pane list", "agent focus p3"]);
}

#[test]
fn herdr_actions_open_and_toggle_the_history_pane() {
    let env = Env::new();
    env.script("herdr", HERDR);
    let action = |name: &str| {
        env.command(&["herdr-action", name])
            .env("HERDR_BIN_PATH", env.path("bin/herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", env.path("state"))
            .env(
                "HERDR_PLUGIN_CONTEXT_JSON",
                r#"{"focused_pane_cwd":"/repo/x","workspace_id":"w1","focused_pane_id":"p1"}"#,
            )
            .output()
            .unwrap()
    };

    assert!(action("recent-here").status.success());
    assert_eq!(
        env.calls(),
        ["plugin pane open --plugin human-history --entrypoint browser --placement overlay --cwd /repo/x \
          --env HH_CWD=/repo/x --env HH_WORKSPACE=w1 --env HH_TARGET_PANE=p1 --env HH_FILTER=repository --focus"]
    );
    let pane_file = env.path("state/browser-pane");
    assert_eq!(std::fs::read_to_string(&pane_file).unwrap(), "hp1");

    // the same key again closes it
    assert!(action("search").status.success());
    assert_eq!(env.calls().last().unwrap(), "plugin pane close hp1");
    assert!(!pane_file.exists());

    assert!(action("refresh-index").status.success());
    assert!(env.calls().last().unwrap().starts_with("notification show human-history --body Indexed 3 changed files"));
    assert_eq!(action("bogus").status.code(), Some(2));
}
