//! TOML configuration (`~/.config/human-history/config.toml`), validated strictly.

use std::collections::BTreeMap;
use std::env;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Every option, with its default.
pub const EXAMPLE: &str = include_str!("config.example.toml");

/// Herdr's theme names (and order); `auto` follows Herdr inside Herdr.
pub const THEMES: [&str; 13] = [
    "terminal",
    "catppuccin",
    "catppuccin-latte",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
];

/// Action id -> comma-separated key names. Unlisted actions keep their defaults.
pub const KEY_ACTIONS: [(&str, &str); 21] = [
    ("focus_search", "slash"),
    ("cycle_scope", "tab"),
    ("scope_user", "ctrl+u"),
    ("scope_assistant", "ctrl+a"),
    ("scope_both", "ctrl+shift+a"),
    ("cycle_project_filter", "p"),
    ("choose_project", "o"),
    ("cycle_agent", "a"),
    ("cycle_sort", "s"),
    ("choose_theme", "t"),
    ("resume", "r"),
    ("favorite", "f"),
    ("info", "i"),
    ("next_match", "n"),
    ("prev_match", "N"),
    ("copy_message", "y"),
    ("copy_conversation", "Y"),
    ("mark_message", "space"),
    ("refresh", "ctrl+r"),
    ("help", "question_mark"),
    ("quit", "q"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    Assistant,
    #[default]
    Both,
    Title,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectFilter {
    CurrentWorktree,
    Repository,
    #[default]
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    LastActivityDesc,
    CreatedDesc,
    TitleAsc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentFilter {
    All,
    Claude,
    Codex,
    Opencode,
    Copilot,
    Pi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumeDirectory {
    #[default]
    Ask,
    Current,
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    #[default]
    Tab,
    SplitRight,
    SplitDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryPlacement {
    #[default]
    Overlay,
    Popup,
    Zoomed,
    Split,
    Tab,
}

/// The TOML spelling of an enum value (`"split-right"`, `"last_activity_desc"`, …).
pub fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// auto: Herdr's theme inside Herdr, otherwise "terminal" (the terminal's own colours).
    pub theme: String,
    pub date_format: String,
    // Accepted so the documented example validates; technical events are never displayed.
    pub show_tool_calls: bool,
    pub show_tool_results: bool,
    pub show_reasoning: bool,
    pub show_system_messages: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: "auto".into(),
            date_format: "%Y-%m-%d %H:%M".into(),
            show_tool_calls: false,
            show_tool_results: false,
            show_reasoning: false,
            show_system_messages: false,
        }
    }
}

/// Any CSS colour: "#rrggbb", "rgb(…)", or a name such as "teal".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorsConfig {
    pub selection: String,
    pub selection_accent: String,
    pub project: String,
    pub claude: String,
    pub codex: String,
    pub opencode: String,
    pub copilot: String,
    pub pi: String,
    #[serde(rename = "match")]
    pub match_: String,
    pub current_match: String,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        ColorsConfig {
            selection: "#4d2a22".into(),
            selection_accent: "#d97757".into(),
            project: "#e0a86e".into(),
            claude: "#d97757".into(),
            codex: "#10a37f".into(),
            opencode: "#5fafff".into(),
            copilot: "#a371f7".into(),
            pi: "#56b6c2".into(),
            match_: "#665c00".into(),
            current_match: "#ff9e3b".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    pub default_scope: Scope,
    pub default_project_filter: ProjectFilter,
    pub sort: Sort,
    pub limit: i64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            default_scope: Scope::Both,
            default_project_filter: ProjectFilter::Repository,
            sort: Sort::LastActivityDesc,
            limit: 200,
        }
    }
}

/// Choices made in the TUI; these override the configured defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RememberedConfig {
    pub scope: Option<Scope>,
    pub agent: Option<AgentFilter>,
    pub project_filter: Option<ProjectFilter>,
    pub sort: Option<Sort>,
    pub theme: Option<String>,
    pub resume_directory: Option<ResumeDirectory>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResumeConfig {
    pub directory: ResumeDirectory,
    /// Where Herdr opens the resumed agent.
    pub placement: Placement,
    pub close_history_on_resume: bool,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        ResumeConfig { directory: ResumeDirectory::Ask, placement: Placement::Tab, close_history_on_resume: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HerdrConfig {
    /// How plugin actions show the history browser.
    pub history_placement: HistoryPlacement,
    pub popup_size: String,
}

impl Default for HerdrConfig {
    fn default() -> Self {
        HerdrConfig { history_placement: HistoryPlacement::Overlay, popup_size: "90%".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexConfig {
    pub auto_refresh: bool,
    pub refresh_interval_seconds: i64,
    pub database: Option<String>,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig { auto_refresh: true, refresh_interval_seconds: 30, database: None }
    }
}

impl IndexConfig {
    pub fn database_path(&self) -> PathBuf {
        match &self.database {
            Some(db) => expand(db),
            None => xdg("XDG_DATA_HOME", ".local/share").join("human-history").join("index.sqlite3"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsConfig {
    pub claude: Vec<String>,
    pub codex: Vec<String>,
    /// The data dir, or opencode.db itself.
    pub opencode: Vec<String>,
    pub copilot: Vec<String>,
    pub pi: Vec<String>,
}

fn env_or(var: &str, default: &str) -> PathBuf {
    PathBuf::from(env::var(var).unwrap_or_else(|_| default.to_string()))
}

fn lossy(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

impl Default for PathsConfig {
    fn default() -> Self {
        let codex = env_or("CODEX_HOME", "~/.codex");
        let opencode = env::var("OPENCODE_DB")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| lossy(xdg("XDG_DATA_HOME", ".local/share").join("opencode")));
        let mut pi = vec![lossy(env_or("PI_CODING_AGENT_DIR", "~/.pi/agent").join("sessions"))];
        pi.extend(env::var("PI_CODING_AGENT_SESSION_DIR").ok().filter(|v| !v.is_empty()));
        PathsConfig {
            claude: vec![lossy(env_or("CLAUDE_CONFIG_DIR", "~/.claude").join("projects"))],
            codex: vec![lossy(codex.join("sessions")), lossy(codex.join("archived_sessions"))],
            opencode: vec![opencode],
            copilot: vec![lossy(env_or("COPILOT_HOME", "~/.copilot").join("session-state"))],
            pi,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub colors: ColorsConfig,
    pub search: SearchConfig,
    pub remembered: RememberedConfig,
    pub resume: ResumeConfig,
    pub index: IndexConfig,
    pub herdr: HerdrConfig,
    pub paths: PathsConfig,
    pub keys: BTreeMap<String, String>,
}

fn is_color(value: &str) -> bool {
    // ponytail: `ansi_*` names are accepted by prefix; tui::theme::parse_color maps the known ones.
    csscolorparser::parse(value).is_ok() || value.starts_with("ansi_")
}

fn check_theme(errors: &mut Vec<String>, field: &str, value: &str) {
    if value != "auto" && !THEMES.contains(&value) {
        errors.push(format!("{field}: unknown theme {value:?}; one of: auto, {}", THEMES.join(", ")));
    }
}

impl Config {
    /// Checks serde cannot express. Each error is `location: message`.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = vec![];
        check_theme(&mut errors, "ui.theme", &self.ui.theme);
        if let Some(theme) = &self.remembered.theme {
            check_theme(&mut errors, "remembered.theme", theme);
        }
        let ui = &self.ui;
        for (field, on) in [
            ("show_tool_calls", ui.show_tool_calls),
            ("show_tool_results", ui.show_tool_results),
            ("show_reasoning", ui.show_reasoning),
            ("show_system_messages", ui.show_system_messages),
        ] {
            if on {
                errors.push(format!("ui.{field}: must be false (technical events are never displayed)"));
            }
        }
        if let Ok(serde_json::Value::Object(colors)) = serde_json::to_value(&self.colors) {
            for (field, value) in colors {
                let value = value.as_str().unwrap_or_default();
                if !is_color(value) {
                    errors.push(format!("colors.{field}: not a colour: {value:?}"));
                }
            }
        }
        if !(10..=5000).contains(&self.search.limit) {
            errors.push("search.limit: must be between 10 and 5000".into());
        }
        if self.index.refresh_interval_seconds < 5 {
            errors.push("index.refresh_interval_seconds: must be at least 5".into());
        }
        let unknown: Vec<&str> =
            self.keys.keys().map(String::as_str).filter(|k| !KEY_ACTIONS.iter().any(|(a, _)| a == k)).collect();
        if !unknown.is_empty() {
            let valid: Vec<&str> = KEY_ACTIONS.iter().map(|(a, _)| *a).collect();
            errors.push(format!("keys: unknown key action(s) {unknown:?}; valid: {valid:?}"));
        }
        errors
    }
}

impl Config {
    /// Action -> comma-separated key names, defaults overridden by `[keys]`.
    pub fn keymap(&self) -> BTreeMap<String, String> {
        let mut map: BTreeMap<String, String> =
            KEY_ACTIONS.iter().map(|(a, k)| (a.to_string(), k.to_string())).collect();
        map.extend(self.keys.clone());
        map
    }
}

static TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[+[^\]]+\]+\s*(?:#.*)?$").unwrap());
static REMEMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[remembered\]\s*(?:#.*)?$").unwrap());
static TRAILING_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+#.*$").unwrap());

fn parse_checked(text: &str) -> Result<toml::Table, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    match config.validate().as_slice() {
        [] => Ok(table),
        errors => Err(errors.join("; ")),
    }
}

/// Write one TUI choice under `[remembered]` without rewriting unrelated TOML or losing
/// concurrent choices. Returns false when `only_if_missing` and the key is already set.
pub fn save_remembered(key: &str, value: &str, only_if_missing: bool) -> Result<bool, String> {
    let remembered: RememberedConfig = serde_json::from_value(serde_json::json!({ key: value }))
        .map_err(|_| format!("invalid remembered {key}: {value:?}"))?;
    if let Some(theme) = &remembered.theme {
        let mut errors = vec![];
        check_theme(&mut errors, "remembered.theme", theme);
        if !errors.is_empty() {
            return Err(format!("invalid remembered {key}: {value:?}"));
        }
    }

    let mut path = config_path();
    if path.is_symlink() {
        path = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}.lock", path.display()))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?; // released when `lock` is dropped

    let original = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let data = parse_checked(&original).map_err(|e| format!("{}: {e}", path.display()))?;
    let present = data.get("remembered").and_then(|r| r.as_table()).is_some_and(|r| r.contains_key(key));
    if only_if_missing && present {
        return Ok(false);
    }

    let assignment = format!("{key} = {}\n", serde_json::to_string(value).unwrap());
    let mut lines: Vec<String> = original.split_inclusive('\n').map(str::to_string).collect();
    let updated = match lines.iter().position(|l| REMEMBERED.is_match(l.trim())) {
        None => {
            let prefix = match () {
                _ if original.is_empty() => "",
                _ if original.ends_with('\n') => "\n",
                _ => "\n\n",
            };
            format!("{original}{prefix}[remembered]\n{assignment}")
        }
        Some(start) => {
            let end = (start + 1..lines.len()).find(|&i| TABLE.is_match(lines[i].trim())).unwrap_or(lines.len());
            let field = Regex::new(&format!(r"^\s*{}\s*=", regex::escape(key))).unwrap();
            match (start + 1..end).find(|&i| field.is_match(&lines[i])) {
                None => {
                    if !lines[start].ends_with('\n') {
                        lines[start].push('\n');
                    }
                    lines.insert(start + 1, assignment);
                }
                Some(i) => {
                    let old = lines[i].trim_end_matches(['\r', '\n']);
                    let comment = TRAILING_COMMENT.find(old).map_or("", |m| m.as_str());
                    lines[i] = format!("{}{comment}\n", assignment.trim_end());
                }
            }
            lines.concat()
        }
    };
    parse_checked(&updated).map_err(|e| format!("{}: {e}", path.display()))?;

    let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o600);
    let temp = dir.join(format!(".config.toml.{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, updated.as_bytes())?;
            f.set_permissions(std::fs::Permissions::from_mode(mode))
        })
        .and_then(|()| std::fs::rename(&temp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e.to_string());
    }
    Ok(true)
}

/// Move old index-database choices into the config without overriding existing choices.
pub fn migrate_legacy_preferences(conn: &rusqlite::Connection) -> Result<bool, String> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT key, value FROM kv WHERE key IN ('project_filter', 'sort', 'theme', 'resume_directory')")
        .and_then(|mut stmt| stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .map_err(|e| e.to_string())?;
    let mut changed = false;
    for (key, value) in &rows {
        changed |= save_remembered(key, value, true)?;
    }
    for (key, _) in &rows {
        conn.execute("DELETE FROM kv WHERE key = ?", [key]).map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

pub fn home() -> PathBuf {
    env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, default: &str) -> PathBuf {
    env::var(var).ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(default))
}

/// `Path(p).expanduser()`: `~` -> home; also drops trailing and doubled slashes.
pub fn expand(path: &str) -> PathBuf {
    let path = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    };
    path.components().collect()
}

/// `os.path.abspath`: absolute and lexically normalized, symlinks untouched.
pub fn absolute(path: &Path) -> PathBuf {
    let base = if path.is_absolute() { PathBuf::new() } else { env::current_dir().unwrap_or_default() };
    let mut out = PathBuf::new();
    for c in base.join(path).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn config_path() -> PathBuf {
    match env::var("HUMAN_HISTORY_CONFIG").ok().filter(|v| !v.is_empty()) {
        Some(p) => expand(&p),
        None => xdg("XDG_CONFIG_HOME", ".config").join("human-history").join("config.toml"),
    }
}

pub fn load_config(path: &Path) -> Result<Config, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let config: Config =
        toml::from_str(&text).map_err(|e| format!("{}: invalid configuration\n  {}", path.display(), e.to_string().trim()))?;
    let errors = config.validate();
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(format!("{}: invalid configuration\n  {}", path.display(), errors.join("\n  ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<Config, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        load_config(&path)
    }

    #[test]
    fn shipped_example_is_valid_and_lists_the_defaults() {
        let config = load(EXAMPLE).unwrap();
        let defaults = Config::default();
        assert_eq!(config.ui.theme, defaults.ui.theme);
        assert_eq!(config.search.default_project_filter, ProjectFilter::Repository);
        assert_eq!(config.keys.len(), KEY_ACTIONS.len());
        for (action, key) in KEY_ACTIONS {
            assert_eq!(config.keys[action], key);
        }
    }

    #[test]
    fn missing_file_gives_defaults() {
        let config = load_config(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config.search.limit, 200);
        assert_eq!(name(&config.resume.placement), "tab");
    }

    #[test]
    fn invalid_config_is_reported() {
        for (text, fragment) in [
            ("[ui]\nnope = 1\n", "nope"),
            ("[search]\nlimit = 3\n", "search.limit"),
            ("[search]\ndefault_scope = \"everything\"\n", "everything"),
            ("[ui]\ntheme = \"neon\"\n", "unknown theme"),
            ("[colors]\nmatch = \"not-a-colour\"\n", "colors.match"),
            ("[keys]\nfly = \"x\"\n", "unknown key action"),
            ("[ui\n", "invalid configuration"),
        ] {
            let err = load(text).unwrap_err();
            assert!(err.contains(fragment), "{text:?}: {err}");
        }
    }

    #[test]
    fn custom_colors() {
        let config = load("[colors]\nselection = \"teal\"\nmatch = \"rgb(77, 42, 34)\"\n").unwrap();
        assert_eq!(config.colors.selection, "teal");
    }

    #[test]
    fn save_remembered_keeps_unrelated_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# mine\n[ui]\ntheme = \"nord\"  # keep\n\n[remembered]\nsort = \"created_desc\"  # note\n[index]\nauto_refresh = false\n";
        std::fs::write(&path, original).unwrap();
        let _env = crate::testutil::lock_env();
        unsafe { env::set_var("HUMAN_HISTORY_CONFIG", &path) };
        assert!(save_remembered("sort", "title_asc", false).unwrap());
        assert!(save_remembered("scope", "user", false).unwrap());
        assert!(!save_remembered("scope", "title", true).unwrap());
        assert!(save_remembered("scope", "nope", false).is_err());
        assert!(save_remembered("bogus", "x", false).is_err());
        unsafe { env::remove_var("HUMAN_HISTORY_CONFIG") };
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# mine\n[ui]\ntheme = \"nord\"  # keep\n\n[remembered]\nscope = \"user\"\nsort = \"title_asc\"  # note\n[index]\nauto_refresh = false\n"
        );
        let config = load_config(&path).unwrap();
        assert_eq!(config.remembered.sort, Some(Sort::TitleAsc));
    }

    #[test]
    fn paths_normalize() {
        assert_eq!(expand("~/a//b/"), home().join("a/b"));
        assert_eq!(absolute(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    }
}
