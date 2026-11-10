//! Terminal UI: a centered session list; Enter opens the conversation full-width.

#[cfg(test)]
mod app_tests;
mod conversation;
mod draw;
mod keys;
mod markdown;
mod theme;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
    KeyEventKind, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
use rusqlite::Connection;

use crate::config::{name, save_remembered, Config, ProjectFilter, Scope, Sort, THEMES};
use crate::git::locate;
use crate::indexer::{index_all, IndexStats};
use crate::model::sanitize;
use crate::resume::{build_plan, directory_for, needs_confirmation, ResumePlan};
use crate::search::{self, Filters, SessionRow};
use crate::{db, herdr};
use conversation::{highlight_pattern, Conversation};
use keys::{Key, Keymap};
use theme::Theme;

const SCOPES: [Scope; 4] = [Scope::User, Scope::Assistant, Scope::Both, Scope::Title];
const FILTERS: [ProjectFilter; 3] = [ProjectFilter::CurrentWorktree, ProjectFilter::Repository, ProjectFilter::All];
const SORTS: [Sort; 3] = [Sort::LastActivityDesc, Sort::CreatedDesc, Sort::TitleAsc];
const AGENTS: [&str; 7] = ["all", "claude", "codex", "opencode", "copilot", "pi", "hermes"];
const LIST_ONLY_ACTIONS: [&str; 8] = [
    "cycle_scope",
    "scope_user",
    "scope_assistant",
    "scope_both",
    "cycle_project_filter",
    "cycle_sort",
    "cycle_agent",
    "choose_project",
];
const DEBOUNCE: Duration = Duration::from_millis(120);
const TWICE: Duration = Duration::from_millis(1500);

pub fn scope_label(s: Scope) -> &'static str {
    match s {
        Scope::User => "my prompts",
        Scope::Assistant => "agent answers",
        Scope::Both => "prompts+answers",
        Scope::Title => "titles",
    }
}

pub fn filter_label(f: ProjectFilter) -> &'static str {
    match f {
        ProjectFilter::CurrentWorktree => "worktree",
        ProjectFilter::Repository => "repository",
        ProjectFilter::All => "all projects",
    }
}

pub fn sort_label(s: Sort) -> &'static str {
    match s {
        Sort::LastActivityDesc => "recent",
        Sort::CreatedDesc => "created",
        Sort::TitleAsc => "title",
    }
}

pub fn agent_filter_label(a: &str) -> &str {
    if a == "all" { "all agents" } else { a }
}

pub fn local_time(value: Option<&str>, fmt: &str) -> String {
    use std::fmt::Write;
    let Some(value) = value else { return "—".into() };
    let Ok(t) = DateTime::parse_from_rfc3339(value) else { return value.into() };
    let mut out = String::new();
    // An invalid strftime item in the config must not panic.
    match write!(out, "{}", t.with_timezone(&Local).format(fmt)) {
        Ok(()) => out,
        Err(_) => value.into(),
    }
}

/// Compact age of an ISO timestamp: 5m, 3h, 2d, 3w, 4mo, 1y.
pub fn ago(value: Option<&str>, now: DateTime<Utc>) -> String {
    let Some(value) = value else { return "—".into() };
    let Ok(t) = DateTime::parse_from_rfc3339(value) else { return "?".into() };
    let seconds = (now - t.to_utc()).num_seconds() as f64;
    let (minutes, hours, days) = (seconds / 60.0, seconds / 3600.0, seconds / 86400.0);
    match () {
        _ if minutes < 60.0 => format!("{}m", minutes.max(0.0) as i64),
        _ if hours < 24.0 => format!("{}h", hours as i64),
        _ if days < 14.0 => format!("{}d", days as i64),
        _ if days < 60.0 => format!("{}w", (days / 7.0) as i64),
        _ if days < 365.0 => format!("{}mo", (days / 30.0) as i64),
        _ => format!("{}y", (days / 365.0) as i64),
    }
}

/// Repository name (shared by all its worktrees), else the directory name.
pub fn project_name(row: &SessionRow) -> String {
    let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match (&row.repo, row.worktree.as_ref().or(row.cwd.as_ref())) {
        (Some(repo), _) => {
            let repo = Path::new(repo);
            match file_name(repo).as_str() {
                ".git" => repo.parent().map(file_name).unwrap_or_default(),
                other => other.strip_suffix(".git").unwrap_or(other).to_string(),
            }
        }
        (None, Some(path)) => file_name(Path::new(path)),
        (None, None) => String::new(),
    };
    if name.is_empty() { "—".into() } else { sanitize(&name) }
}

/// (label, root) options for the project picker: the current one first, then by recent activity.
fn project_labels(roots: &[String], here: &str) -> Vec<(String, String)> {
    let mut all = vec![here.to_string()];
    all.extend(roots.iter().filter(|r| *r != here).cloned());
    let base = |r: &str| {
        let name = Path::new(r.trim_end_matches('/')).file_name().map(|n| n.to_string_lossy().into_owned());
        name.unwrap_or_else(|| r.to_string())
    };
    let names: Vec<String> = all.iter().map(|r| base(r)).collect();
    all.iter()
        .zip(&names)
        .map(|(root, name)| {
            let label = if names.iter().filter(|n| *n == name).count() == 1 {
                name.clone()
            } else {
                let parent = Path::new(root).parent().map(|p| base(&p.to_string_lossy())).unwrap_or_default();
                format!("{name} ({parent})")
            };
            let label = if root == here { format!("{label} (here)") } else { label };
            (sanitize(&label), root.clone())
        })
        .collect()
}

#[derive(Default)]
pub struct Input {
    pub text: String,
    /// In characters.
    pub cursor: usize,
}

impl Input {
    fn byte(&self, char_index: usize) -> usize {
        self.text.char_indices().nth(char_index).map_or(self.text.len(), |(i, _)| i)
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = text.chars().count();
    }

    fn insert(&mut self, s: &str) {
        let at = self.byte(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    /// Editing keys; true if the text changed.
    fn edit(&mut self, key: &Key) -> bool {
        let len = self.text.chars().count();
        let before = self.text.clone();
        match key.code {
            KeyCode::Char(c) if !key.ctrl && !key.alt => self.insert(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
                self.text.replace_range(a..b, "");
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Char('e') if key.ctrl => self.cursor = len,
            KeyCode::Char('k') if key.ctrl => {
                let at = self.byte(self.cursor);
                self.text.truncate(at);
            }
            KeyCode::Char('w') if key.ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                let (a, b) = (self.byte(start), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor = start;
            }
            _ => {}
        }
        self.text != before
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Search,
    List,
    Conversation,
    Find,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Select {
    Scope,
    Agent,
    Filter,
    Project,
}

pub enum Modal {
    Theme { cursor: usize, current: String },
    Info(Box<SessionRow>),
    ResumeDir { row: Box<SessionRow>, current: String, cursor: usize, remember: bool },
    Confirm { message: String, row: Box<SessionRow>, directory: String },
    Help { scroll: usize },
    Dropdown { which: Select, options: Vec<(String, String)>, cursor: usize, anchor: Rect },
}

/// Clickable regions of the last frame.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    Search,
    Row(usize),
    Select(Select),
    Option(usize),
    Conversation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub severity: Severity,
    pub until: Instant,
}

pub struct App {
    pub config: Config,
    db: Connection,
    cwd: String,
    pub here: String,
    pub project_root: String,
    pub scope: Scope,
    pub agent: String,
    pub project_filter: ProjectFilter,
    pub sort: Sort,
    pub theme: Theme,
    pub keymap: Keymap,
    pub input: Input,
    pub find: Input,
    pub focus: Focus,
    pub reading: bool,
    pub finding: bool,
    return_to: Focus,
    pub rows: Vec<SessionRow>,
    pub selected: usize,
    pub list_offset: usize,
    pub list_height: usize,
    pub projects: Vec<(String, String)>,
    pub conv: Conversation,
    current_key: Option<String>,
    scroll_memory: HashMap<String, usize>,
    search_due: Option<Instant>,
    find_origin: usize,
    find_saved: (Option<regex::Regex>, isize),
    last_ctrl_c: Option<Instant>,
    last_escape: Option<Instant>,
    pub toasts: Vec<Toast>,
    pub modal: Option<Modal>,
    exit: Option<Option<ResumePlan>>,
    indexing: Option<Receiver<Result<IndexStats, String>>>,
    next_reindex: Option<Instant>,
    pub targets: Vec<(Rect, Target)>,
    pub size: (u16, u16),
}

pub struct Options {
    pub project_filter: Option<ProjectFilter>,
    pub scope: Option<Scope>,
    pub agent: Option<String>,
    pub query: String,
}

impl App {
    fn new(config: Config, cwd: String, opts: Options) -> Result<App, String> {
        let db = db::connect(&config.index.database_path()).map_err(|e| e.to_string())?;
        let r = &config.remembered;
        let here = locate(&cwd).worktree.unwrap_or_else(|| cwd.clone());
        let mut app = App {
            project_filter: opts.project_filter.or(r.project_filter).unwrap_or(config.search.default_project_filter),
            sort: r.sort.unwrap_or(config.search.sort),
            scope: opts.scope.or(r.scope).unwrap_or(config.search.default_scope),
            agent: opts.agent.or_else(|| r.agent.map(|a| name(&a))).unwrap_or_else(|| "all".into()),
            theme: theme::initial(&config),
            keymap: Keymap::new(config.keymap()),
            db,
            cwd,
            project_root: here.clone(),
            here,
            input: Input::default(),
            find: Input::default(),
            focus: Focus::List, // search starts inactive: "/" or a click enters it
            reading: false,
            finding: false,
            return_to: Focus::List,
            rows: vec![],
            selected: 0,
            list_offset: 0,
            list_height: 20,
            projects: vec![],
            conv: Conversation::default(),
            current_key: None,
            scroll_memory: HashMap::new(),
            search_due: None,
            find_origin: 0,
            find_saved: (None, -1),
            last_ctrl_c: None,
            last_escape: None,
            toasts: vec![],
            modal: None,
            exit: None,
            indexing: None,
            next_reindex: None,
            targets: vec![],
            size: (80, 24),
            config,
        };
        app.input.set(&opts.query);
        app.refresh_projects();
        app.reload(false);
        app.reindex();
        Ok(app)
    }

    pub fn notify(&mut self, text: impl Into<String>, severity: Severity, seconds: f32) {
        let until = Instant::now() + Duration::from_secs_f32(seconds);
        self.toasts.push(Toast { text: text.into(), severity, until });
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notify(text, Severity::Info, 1.5);
    }

    // ------------------------------------------------------------------ data

    fn refresh_projects(&mut self) {
        let roots = search::projects(&self.db).unwrap_or_default();
        self.projects = project_labels(&roots, &self.here);
    }

    fn filters(&self) -> (String, Filters) {
        let base = Filters {
            project: self.project_filter,
            cwd: Some(self.project_root.clone()),
            agent: (self.agent != "all").then(|| self.agent.clone()),
            ..Default::default()
        };
        search::split_filters(&self.input.text, &base)
    }

    fn reload(&mut self, keep_cursor: bool) {
        self.search_due = None;
        let (query, filters) = self.filters();
        let limit = self.config.search.limit as usize;
        let rows = if query.trim().is_empty() {
            search::list_sessions(&self.db, &filters, self.sort, limit)
        } else {
            search::search(&self.db, &query, self.scope, &filters, self.sort, limit)
        };
        let rows = rows.unwrap_or_else(|e| {
            self.notify(e.to_string(), Severity::Error, 6.0);
            vec![]
        });
        let keep = keep_cursor.then(|| self.current().map(|r| r.key.clone())).flatten();
        self.rows = rows;
        // A background refresh keeps the cursor on the same session; a new query starts at the top.
        self.selected = keep.and_then(|k| self.rows.iter().position(|r| r.key == k)).unwrap_or(0);
        if !keep_cursor {
            self.list_offset = 0;
        }
        if self.rows.is_empty() {
            self.current_key = None;
            self.conv.clear();
        }
    }

    pub fn current(&self) -> Option<&SessionRow> {
        self.rows.get(self.selected)
    }

    fn pattern(&self) -> Option<regex::Regex> {
        let (query, _) = self.filters();
        if query.trim().is_empty() || self.scope == Scope::Title { None } else { highlight_pattern(&query) }
    }

    pub fn conv_width(&self) -> usize {
        // padding 2 + bar 1 + pad 1 on the left, pad 1 + padding 2 + scrollbar 1 on the right
        (self.size.0 as usize).saturating_sub(8).max(10)
    }

    pub fn conv_height(&self) -> usize {
        (self.size.1 as usize).saturating_sub(2 + usize::from(self.finding)).max(1)
    }

    fn ensure_conv(&mut self) {
        self.conv.height = self.conv_height();
        let width = self.conv_width();
        self.conv.ensure(width, &self.theme);
    }

    fn open_current(&mut self, focus: bool) {
        let Some(row) = self.current().cloned() else { return };
        let same = self.current_key.as_deref() == Some(&row.key) && self.conv.session.is_some();
        if !same {
            if let Some(key) = &self.current_key {
                self.scroll_memory.insert(key.clone(), self.conv.scroll);
            }
            self.current_key = Some(row.key.clone());
            let messages = search::messages(&self.db, &row.key).unwrap_or_default();
            let pattern = self.pattern();
            self.conv.load(row.clone(), messages, pattern, &self.config.ui.date_format);
            self.ensure_conv();
            let remembered = self.scroll_memory.get(&row.key).copied().unwrap_or(0);
            self.conv.scroll_to(remembered);
        } else {
            let pattern = self.pattern();
            self.ensure_conv();
            self.conv.set_pattern(pattern);
        }
        self.conv.session = Some(row);
        if focus {
            self.reading = true;
            self.focus = Focus::Conversation;
            self.conv.cursor = None;
            if self.conv.pattern.is_some() {
                self.conv.jump(1, Some(0), true);
            }
        }
    }

    fn reindex(&mut self) {
        if self.indexing.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let config = self.config.clone();
        std::thread::spawn(move || {
            let result = db::connect(&config.index.database_path())
                .and_then(|conn| index_all(&conn, &config))
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.indexing = Some(rx);
        if self.config.index.auto_refresh {
            let every = Duration::from_secs(self.config.index.refresh_interval_seconds.max(5) as u64);
            self.next_reindex = Some(Instant::now() + every);
        }
    }

    fn after_reindex(&mut self) {
        let (key, at_bottom) = (self.current_key.clone(), self.conv.scroll + 1 >= self.conv.max_scroll());
        self.refresh_projects();
        self.reload(true);
        let Some(row) = self.current().cloned() else { return };
        let grew = self.conv.session.as_ref().is_some_and(|s| {
            row.user_count + row.assistant_count != s.user_count + s.assistant_count
        });
        if self.reading && key.as_deref() == Some(&row.key) && grew {
            let scroll = if at_bottom { usize::MAX } else { self.conv.scroll };
            self.scroll_memory.insert(row.key.clone(), scroll);
            self.current_key = None; // force re-render with the new messages
            self.open_current(false);
        }
    }

    fn remember(&mut self, key: &str, value: &str) {
        if let Err(e) = save_remembered(key, value, false) {
            self.notify(e, Severity::Error, 6.0);
            return;
        }
        let r = &mut self.config.remembered;
        let parsed = serde_json::Value::String(value.to_string());
        match key {
            "scope" => r.scope = serde_json::from_value(parsed).ok(),
            "agent" => r.agent = serde_json::from_value(parsed).ok(),
            "project_filter" => r.project_filter = serde_json::from_value(parsed).ok(),
            "sort" => r.sort = serde_json::from_value(parsed).ok(),
            "theme" => r.theme = Some(value.to_string()),
            "resume_directory" => r.resume_directory = serde_json::from_value(parsed).ok(),
            _ => {}
        }
    }

    fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        self.remember("scope", &name(&scope));
        self.reload(false);
    }

    fn set_agent(&mut self, agent: &str) {
        self.agent = agent.to_string();
        self.remember("agent", agent);
        self.reload(false);
    }

    fn set_project_filter(&mut self, filter: ProjectFilter) {
        self.project_filter = filter;
        self.remember("project_filter", &name(&filter));
        self.reload(false);
    }

    fn apply_theme(&mut self, choice: &str) {
        self.theme = Theme::new(&theme::resolve(choice), &self.config.colors);
        self.conv.invalidate();
    }

    // ------------------------------------------------------------------ list movement

    pub fn select(&mut self, index: usize) {
        if !self.rows.is_empty() {
            self.selected = index.min(self.rows.len() - 1);
        }
    }

    fn move_list(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    fn list_nav(&mut self, key: &Key) -> bool {
        let page = (self.list_height / 2).max(1) as isize;
        match key.code {
            KeyCode::Up => self.move_list(-1),
            KeyCode::Down => self.move_list(1),
            KeyCode::PageUp => self.move_list(-page),
            KeyCode::PageDown => self.move_list(page),
            KeyCode::Char('p') if key.ctrl => self.move_list(-1),
            KeyCode::Char('n') if key.ctrl => self.move_list(1),
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------------ keys

    fn handle_key(&mut self, key: Key) {
        if key.ctrl('c') {
            return self.ctrl_c();
        }
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        let action = self.keymap.action(&key).map(str::to_string);
        // Priority bindings fire even while typing (Tab, Ctrl+U/A); they do nothing while reading.
        if let Some(a) = action.as_deref().filter(|a| a.starts_with("scope_") || *a == "cycle_scope")
            && !self.reading
        {
            return self.run_action(a);
        }
        match self.focus {
            Focus::Find => match key.code {
                KeyCode::Enter => self.submit_find(),
                KeyCode::Esc => self.cancel_find(),
                _ if self.find.edit(&key) => {
                    self.ensure_conv();
                    let (text, origin) = (self.find.text.clone(), self.find_origin);
                    self.conv.find(&text, origin);
                }
                _ if key.char().is_none() => self.maybe_action(action.as_deref()),
                _ => {}
            },
            Focus::Search => {
                if self.list_nav(&key) {
                    return;
                }
                match key.code {
                    KeyCode::Enter => {
                        if self.search_due.is_some() {
                            self.reload(false); // typed and hit Enter before the debounce fired
                        }
                        self.return_to = Focus::Search;
                        self.open_current(true);
                    }
                    KeyCode::Esc => self.back(),
                    _ if self.input.edit(&key) => self.search_due = Some(Instant::now() + DEBOUNCE),
                    _ if key.char().is_none() => self.maybe_action(action.as_deref()),
                    _ => {}
                }
            }
            Focus::List => {
                if self.list_nav(&key) {
                    return;
                }
                match key.code {
                    KeyCode::Char('j') if key.char().is_some() => self.move_list(1),
                    KeyCode::Char('k') if key.char().is_some() => self.move_list(-1),
                    KeyCode::Char('g') | KeyCode::Home if !key.ctrl => self.select(0),
                    KeyCode::Char('G') | KeyCode::End if !key.ctrl => self.select(usize::MAX),
                    KeyCode::Enter => {
                        self.return_to = Focus::List;
                        self.open_current(true);
                    }
                    KeyCode::Esc => self.back(),
                    _ => self.maybe_action(action.as_deref()),
                }
            }
            Focus::Conversation => {
                self.ensure_conv();
                let page = self.conv.height.saturating_sub(2).max(1) as isize;
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down if !key.ctrl => self.conv.move_cursor(1),
                    KeyCode::Char('k') | KeyCode::Up if !key.ctrl => self.conv.move_cursor(-1),
                    KeyCode::Char('g') | KeyCode::Home if !key.ctrl => self.conv.focus_message(0),
                    KeyCode::Char('G') | KeyCode::End if !key.ctrl => {
                        let last = self.conv.messages.len().saturating_sub(1);
                        self.conv.focus_message(last);
                    }
                    KeyCode::PageUp => self.conv.scroll_by(-page),
                    KeyCode::PageDown => self.conv.scroll_by(page),
                    KeyCode::Esc => self.back(),
                    _ => self.maybe_action(action.as_deref()),
                }
            }
        }
    }

    fn maybe_action(&mut self, action: Option<&str>) {
        if let Some(a) = action {
            // Session-list search controls do nothing useful while reading.
            if !(self.reading && LIST_ONLY_ACTIONS.contains(&a)) {
                self.run_action(a);
            }
        }
    }

    fn run_action(&mut self, action: &str) {
        match action {
            "focus_search" => self.focus_search(),
            "cycle_scope" => {
                let i = SCOPES.iter().position(|s| *s == self.scope).unwrap_or(0);
                self.set_scope(SCOPES[(i + 1) % SCOPES.len()]);
            }
            "scope_user" => self.set_scope(Scope::User),
            "scope_assistant" => self.set_scope(Scope::Assistant),
            "scope_both" => self.set_scope(Scope::Both),
            "cycle_project_filter" => {
                let i = FILTERS.iter().position(|f| *f == self.project_filter).unwrap_or(0);
                self.set_project_filter(FILTERS[(i + 1) % FILTERS.len()]);
            }
            "choose_project" => {
                if self.project_filter == ProjectFilter::All {
                    // a project only narrows worktree/repository views
                    self.set_project_filter(ProjectFilter::Repository);
                }
                self.open_dropdown(Select::Project);
            }
            "cycle_agent" => {
                let i = AGENTS.iter().position(|a| *a == self.agent).unwrap_or(0);
                self.set_agent(AGENTS[(i + 1) % AGENTS.len()]);
            }
            "cycle_sort" => {
                let i = SORTS.iter().position(|s| *s == self.sort).unwrap_or(0);
                self.sort = SORTS[(i + 1) % SORTS.len()];
                self.remember("sort", &name(&self.sort));
                self.reload(false);
            }
            "choose_theme" => {
                let current = self.config.remembered.theme.clone().unwrap_or_else(|| self.config.ui.theme.clone());
                let cursor = THEMES.iter().position(|t| *t == current).map_or(0, |i| i + 1);
                self.modal = Some(Modal::Theme { cursor, current });
            }
            "resume" => self.resume(),
            "favorite" => {
                if let Some(key) = self.current().map(|r| r.key.clone()) {
                    match search::toggle_favorite(&self.db, &key) {
                        Ok(added) => self.info(if added { "Pinned" } else { "Unpinned" }),
                        Err(e) => self.notify(e.to_string(), Severity::Error, 6.0),
                    }
                    self.reload(true);
                }
            }
            "info" => {
                if let Some(row) = self.current().cloned() {
                    self.modal = Some(Modal::Info(Box::new(row)));
                }
            }
            "next_match" | "prev_match" => {
                self.ensure_conv();
                let delta = if action == "next_match" { 1 } else { -1 };
                if self.conv.jump(delta, None, true) {
                    if self.reading {
                        self.focus = Focus::Conversation;
                    }
                } else {
                    self.info("No matches (press / to search this session)");
                }
            }
            "mark_message" => {
                if let (Focus::Conversation, Some(i)) = (self.focus, self.conv.cursor) {
                    if !self.conv.marked.remove(&i) {
                        self.conv.marked.insert(i);
                    }
                    self.conv.move_cursor(1);
                }
            }
            "copy_message" => {
                let marked: Vec<usize> = self.conv.marked.iter().copied().collect();
                if !marked.is_empty() {
                    let text: Vec<String> = marked.iter().map(|&i| self.conv.as_markdown(i)).collect();
                    self.copy(&text.join("\n\n---\n\n"), &format!("{} messages", marked.len()));
                    self.conv.marked.clear();
                } else if let (Focus::Conversation, Some(i)) = (self.focus, self.conv.cursor) {
                    let text = self.conv.messages[i].text.clone();
                    self.copy(&text, "message");
                } else {
                    self.notify("Focus a message first (Enter, then j/k)", Severity::Warning, 3.0);
                }
            }
            "copy_conversation" => {
                if !self.conv.messages.is_empty() {
                    let all: Vec<String> = (0..self.conv.messages.len()).map(|i| self.conv.as_markdown(i)).collect();
                    self.copy(&all.join("\n\n"), "conversation");
                }
            }
            "refresh" => {
                self.notify("Re-indexing…", Severity::Info, 1.0);
                self.reindex();
            }
            "help" => self.modal = Some(Modal::Help { scroll: 0 }),
            "quit" => self.exit = Some(None),
            _ => {}
        }
    }

    fn copy(&mut self, text: &str, what: &str) {
        // OSC 52: works over SSH in Kitty/Ghostty/Herdr.
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.flush();
        self.info(format!("Copied {what}"));
    }

    fn ctrl_c(&mut self) {
        // Ctrl+C twice (within 1.5 s) quits from anywhere.
        if self.last_ctrl_c.is_some_and(|t| t.elapsed() < TWICE) {
            self.exit = Some(None);
            return;
        }
        self.last_ctrl_c = Some(Instant::now());
        self.info("Press Ctrl+C again to quit");
    }

    /// "/": the search box on the list, vim-style find inside the open conversation.
    fn focus_search(&mut self) {
        if !self.reading {
            self.focus = Focus::Search;
            return;
        }
        self.find_saved = (self.conv.pattern.clone(), self.conv.hit_index);
        self.find_origin = self.conv.cursor_index();
        self.find.set("");
        self.finding = true;
        self.focus = Focus::Find;
    }

    fn close_find(&mut self) {
        self.finding = false;
        self.focus = Focus::Conversation;
        self.ensure_conv();
    }

    fn submit_find(&mut self) {
        if !self.find.text.is_empty() && self.conv.hits.is_empty() {
            self.notify(format!("Pattern not found: {}", self.find.text), Severity::Warning, 2.0);
        }
        self.close_find();
        if self.conv.hit_index >= 0 {
            self.conv.cursor = Some(self.conv.hits[self.conv.hit_index as usize].msg);
        }
    }

    fn cancel_find(&mut self) {
        self.close_find();
        let (pattern, index) = self.find_saved.clone();
        self.conv.set_pattern(pattern);
        if index >= 0 && (index as usize) < self.conv.hits.len() {
            self.conv.hit_index = index;
            self.conv.show(index as usize, true);
        }
        if self.conv.hit_index < 0 && !self.conv.messages.is_empty() {
            let i = self.find_origin.min(self.conv.messages.len() - 1);
            self.conv.focus_message(i);
        }
    }

    /// Esc: cancel find → leave the conversation → clear the search at once → list. On the home
    /// page with an empty search, a second Esc within 1.5 s quits.
    fn back(&mut self) {
        if self.focus == Focus::Find {
            return self.cancel_find();
        }
        if self.reading {
            self.reading = false;
            self.focus = self.return_to;
            return;
        }
        if !self.input.text.is_empty() {
            // the whole query at once; quitting needs Esc on an already empty search
            self.input.set("");
            self.focus = Focus::Search;
            self.last_escape = None;
            self.reload(false);
            return;
        }
        if self.last_escape.is_some_and(|t| t.elapsed() < TWICE) {
            self.exit = Some(None);
            return;
        }
        self.focus = Focus::List; // single-key actions (r, f, q…) work there
        self.last_escape = Some(Instant::now());
        self.info("Press Esc again to quit");
    }

    // ------------------------------------------------------------------ dialogs

    fn open_dropdown(&mut self, which: Select) {
        let options: Vec<(String, String)> = match which {
            Select::Scope => SCOPES.iter().map(|s| (scope_label(*s).into(), name(s))).collect(),
            Select::Agent => AGENTS.iter().map(|a| (agent_filter_label(a).into(), a.to_string())).collect(),
            Select::Filter => FILTERS.iter().map(|f| (filter_label(*f).into(), name(f))).collect(),
            Select::Project => self.projects.clone(),
        };
        let current = match which {
            Select::Scope => name(&self.scope),
            Select::Agent => self.agent.clone(),
            Select::Filter => name(&self.project_filter),
            Select::Project => self.project_root.clone(),
        };
        let cursor = options.iter().position(|(_, v)| *v == current).unwrap_or(0);
        let anchor = self
            .targets
            .iter()
            .find(|(_, t)| matches!(t, Target::Select(s) if *s == which))
            .map_or(Rect::new(2, 3, 30, 1), |(r, _)| *r);
        self.modal = Some(Modal::Dropdown { which, options, cursor, anchor });
    }

    fn choose(&mut self, which: Select, value: String) {
        self.modal = None;
        match which {
            Select::Scope => {
                if let Ok(scope) = serde_json::from_value(serde_json::Value::String(value)) {
                    self.set_scope(scope);
                }
            }
            Select::Agent => self.set_agent(&value),
            Select::Filter => {
                if let Ok(filter) = serde_json::from_value(serde_json::Value::String(value)) {
                    self.set_project_filter(filter);
                }
            }
            Select::Project => {
                self.project_root = value;
                self.reload(false);
            }
        }
        self.focus = Focus::Search; // back to typing after picking
    }

    fn modal_key(&mut self, key: Key) {
        let Some(modal) = self.modal.take() else { return };
        let up = key.is(KeyCode::Up) || key.char() == Some('k');
        let down = key.is(KeyCode::Down) || key.char() == Some('j');
        let cancel = key.is(KeyCode::Esc) || key.char() == Some('q');
        let enter = key.is(KeyCode::Enter);
        match modal {
            Modal::Theme { mut cursor, current } => {
                let count = THEMES.len() + 1;
                let pick = |i: usize| if i == 0 { "auto".to_string() } else { THEMES[i - 1].to_string() };
                if cancel {
                    self.apply_theme(&current); // Esc undoes the preview
                } else if enter {
                    let choice = pick(cursor);
                    self.remember("theme", &choice);
                    self.apply_theme(&choice);
                } else {
                    if up {
                        cursor = (cursor + count - 1) % count;
                    } else if down {
                        cursor = (cursor + 1) % count;
                    }
                    self.apply_theme(&pick(cursor));
                    self.modal = Some(Modal::Theme { cursor, current });
                }
            }
            Modal::Info(row) => {
                if !(cancel || key.char() == Some('i')) {
                    self.modal = Some(Modal::Info(row));
                }
            }
            Modal::Help { mut scroll } => {
                if !(cancel || key.char() == Some('?')) {
                    if up {
                        scroll = scroll.saturating_sub(1);
                    } else if down {
                        scroll += 1;
                    }
                    self.modal = Some(Modal::Help { scroll });
                }
            }
            Modal::Confirm { message, row, directory } => {
                if key.char() == Some('y') || enter {
                    self.resume_in(&row, &directory);
                } else if !(key.char() == Some('n') || cancel) {
                    self.modal = Some(Modal::Confirm { message, row, directory });
                }
            }
            Modal::ResumeDir { row, current, mut cursor, mut remember } => {
                let which = match key.char() {
                    Some('o') => Some(0),
                    Some('c') => Some(1),
                    _ if enter => Some(cursor),
                    _ => None,
                };
                if cancel {
                    return;
                }
                if let Some(which) = which {
                    let directory = if which == 0 { row.cwd.clone().unwrap_or_default() } else { current.clone() };
                    if directory.is_empty() {
                        self.notify("The session has no recorded directory.", Severity::Error, 3.0);
                    } else {
                        if remember {
                            let policy = if which == 0 { "original" } else { "current" };
                            self.remember("resume_directory", policy);
                        }
                        return self.resume_in(&row, &directory);
                    }
                }
                if key.char() == Some('m') {
                    remember = !remember;
                }
                if up || down {
                    cursor = 1 - cursor;
                }
                self.modal = Some(Modal::ResumeDir { row, current, cursor, remember });
            }
            Modal::Dropdown { which, options, mut cursor, anchor } => {
                if enter {
                    if let Some((_, value)) = options.get(cursor).cloned() {
                        self.choose(which, value);
                    }
                } else if !cancel {
                    if up {
                        cursor = cursor.saturating_sub(1);
                    } else if down {
                        cursor = (cursor + 1).min(options.len().saturating_sub(1));
                    }
                    self.modal = Some(Modal::Dropdown { which, options, cursor, anchor });
                } else {
                    self.focus = Focus::Search;
                }
            }
        }
    }

    // ------------------------------------------------------------------ resume

    fn resume(&mut self) {
        let Some(row) = self.current().cloned() else { return };
        let current = locate(&self.cwd).worktree.unwrap_or_else(|| self.cwd.clone());
        let policy = self.config.remembered.resume_directory.unwrap_or(self.config.resume.directory);
        match directory_for(policy, &row, &current) {
            None => self.modal = Some(Modal::ResumeDir { row: Box::new(row), current, cursor: 0, remember: false }),
            Some(directory) if needs_confirmation(&row, &directory) => {
                let recorded = row.cwd.clone().unwrap_or_else(|| "an unknown directory".into());
                let message = format!("Resume in {directory}?\n\nThe session was recorded in {recorded}.");
                self.modal = Some(Modal::Confirm { message, row: Box::new(row), directory });
            }
            Some(directory) => self.resume_in(&row, &directory),
        }
    }

    fn resume_in(&mut self, row: &SessionRow, directory: &str) {
        let plan = match build_plan(row, directory) {
            Ok(plan) => plan,
            Err(e) => return self.notify(e, Severity::Error, 6.0),
        };
        if !herdr::inside_herdr() {
            self.exit = Some(Some(plan)); // the CLI execs the agent after the TUI restored the terminal
            return;
        }
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let workspace = env("HH_WORKSPACE").or_else(|| env("HERDR_WORKSPACE_ID"));
        let target = env("HH_TARGET_PANE").or_else(|| env("HERDR_PANE_ID"));
        // ponytail: blocks the UI for the herdr calls (15 s timeout each); a thread if it ever matters.
        match herdr::resume(&plan, self.config.resume.placement, workspace.as_deref(), target.as_deref()) {
            Err(e) => self.notify(e, Severity::Error, 8.0),
            Ok(_) if self.config.resume.close_history_on_resume => self.exit = Some(None),
            Ok(_) => self.notify(format!("Resumed {} session in Herdr", plan.agent), Severity::Info, 2.0),
        }
    }

    // ------------------------------------------------------------------ mouse

    fn target_at(&self, x: u16, y: u16) -> Option<Target> {
        let pos = Position::new(x, y);
        self.targets.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, t)| *t)
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let target = self.target_at(x, y);
        match kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta: isize = if kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                match &mut self.modal {
                    Some(Modal::Help { scroll }) => *scroll = scroll.saturating_add_signed(delta),
                    Some(Modal::Dropdown { options, cursor, .. }) => {
                        *cursor = cursor.saturating_add_signed(delta).min(options.len().saturating_sub(1));
                    }
                    Some(_) => {}
                    None if self.reading => {
                        self.ensure_conv();
                        self.conv.scroll_by(delta * 3);
                    }
                    None => self.move_list(delta),
                }
            }
            MouseEventKind::Moved => {
                // One highlight for mouse and keyboard: hovering moves the cursor.
                if let Some(Target::Row(i)) = target.filter(|_| self.modal.is_none()) {
                    self.select(i);
                }
                if let (Some(Modal::Dropdown { cursor, .. }), Some(Target::Option(i))) = (&mut self.modal, target) {
                    *cursor = i;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match (self.modal.is_some(), target) {
                (true, Some(Target::Option(i))) => {
                    if let Some(Modal::Dropdown { which, options, .. }) = &self.modal
                        && let Some((_, value)) = options.get(i).cloned()
                    {
                        let which = *which;
                        self.choose(which, value);
                    }
                }
                (true, _) => {
                    if matches!(self.modal, Some(Modal::Dropdown { .. })) {
                        self.modal = None;
                    }
                }
                (false, Some(Target::Row(i))) => {
                    self.select(i);
                    self.return_to = Focus::List;
                    self.open_current(true);
                }
                (false, Some(Target::Search)) => self.focus = Focus::Search,
                (false, Some(Target::Select(which))) => {
                    if !(which == Select::Project && self.project_filter == ProjectFilter::All) {
                        self.open_dropdown(which);
                    }
                }
                (false, Some(Target::Conversation)) => {
                    let area = self.targets.iter().find(|(_, t)| matches!(t, Target::Conversation)).map(|(r, _)| *r);
                    if let Some(area) = area
                        && let Some(i) = self.conv.message_at((y - area.y) as usize)
                    {
                        self.conv.cursor = Some(i);
                        self.focus = Focus::Conversation;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // ------------------------------------------------------------------ loop

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let typing = matches!(self.focus, Focus::Search | Focus::Find) && self.modal.is_none();
                self.handle_key(keys::normalize(k, typing));
            }
            Event::Mouse(m) => self.handle_mouse(m.kind, m.column, m.row),
            Event::Paste(text) => {
                let text = sanitize(&text).replace('\n', " ");
                match self.focus {
                    Focus::Search => {
                        self.input.insert(&text);
                        self.search_due = Some(Instant::now() + DEBOUNCE);
                    }
                    Focus::Find => {
                        self.find.insert(&text);
                        let (t, origin) = (self.find.text.clone(), self.find_origin);
                        self.ensure_conv();
                        self.conv.find(&t, origin);
                    }
                    _ => {}
                }
            }
            Event::Resize(w, h) => {
                self.size = (w, h);
                self.conv.invalidate();
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        if self.search_due.is_some_and(|t| Instant::now() >= t) {
            self.reload(false);
        }
        if let Some(rx) = &self.indexing {
            match rx.try_recv() {
                Ok(result) => {
                    self.indexing = None;
                    match result {
                        Ok(stats) if stats.parsed > 0 => self.after_reindex(),
                        Ok(_) => {}
                        Err(e) => self.notify(format!("Indexing failed: {e}"), Severity::Error, 6.0),
                    }
                }
                Err(TryRecvError::Disconnected) => self.indexing = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.next_reindex.is_some_and(|t| Instant::now() >= t) {
            self.reindex();
        }
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
    }
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

// Mouse tracking (all modes), focus events, bracketed paste off; cursor visible. Harmless if already off.
const TERMINAL_RESET: &str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[?25h";

fn reset_terminal() {
    let mut out = std::io::stdout();
    let _ = write!(out, "{TERMINAL_RESET}");
    let _ = out.flush();
}

/// Once the terminal is gone (window closed, pty revoked), crossterm 0.29 spins at 100% CPU inside
/// `event::poll`: its tty read loop gets EOF, never `WouldBlock`, and never returns. The main loop
/// then never sees `stop`, so this thread ends the process when the terminal disappears or when a
/// SIGTERM/SIGHUP is not honoured within a second.
fn watch_hangup(stop: Arc<AtomicBool>) {
    use std::io::IsTerminal;
    let has_tty = || std::io::stdin().is_terminal() || std::io::stdout().is_terminal();
    if !has_tty() {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if !has_tty() {
            std::process::exit(129); // as if killed by SIGHUP; there is no terminal left to restore
        }
        if stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1)); // the normal exit path restores the terminal
            std::process::exit(143);
        }
    });
}

pub fn run(config: Config, cwd: String, opts: Options) -> Result<Option<ResumePlan>, String> {
    let mut app = App::new(config, cwd, opts)?;
    let stop = Arc::new(AtomicBool::new(false));
    // SIGTERM (VS Code "Stop", kill) / SIGHUP (terminal closed): exit through the normal path so the
    // shell never gets the terminal back with mouse reporting still on.
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
        let _ = signal_hook::flag::register(signal, stop.clone());
    }
    watch_hangup(stop.clone());
    let mut terminal = ratatui::init();
    if let Ok(size) = ratatui::crossterm::terminal::size() {
        app.size = size;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        reset_terminal();
        previous(info);
    }));
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    if herdr::inside_herdr() {
        // Herdr shows the terminal title, else the process name.
        print!("\x1b]2;human-history\x07");
    }

    let result = (|| -> std::io::Result<()> {
        while app.exit.is_none() && !stop.load(Ordering::Relaxed) {
            terminal.draw(|frame| draw::draw(frame, &mut app))?;
            // Handle everything already queued before drawing again: a trackpad or key repeat
            // sends events faster than the terminal can show full frames.
            let mut wait = Duration::from_millis(100);
            while app.exit.is_none() && event::poll(wait)? {
                app.handle_event(event::read()?);
                wait = Duration::ZERO;
            }
            app.tick();
        }
        Ok(())
    })();

    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    reset_terminal();
    result.map_err(|e| e.to_string())?;
    Ok(app.exit.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ї".as_bytes()), "0Zc=");
    }

    #[test]
    fn ages() {
        let now = DateTime::parse_from_rfc3339("2026-11-10T12:00:00+00:00").unwrap().to_utc();
        assert_eq!(ago(Some("2026-11-10T11:55:00.000+00:00"), now), "5m");
        assert_eq!(ago(Some("2026-11-08T12:00:00.000+00:00"), now), "2d");
        assert_eq!(ago(Some("2026-10-01T12:00:00.000+00:00"), now), "5w");
        assert_eq!(ago(None, now), "—");
    }

    #[test]
    fn project_picker_labels() {
        let roots = ["/a/app".to_string(), "/b/app".to_string(), "/c/web".to_string()];
        let labels = project_labels(&roots, "/c/web");
        let labels: Vec<&str> = labels.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["web (here)", "app (a)", "app (b)"]);
    }
}
