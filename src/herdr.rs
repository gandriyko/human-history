//! Herdr integration through its CLI (verified against herdr 0.9.5).
//!
//! Used commands: `pane list`, `pane process-info`, `agent focus`, `tab create`, `pane split`,
//! `pane run`, `plugin pane open/close`, `notification show`. All calls pass argv lists; the only
//! text typed into a shell is `shell_join` of a validated argv.

use std::env;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::config::{name, HistoryPlacement, Placement};
use crate::resume::{which, ResumePlan};

const PLUGIN_ID: &str = "human-history";
const ENTRYPOINT: &str = "browser";
const TIMEOUT: Duration = Duration::from_secs(15);

fn herdr_bin() -> Option<PathBuf> {
    env::var_os("HERDR_BIN_PATH").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| which("herdr"))
}

pub fn inside_herdr() -> bool {
    env::var("HERDR_ENV").as_deref() == Ok("1") && herdr_bin().is_some()
}

fn call(args: &[&str]) -> Result<Value, String> {
    let binary = herdr_bin().ok_or("herdr CLI not found")?;
    let fail = |e: &dyn std::fmt::Display| format!("herdr {} failed: {e}", args[0]);
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(&e))?;
    // Drain both pipes on threads so a large `pane list` cannot block the child.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut out = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut out);
            }
            out
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| fail(&e))? {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(&"timed out"));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    if !status.success() {
        let stderr: String = stderr.trim().chars().take(300).collect();
        return Err(format!("herdr {} failed: {stderr}", args[..args.len().min(2)].join(" ")));
    }
    Ok(serde_json::from_str(&stdout).unwrap_or(Value::Object(Map::new())))
}

fn at<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    value.pointer(path).and_then(Value::as_str)
}

pub fn plugin_context() -> Map<String, Value> {
    env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Pane already running this session, if Herdr can tell.
fn running_pane(agent: &str, session_id: &str) -> Result<Option<String>, String> {
    let list = call(&["pane", "list"])?;
    let panes = list.pointer("/result/panes").and_then(Value::as_array).cloned().unwrap_or_default();
    for pane in &panes {
        let sess_agent = at(pane, "/agent_session/agent").or_else(|| at(pane, "/agent"));
        if at(pane, "/agent_session/value") == Some(session_id) && sess_agent == Some(agent) {
            return Ok(at(pane, "/pane_id").map(str::to_string));
        }
    }
    // Without a Herdr agent integration the session id is only visible in the process argv.
    for pane in &panes {
        let Some(pane_id) = at(pane, "/pane_id").filter(|_| at(pane, "/agent") == Some(agent)) else { continue };
        let Ok(info) = call(&["pane", "process-info", "--pane", pane_id]) else { continue };
        let procs = info.pointer("/result/process_info/foreground_processes").and_then(Value::as_array);
        let runs_it = procs.into_iter().flatten().any(|p| {
            p.get("argv").and_then(Value::as_array).is_some_and(|argv| argv.iter().any(|a| a.as_str() == Some(session_id)))
        });
        if runs_it {
            return Ok(Some(pane_id.to_string()));
        }
    }
    Ok(None)
}

/// `shlex.join`: quote each argument for a POSIX shell.
pub fn shell_join(argv: &[String]) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(safe) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r#"'"'"'"#))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Focus the pane already running the session, or start it in a new tab/split. Returns the pane id.
pub fn resume(
    plan: &ResumePlan,
    placement: Placement,
    workspace: Option<&str>,
    target_pane: Option<&str>,
) -> Result<String, String> {
    if let Some(existing) = running_pane(&plan.agent, &plan.session_id)? {
        call(&["agent", "focus", &existing])?;
        return Ok(existing);
    }
    let pane_id = match target_pane.filter(|_| placement != Placement::Tab) {
        None => {
            let title = if plan.title.is_empty() { &plan.agent } else { &plan.title };
            let label: String = title.chars().take(40).collect();
            let mut args = vec!["tab", "create", "--cwd", &plan.directory, "--label", &label, "--focus"];
            if let Some(ws) = workspace {
                args.extend(["--workspace", ws]);
            }
            at(&call(&args)?, "/result/root_pane/pane_id").map(str::to_string)
        }
        Some(target) => {
            let direction = if placement == Placement::SplitDown { "down" } else { "right" };
            let out = call(&["pane", "split", target, "--direction", direction, "--cwd", &plan.directory, "--focus"])?;
            at(&out, "/result/pane/pane_id").map(str::to_string)
        }
    };
    let pane_id = pane_id.ok_or("herdr did not return the new pane id")?;
    call(&["pane", "run", &pane_id, &shell_join(&plan.argv)])?;
    Ok(pane_id)
}

fn pane_file() -> Option<PathBuf> {
    env::var_os("HERDR_PLUGIN_STATE_DIR").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("browser-pane"))
}

/// Close the history pane opened by a previous action. False when none is open.
pub fn close_history() -> bool {
    let Some(file) = pane_file().filter(|f| f.is_file()) else { return false };
    let pane_id = std::fs::read_to_string(&file).unwrap_or_default().trim().to_string();
    let _ = std::fs::remove_file(&file);
    call(&["plugin", "pane", "close", &pane_id]).is_ok() // fails once the user has quit the browser
}

pub fn open_history(
    placement: HistoryPlacement,
    cwd: &str,
    env: &[(&str, String)],
    workspace: Option<&str>,
    target_pane: Option<&str>,
    size: &str,
) -> Result<(), String> {
    let placement_name = name(&placement);
    let mut args: Vec<String> = ["plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", ENTRYPOINT]
        .map(String::from)
        .into();
    args.extend(["--placement".into(), placement_name]);
    if let (HistoryPlacement::Split | HistoryPlacement::Zoomed, Some(target)) = (placement, target_pane) {
        args.extend(["--target-pane".into(), target.into()]);
    }
    if placement == HistoryPlacement::Split {
        args.extend(["--direction".into(), "right".into()]);
    }
    if placement == HistoryPlacement::Popup {
        args.extend(["--width".into(), size.into(), "--height".into(), size.into()]);
    }
    // Herdr rejects a workspace for overlays: they open on the active pane.
    if let (Some(ws), false) = (workspace, placement == HistoryPlacement::Overlay) {
        args.extend(["--workspace".into(), ws.into()]);
    }
    args.extend(["--cwd".into(), cwd.into()]);
    for (key, value) in env {
        args.extend(["--env".into(), format!("{key}={value}")]);
    }
    args.push("--focus".into());
    let out = call(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    // Popups have no pane id, so they cannot be toggled.
    if let (Some(pane_id), Some(file)) = (at(&out, "/result/plugin_pane/pane/pane_id"), pane_file()) {
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(file, pane_id);
    }
    Ok(())
}

/// Best effort.
pub fn notify(title: &str, body: &str) {
    let _ = call(&["notification", "show", title, "--body", body]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn pane_run_text_is_shell_quoted() {
        let argv = ["claude", "--resume", "a b", "it's", ""].map(String::from);
        assert_eq!(super::shell_join(&argv), r#"claude --resume 'a b' 'it'"'"'s' ''"#);
    }
}
