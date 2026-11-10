//! Building and validating agent resume commands (no shell involved anywhere).

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::config::{absolute, expand, ResumeDirectory};
use crate::search::SessionRow;

static SAFE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$").unwrap());

#[derive(Clone, Debug)]
pub struct ResumePlan {
    pub agent: String,
    pub session_id: String,
    pub directory: String,
    pub argv: Vec<String>,
    pub title: String,
}

pub fn resume_argv(agent: &str, session_id: &str) -> Result<Vec<String>, String> {
    if !SAFE_ID.is_match(session_id) {
        return Err(format!("refusing to resume: unexpected session id {session_id:?}"));
    }
    let argv: Vec<String> = match agent {
        "claude" => vec!["claude".into(), "--resume".into(), session_id.into()],
        "codex" => vec!["codex".into(), "resume".into(), session_id.into()],
        "opencode" => vec!["opencode".into(), "--session".into(), session_id.into()],
        // The value is optional: `--resume <id>` may not bind.
        "copilot" => vec!["copilot".into(), format!("--resume={session_id}")],
        "pi" => vec!["pi".into(), "--session".into(), session_id.into()],
        _ => return Err(format!("resume is not supported for agent {agent:?}")),
    };
    Ok(argv)
}

/// `shutil.which`: the first executable `name` on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// Target directory for a fixed policy; `None` means the user must be asked.
pub fn directory_for(policy: ResumeDirectory, session: &SessionRow, current: &str) -> Option<String> {
    match policy {
        ResumeDirectory::Original => session.cwd.clone().filter(|c| !c.is_empty()),
        ResumeDirectory::Current => Some(current.to_string()),
        ResumeDirectory::Ask => None,
    }
}

/// `os.path.realpath`: symlinks resolved where the path exists.
pub fn realpath(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| absolute(Path::new(path)))
}

/// Resuming anywhere but the session's own directory is never done silently.
pub fn needs_confirmation(session: &SessionRow, directory: &str) -> bool {
    session.cwd.as_deref().filter(|c| !c.is_empty()).is_none_or(|cwd| realpath(directory) != realpath(cwd))
}

pub fn build_plan(session: &SessionRow, directory: &str) -> Result<ResumePlan, String> {
    let argv = resume_argv(&session.agent, &session.session_id)?;
    // An unknown directory means "here", like Python's Path("").
    let path = if directory.is_empty() { PathBuf::from(".") } else { expand(directory) };
    if !path.is_dir() {
        return Err(format!("directory does not exist: {}", path.display()));
    }
    if which(&argv[0]).is_none() {
        return Err(format!("{:?} CLI is not installed (not found on PATH)", argv[0]));
    }
    let directory = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(ResumePlan {
        agent: session.agent.clone(),
        session_id: session.session_id.clone(),
        directory: directory.to_string_lossy().into_owned(),
        argv,
        title: session.title.clone(),
    })
}

/// Replace this process with the agent. Returns only on failure.
pub fn exec_plan(plan: &ResumePlan) -> String {
    if let Err(e) = std::env::set_current_dir(Path::new(&plan.directory)) {
        return format!("cannot enter {}: {e}", plan.directory);
    }
    let err = Command::new(&plan.argv[0]).args(&plan.argv[1..]).exec();
    format!("cannot run {}: {err}", plan.argv[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_commands() {
        assert_eq!(resume_argv("claude", "abc-1").unwrap(), ["claude", "--resume", "abc-1"]);
        assert_eq!(resume_argv("codex", "abc-1").unwrap(), ["codex", "resume", "abc-1"]);
        assert_eq!(resume_argv("opencode", "ses_1").unwrap(), ["opencode", "--session", "ses_1"]);
        assert_eq!(resume_argv("copilot", "4444-dd").unwrap(), ["copilot", "--resume=4444-dd"]);
        assert_eq!(resume_argv("pi", "5555-ee").unwrap(), ["pi", "--session", "5555-ee"]);
        assert!(resume_argv("claude", "-rf; rm").is_err());
        assert!(resume_argv("claude", "../x").is_err());
        assert!(resume_argv("vim", "abc").is_err());
    }

    fn session(cwd: Option<&str>) -> SessionRow {
        let e = crate::testutil::env();
        crate::indexer::index_all(&e.db, &e.config).unwrap();
        let mut row = crate::search::get_session(&e.db, crate::testutil::CLAUDE_KEY).unwrap().unwrap();
        row.cwd = cwd.map(str::to_string);
        row
    }

    #[test]
    fn directory_policies_and_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_string_lossy().into_owned();
        let recorded = session(Some(&here));
        assert_eq!(directory_for(ResumeDirectory::Original, &recorded, "/elsewhere").as_deref(), Some(here.as_str()));
        assert_eq!(directory_for(ResumeDirectory::Current, &recorded, "/elsewhere").as_deref(), Some("/elsewhere"));
        assert_eq!(directory_for(ResumeDirectory::Ask, &recorded, "/elsewhere"), None);
        assert_eq!(directory_for(ResumeDirectory::Original, &session(None), "/elsewhere"), None);
        // the session's own directory (even spelled differently) is fine; anything else is confirmed
        assert!(!needs_confirmation(&recorded, &format!("{here}/./")));
        assert!(needs_confirmation(&recorded, "/elsewhere"));
        assert!(needs_confirmation(&session(None), &here));
    }

    #[test]
    fn build_plan_refuses_missing_directories_and_unsafe_ids() {
        let mut row = session(Some("/work/app"));
        assert!(build_plan(&row, "/definitely/missing").unwrap_err().contains("does not exist"));
        row.session_id = "x; rm -rf ~".into();
        assert!(build_plan(&row, "/").unwrap_err().contains("unexpected session id"));
        row.session_id = "abc".into();
        row.agent = "vim".into();
        assert!(build_plan(&row, "/").unwrap_err().contains("not supported"));
    }
}
