//! TUI behaviour driven through key events, rendered into an in-memory terminal.

use std::path::PathBuf;
use std::sync::MutexGuard;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use super::*;
use crate::config::load_config;
use crate::indexer::index_all;
use crate::testutil::*;

struct Harness {
    app: App,
    env: Env,
    config_file: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

/// An app over the indexed fixtures; TUI choices go to a temporary config, never the real one.
fn harness(opts: Options) -> Harness {
    let lock = lock_env();
    let env = env();
    index_all(&env.db, &env.config).unwrap();
    let config_file = env.root.join("config.toml");
    unsafe {
        std::env::set_var("HUMAN_HISTORY_CONFIG", &config_file);
        std::env::remove_var("HERDR_ENV"); // never drive a real Herdr from tests
    }
    let app = App::new(env.config.clone(), lossy(&env.root), opts).unwrap();
    Harness { app, env, config_file, _lock: lock }
}

fn all_projects() -> Options {
    Options { project_filter: Some(ProjectFilter::All), scope: None, agent: None, query: String::new() }
}

impl Harness {
    fn key(&mut self, code: KeyCode) -> &mut Self {
        self.app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        self
    }

    fn ctrl(&mut self, c: char) -> &mut Self {
        self.app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)));
        self
    }

    fn chars(&mut self, text: &str) -> &mut Self {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self
    }

    /// Typed text is debounced: run the pending search now.
    fn settle(&mut self) -> &mut Self {
        if self.app.search_due.is_some() {
            self.app.reload(false);
        }
        self
    }

    fn screen(&mut self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw::draw(frame, &mut self.app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn select_key(&mut self, key: &str) -> &mut Self {
        let i = self.app.rows.iter().position(|r| r.key == key).unwrap();
        self.app.select(i);
        self
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(&self.config_file).unwrap_or_default()
    }

    fn toasts(&self) -> String {
        self.app.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" | ")
    }
}

#[test]
fn browse_search_read_and_back_out() {
    let mut h = harness(all_projects());
    assert_eq!(h.app.rows.len(), 3);
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.screen(120, 30).contains("Approvals perf"));

    h.chars("/joinedload").settle();
    assert_eq!(h.app.focus, Focus::Search);
    assert_eq!(h.app.rows.len(), 1);
    let screen = h.screen(120, 30);
    assert!(screen.contains("agent: ") && screen.contains("joinedload"), "{screen}");

    h.key(KeyCode::Enter);
    assert!(h.app.reading);
    assert_eq!(h.app.conv.position(), (1, 1));
    let screen = h.screen(120, 30);
    let title = screen.lines().next().unwrap();
    assert!(title.contains("Approvals perf") && title.contains("[1/1]"), "{title}");
    assert!(screen.contains("USER") && screen.contains("CLAUDE"));
    for marker in FORBIDDEN {
        assert!(!screen.contains(marker));
    }

    // Esc: leave the conversation (back to the search box) → clear the query → list → quit
    h.key(KeyCode::Esc);
    assert!(!h.app.reading && h.app.focus == Focus::Search);
    h.key(KeyCode::Esc);
    assert!(h.app.input.text.is_empty() && h.app.rows.len() == 3);
    h.key(KeyCode::Esc);
    assert!(h.app.focus == Focus::List && h.toasts().contains("Esc again"));
    assert!(h.app.exit.is_none());
    h.key(KeyCode::Esc);
    assert!(matches!(h.app.exit, Some(None)));
}

#[test]
fn typing_in_the_search_box_never_triggers_shortcuts() {
    let mut h = harness(all_projects());
    h.chars("/qrtf").settle();
    assert!(h.app.exit.is_none() && h.app.modal.is_none());
    assert_eq!(h.app.input.text, "qrtf");
    // Cyrillic text stays Cyrillic while typing
    h.ctrl('w').chars("пошук");
    assert_eq!(h.app.input.text, "пошук");
}

#[test]
fn ctrl_c_twice_quits_from_anywhere() {
    let mut h = harness(all_projects());
    h.chars("/abc").ctrl('c');
    assert!(h.app.exit.is_none() && h.toasts().contains("Ctrl+C again"));
    h.ctrl('c');
    assert!(matches!(h.app.exit, Some(None)));
}

#[test]
fn shortcuts_work_on_a_cyrillic_layout() {
    let mut h = harness(all_projects());
    h.key(KeyCode::Char('ш')); // i
    assert!(matches!(h.app.modal, Some(Modal::Info(_))));
    h.key(KeyCode::Esc).key(KeyCode::Char('й')); // q
    assert!(matches!(h.app.exit, Some(None)));
}

#[test]
fn vim_search_inside_a_conversation() {
    let mut h = harness(all_projects());
    h.select_key(CLAUDE_KEY).key(KeyCode::Enter);
    assert!(h.app.reading && h.app.conv.pattern.is_none());

    h.chars("/approval");
    assert_eq!(h.app.focus, Focus::Find);
    let (_, total) = h.app.conv.position();
    assert!(total >= 3, "{total}");
    assert!(h.screen(120, 30).lines().any(|l| l.trim_start().starts_with("/ approval")));
    h.key(KeyCode::Enter);
    assert_eq!(h.app.focus, Focus::Conversation);
    assert_eq!(h.app.conv.position().0, 1);
    assert!(h.app.conv.cursor.is_some());
    h.key(KeyCode::Char('n'));
    assert_eq!(h.app.conv.position().0, 2);
    h.key(KeyCode::Char('N')).key(KeyCode::Char('N'));
    assert_eq!(h.app.conv.position(), (total as isize, total)); // wrapped backwards

    // a cancelled search restores the previous pattern and position
    h.chars("/zzz");
    assert_eq!(h.app.conv.position(), (0, 0));
    h.key(KeyCode::Esc);
    assert_eq!(h.app.conv.position(), (total as isize, total));
    assert_eq!(h.app.conv.pattern.as_ref().unwrap().as_str(), "(?i)approval");

    // smartcase: lowercase matches any case; a capital letter makes the search case-sensitive
    h.chars("/sqlalchemy").key(KeyCode::Enter);
    assert_eq!(h.app.conv.position(), (1, 1));
    h.chars("/SQLAlchemy").key(KeyCode::Enter);
    assert_eq!(h.app.conv.position(), (1, 1));
    assert!(!h.toasts().contains("not found"));
    h.chars("/Sqlalchemy").key(KeyCode::Enter);
    assert!(h.toasts().contains("Pattern not found: Sqlalchemy"));
}

#[test]
fn choices_are_remembered_unless_given_on_the_command_line() {
    let mut h = harness(all_projects());
    h.key(KeyCode::Tab); // both -> title
    assert_eq!(h.app.scope, Scope::Title);
    h.key(KeyCode::Char('a')).key(KeyCode::Char('s'));
    assert_eq!((h.app.agent.as_str(), h.app.sort), ("claude", Sort::CreatedDesc));
    let text = h.config_text();
    for line in ["scope = \"title\"", "agent = \"claude\"", "sort = \"created_desc\"", "project_filter"] {
        assert_eq!(text.contains(line), line != "project_filter", "{line} in\n{text}");
    }

    // only [remembered] from the written file: the paths and index must stay the test's own
    let mut config = h.env.config.clone();
    config.remembered = load_config(&h.config_file).unwrap().remembered;
    let remembered = App::new(config.clone(), lossy(&h.env.root), all_projects()).unwrap();
    assert_eq!((remembered.scope, remembered.agent.as_str()), (Scope::Title, "claude"));
    let given = Options { scope: Some(Scope::User), agent: Some("all".into()), ..all_projects() };
    let overridden = App::new(config, lossy(&h.env.root), given).unwrap();
    assert_eq!((overridden.scope, overridden.agent.as_str()), (Scope::User, "all"));
}

#[test]
fn list_only_controls_do_nothing_while_reading() {
    let mut h = harness(all_projects());
    h.key(KeyCode::Enter).key(KeyCode::Tab).key(KeyCode::Char('p'));
    assert_eq!((h.app.scope, h.app.project_filter), (Scope::Both, ProjectFilter::All));
    assert!(h.config_text().is_empty());
}

#[test]
fn theme_picker_previews_and_only_saves_on_enter() {
    let mut h = harness(all_projects());
    assert!(h.app.theme.bg.is_none()); // auto outside Herdr = the terminal's colours
    h.key(KeyCode::Char('t')).key(KeyCode::Down).key(KeyCode::Down); // auto -> terminal -> catppuccin
    assert!(h.app.theme.bg.is_some());
    h.key(KeyCode::Esc);
    assert!(h.app.theme.bg.is_none() && !h.config_text().contains("theme"));

    h.key(KeyCode::Char('t')).key(KeyCode::Down).key(KeyCode::Down).key(KeyCode::Enter);
    assert!(h.app.modal.is_none() && h.app.theme.bg.is_some());
    assert!(h.config_text().contains("theme = \"catppuccin\""));
}

#[test]
fn favorites_toggle_and_pin_to_the_top() {
    let mut h = harness(all_projects());
    let last = h.app.rows.last().unwrap().key.clone();
    h.select_key(&last).key(KeyCode::Char('f'));
    assert_eq!(h.app.rows[0].key, last);
    assert!(h.screen(120, 20).contains("★"));
    h.key(KeyCode::Char('f'));
    assert!(!h.app.rows[0].favorite && h.toasts().contains("Unpinned"));
}

#[test]
fn resume_asks_for_the_directory_and_returns_the_plan() {
    let mut h = harness(all_projects());
    let bin = h.env.root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    script(&bin.join("claude"), "exit 0");
    let path = std::env::var_os("PATH");
    unsafe { std::env::set_var("PATH", &bin) };

    // the recorded directory (/work/app) is gone: refused, the TUI stays open
    h.select_key(CLAUDE_KEY).key(KeyCode::Char('r'));
    assert!(matches!(h.app.modal, Some(Modal::ResumeDir { .. })));
    assert!(h.screen(120, 30).contains("(missing)"));
    h.key(KeyCode::Char('o'));
    assert!(h.app.exit.is_none() && h.toasts().contains("does not exist"));

    // "current", remembered: the next resume elsewhere only needs a confirmation
    h.key(KeyCode::Char('r')).key(KeyCode::Char('m')).key(KeyCode::Char('c'));
    assert!(h.config_text().contains("resume_directory = \"current\""));
    let plan = h.app.exit.take().flatten().expect("a resume plan");
    assert_eq!(plan.argv, ["claude", "--resume", "11111111-aaaa-4bbb-8ccc-000000000001"]);
    assert_eq!(PathBuf::from(&plan.directory), std::fs::canonicalize(&h.env.root).unwrap());

    h.key(KeyCode::Char('r'));
    assert!(matches!(h.app.modal, Some(Modal::Confirm { .. })));
    h.key(KeyCode::Char('n'));
    assert!(h.app.modal.is_none() && h.app.exit.is_none());

    match path {
        Some(p) => unsafe { std::env::set_var("PATH", p) },
        None => unsafe { std::env::remove_var("PATH") },
    }
}

#[test]
fn every_view_renders_at_any_size_without_panicking() {
    let mut h = harness(all_projects());
    h.app.notify("a long notification ".repeat(10), Severity::Error, 60.0);
    let sizes = [(20, 4), (21, 5), (24, 6), (30, 8), (50, 12), (80, 24), (200, 60)];
    let render_all = |h: &mut Harness| {
        for (w, height) in sizes {
            h.screen(w, height);
        }
    };
    render_all(&mut h);
    for modal in ["t", "i", "?", "r", "o"] {
        h.chars(modal);
        render_all(&mut h);
        h.key(KeyCode::Esc);
    }
    h.app.open_dropdown(Select::Scope);
    render_all(&mut h);
    h.key(KeyCode::Esc);
    h.app.project_filter = ProjectFilter::All; // "o" narrowed the list to this (empty) project
    h.app.reload(false);
    h.select_key(CLAUDE_KEY).key(KeyCode::Enter).chars("/approval");
    render_all(&mut h);
    h.key(KeyCode::Enter).key(KeyCode::Char('G')).key(KeyCode::Char(' '));
    render_all(&mut h);
    assert!(h.app.reading);
}
