//! Command line entry point: `human-history` (alias `hh`).

mod config;
mod db;
mod git;
mod herdr;
mod indexer;
mod model;
mod parsers;
mod resume;
mod search;
#[cfg(test)]
mod testutil;
mod tui;

use std::env;
use std::io::IsTerminal;
use std::path::Path;
use std::process::exit;

use clap::{Args, Parser, Subcommand};
use rusqlite::Connection;

use config::{config_path, load_config, Config, ProjectFilter, Scope, Sort, EXAMPLE};
use model::Agent;
use search::{Filters, HL_END, HL_START};
use tui::{local_time, project_name};

#[derive(Parser)]
#[command(name = "human-history", about = "Humanized, offline history of Claude Code and Codex sessions.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    #[command(flatten)]
    tui: TuiArgs,
}

/// Options not given come from [remembered] in the config.
#[derive(Args, Default)]
struct TuiArgs {
    /// current-worktree | repository | all (default: $HH_FILTER, then the config)
    #[arg(short = 'f', long = "filter", value_enum)]
    project_filter: Option<ProjectFilter>,
    #[arg(short, long, value_enum)]
    scope: Option<Scope>,
    /// all | claude | codex | opencode | copilot | pi
    #[arg(short, long, value_parser = ["all", "claude", "codex", "opencode", "copilot", "pi"])]
    agent: Option<String>,
    /// Initial search text
    #[arg(short, long, default_value = "")]
    query: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Browse and search sessions; resume one (the default command).
    Tui(TuiArgs),
    /// Incrementally index session logs.
    Index {
        /// Drop the index and parse everything again
        #[arg(long)]
        rebuild: bool,
    },
    /// Search prompts/answers/titles from the command line.
    Search {
        query: String,
        #[arg(short, long, value_enum, default_value_t = Scope::Both)]
        scope: Scope,
        #[arg(short = 'f', long = "filter", value_enum, default_value_t = ProjectFilter::All)]
        project_filter: ProjectFilter,
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        #[arg(long)]
        branch: Option<String>,
        /// ISO date, e.g. 2026-10-01
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        /// Only sessions started in exactly this directory
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long, value_enum, default_value_t = Sort::LastActivityDesc)]
        sort: Sort,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print a session as a clean Markdown conversation.
    Show {
        /// Session id (or unique prefix)
        session: String,
    },
    /// Resume the most recent session of the current worktree in its original directory.
    ResumeLast,
    /// Entry point for Herdr plugin actions (see herdr-plugin/herdr-plugin.toml).
    #[command(hide = true)]
    HerdrAction { action: String },
    /// Validate and print the effective configuration (or create it with --init).
    Config {
        /// Create the config file from the documented example
        #[arg(long)]
        init: bool,
        /// With --init: overwrite an existing file
        #[arg(long)]
        force: bool,
    },
}

fn fail(code: i32, message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    exit(code)
}

fn load() -> Config {
    load_config(&config_path()).unwrap_or_else(|e| fail(2, e))
}

fn open(config: &Config) -> Connection {
    let path = config.index.database_path();
    db::connect(&path).unwrap_or_else(|e| fail(1, format!("{}: {e}", path.display())))
}

fn cwd() -> String {
    env::var("HH_CWD")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().into_owned())
}

fn tui_cmd(args: TuiArgs) {
    let mut config = load();
    match config::migrate_legacy_preferences(&open(&config)) {
        Ok(true) => config = load(),
        Ok(false) => {}
        Err(e) => eprintln!("{e}"),
    }
    let project_filter = args.project_filter.or_else(|| {
        let env = env::var("HH_FILTER").ok().filter(|v| !v.is_empty())?;
        let parsed = serde_json::from_value(serde_json::Value::String(env.clone()));
        Some(parsed.unwrap_or_else(|_| {
            fail(2, format!("invalid filter {env:?}; choose from current-worktree, repository, all"))
        }))
    });
    let opts = tui::Options { project_filter, scope: args.scope, agent: args.agent, query: args.query };
    match tui::run(config, cwd(), opts) {
        Ok(Some(plan)) => {
            eprintln!("\x1b[2mResuming {} session in {}…\x1b[0m", plan.agent, plan.directory);
            fail(1, resume::exec_plan(&plan));
        }
        Ok(None) => {}
        Err(e) => fail(1, e),
    }
}

struct Style {
    on: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.on { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }
}

fn index(rebuild: bool) {
    let config = load();
    let path = config.index.database_path();
    if rebuild {
        for suffix in ["", "-wal", "-shm"] {
            let file = format!("{}{suffix}", path.display());
            match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => fail(1, format!("{file}: {e}")),
                _ => {}
            }
        }
    }
    let stats = indexer::index_all(&open(&config), &config).unwrap_or_else(|e| fail(1, e));
    println!(
        "scanned {} files, parsed {}, +{} messages, {} malformed lines skipped, {} sessions removed → {}",
        stats.scanned,
        stats.parsed,
        stats.messages,
        stats.skipped_lines,
        stats.removed_sessions,
        path.display()
    );
}

#[allow(clippy::too_many_arguments)]
fn search_cmd(
    query: &str,
    scope: Scope,
    project: ProjectFilter,
    agent: Option<Agent>,
    branch: Option<String>,
    since: Option<String>,
    until: Option<String>,
    exact_cwd: Option<String>,
    sort: Sort,
    limit: usize,
) {
    let config = load();
    let conn = open(&config);
    let filters = Filters {
        project,
        cwd: Some(cwd()),
        agent: agent.map(|a| a.as_str().to_string()),
        branch,
        since,
        until,
        exact_cwd: exact_cwd.map(|c| config::absolute(Path::new(&c)).to_string_lossy().into_owned()),
        favorites_only: false,
    };
    let rows = search::search(&conn, query, scope, &filters, sort, limit).unwrap_or_else(|e| fail(1, e));
    let style = Style { on: std::io::stdout().is_terminal() };
    if rows.is_empty() {
        println!("{}", style.paint("2", "no matches"));
        return;
    }
    let when: Vec<String> = rows.iter().map(|r| local_time(r.updated_at.as_deref(), &config.ui.date_format)).collect();
    let projects: Vec<String> = rows.iter().map(project_name).collect();
    let width = |cells: &[String]| cells.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    let (w_when, w_project) = (width(&when), width(&projects).min(24));
    for ((row, when), project) in rows.iter().zip(&when).zip(&projects) {
        let project: String = project.chars().take(w_project).collect();
        let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
        println!(
            "{}  {}  {}  {}",
            pad(when, w_when),
            pad(Agent::label(&row.agent), 8),
            style.paint("33", &pad(&project, w_project)),
            style.paint("1", &row.title)
        );
        if let Some(snippet) = &row.snippet {
            let mut line = String::new();
            for (i, part) in snippet.replace('\n', " ").split([HL_START, HL_END]).enumerate() {
                line += &if i % 2 == 1 { style.paint("1;7", part) } else { part.to_string() };
            }
            println!("{}  {line}", " ".repeat(w_when + 8 + w_project + 4));
        }
    }
}

fn show(session: &str) {
    let config = load();
    let conn = open(&config);
    let keys: Vec<String> = conn
        .prepare("SELECT key FROM sessions WHERE session_id LIKE ? || '%'")
        .and_then(|mut stmt| stmt.query_map([session], |r| r.get(0))?.collect())
        .unwrap_or_else(|e| fail(1, e));
    if keys.len() != 1 {
        fail(1, format!("{} session matching {session:?}", if keys.is_empty() { "no" } else { "ambiguous" }));
    }
    let row = search::get_session(&conn, &keys[0]).unwrap_or_else(|e| fail(1, e)).expect("key just listed");
    let style = Style { on: std::io::stdout().is_terminal() };
    println!("{}", style.paint("1;4", &row.title));
    let mut previous: Option<String> = None;
    for m in search::messages(&conn, &row.key).unwrap_or_else(|e| fail(1, e)) {
        if previous.as_deref() != Some(&m.role) {
            if m.role == "user" && previous.is_some() {
                println!("\n{}", style.paint("2", &"─".repeat(40)));
            }
            let (name, color) = match m.role.as_str() {
                "user" => ("USER".to_string(), "1;36"),
                _ => (Agent::label(&row.agent).to_uppercase(), "1;35"),
            };
            println!("\n{}", style.paint(color, &name));
        }
        // ponytail: raw Markdown; a renderer can come with the TUI port.
        println!("{}\n", m.text);
        previous = Some(m.role);
    }
}

fn resume_last(config: &Config, cwd: &str, workspace: Option<&str>, target: Option<&str>) -> Result<(), String> {
    let conn = open(config);
    indexer::index_all(&conn, config).map_err(|e| e.to_string())?;
    let filters = Filters { project: ProjectFilter::CurrentWorktree, cwd: Some(cwd.into()), ..Default::default() };
    let rows = search::list_sessions(&conn, &filters, Sort::LastActivityDesc, 1).map_err(|e| e.to_string())?;
    let row = rows.first().ok_or_else(|| format!("no sessions recorded for the worktree of {cwd}"))?;
    // Non-interactive: always the session's own directory, so nothing lands in an unexpected place.
    let plan = resume::build_plan(row, row.cwd.as_deref().unwrap_or(""))?;
    if herdr::inside_herdr() {
        herdr::resume(&plan, config.resume.placement, workspace, target).map(|_| ())
    } else {
        Err(resume::exec_plan(&plan))
    }
}

const HERDR_ACTIONS: [&str; 5] = ["open", "recent-here", "search", "resume-last", "refresh-index"];

fn herdr_action(action: &str) {
    if !HERDR_ACTIONS.contains(&action) {
        fail(2, format!("unknown action {action:?}"));
    }
    let config = load();
    let ctx = herdr::plugin_context();
    let ctx_str = |k: &str| ctx.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
    let env_str = |k: &str| env::var(k).ok().filter(|v| !v.is_empty());
    let cwd = ctx_str("focused_pane_cwd")
        .or_else(|| ctx_str("workspace_cwd"))
        .unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().into_owned());
    let workspace = ctx_str("workspace_id").or_else(|| env_str("HERDR_WORKSPACE_ID"));
    let target = ctx_str("focused_pane_id").or_else(|| env_str("HERDR_PANE_ID"));

    let result = match action {
        "refresh-index" => indexer::index_all(&open(&config), &config).map_err(|e| e.to_string()).map(|stats| {
            let body = format!("Indexed {} changed files, +{} messages", stats.parsed, stats.messages);
            herdr::notify("human-history", &body);
        }),
        "resume-last" => resume_last(&config, &cwd, workspace.as_deref(), target.as_deref()),
        _ if herdr::close_history() => Ok(()), // pressing the key again closes the browser
        _ => {
            let mut env = vec![("HH_CWD", cwd.clone())];
            env.extend(workspace.clone().map(|w| ("HH_WORKSPACE", w)));
            env.extend(target.clone().map(|t| ("HH_TARGET_PANE", t)));
            match action {
                "recent-here" => env.push(("HH_FILTER", "repository".into())),
                "search" => env.push(("HH_FILTER", "all".into())), // the search box is focused; search everything
                _ => {}
            }
            let h = &config.herdr;
            herdr::open_history(h.history_placement, &cwd, &env, workspace.as_deref(), target.as_deref(), &h.popup_size)
        }
    };
    if let Err(e) = result {
        herdr::notify("human-history", &e);
        fail(1, e);
    }
}

fn config_cmd(init: bool, force: bool) {
    let path = config_path();
    if init {
        if path.exists() && !force {
            fail(1, format!("{} already exists (use --force to overwrite)", path.display()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| fail(1, e));
        }
        std::fs::write(&path, EXAMPLE).unwrap_or_else(|e| fail(1, e));
        println!("created {} — edit it, then check with `hh config`", path.display());
        return;
    }
    let config = load();
    let found = if path.exists() { "found" } else { "not found, defaults" };
    println!("config file: {} ({found})", path.display());
    println!("database:    {}", config.index.database_path().display());
    println!("{}", serde_json::to_string_pretty(&config).unwrap());
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        None => tui_cmd(cli.tui),
        Some(Cmd::Tui(args)) => tui_cmd(args),
        Some(Cmd::Index { rebuild }) => index(rebuild),
        Some(Cmd::Search { query, scope, project_filter, agent, branch, since, until, cwd, sort, limit }) => {
            search_cmd(&query, scope, project_filter, agent, branch, since, until, cwd, sort, limit)
        }
        Some(Cmd::Show { session }) => show(&session),
        Some(Cmd::ResumeLast) => {
            let workspace = env::var("HERDR_WORKSPACE_ID").ok();
            let target = env::var("HERDR_PANE_ID").ok();
            if let Err(e) = resume_last(&load(), &cwd(), workspace.as_deref(), target.as_deref()) {
                fail(1, e);
            }
        }
        Some(Cmd::HerdrAction { action }) => herdr_action(&action),
        Some(Cmd::Config { init, force }) => config_cmd(init, force),
    }
}
