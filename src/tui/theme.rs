//! Themes named like Herdr's, so `auto` can follow the theme Herdr is using.
//!
//! `terminal` keeps the terminal's own colours and background; every other theme paints its own
//! background. Palettes follow the Textual built-ins the Python version used.

use ratatui::style::Color;

use crate::config::{ColorsConfig, Config, THEMES};

pub const TERMINAL: &str = "terminal";

/// (primary, accent, warning, error, foreground, background, surface, panel)
type Palette = [&'static str; 8];

fn palette(name: &str) -> Option<Palette> {
    // ponytail: hand-copied palettes; tweak here if a theme looks off.
    Some(match name {
        "catppuccin" => ["#F5C2E7", "#fab387", "#FAE3B0", "#F28FAD", "#cdd6f4", "#181825", "#313244", "#45475a"],
        "catppuccin-latte" => ["#8839EF", "#FE640B", "#DF8E1D", "#D20F39", "#4C4F69", "#EFF1F5", "#E6E9EF", "#CCD0DA"],
        "tokyo-night" => ["#BB9AF7", "#FF9E64", "#E0AF68", "#F7768E", "#a9b1d6", "#1A1B26", "#24283B", "#414868"],
        "tokyo-night-day" => ["#2e7de9", "#007197", "#b15c00", "#f52a65", "#3760bf", "#e1e2e7", "#d0d5e3", "#c4c8da"],
        "dracula" => ["#BD93F9", "#FF79C6", "#FFB86C", "#FF5555", "#F8F8F2", "#282A36", "#2B2E3B", "#313442"],
        "nord" => ["#88C0D0", "#B48EAD", "#EBCB8B", "#BF616A", "#D8DEE9", "#2E3440", "#3B4252", "#434C5E"],
        "gruvbox" => ["#85A598", "#fabd2f", "#fe8019", "#fb4934", "#fbf1c7", "#282828", "#3c3836", "#504945"],
        "gruvbox-light" => ["#b57614", "#427b58", "#af3a03", "#9d0006", "#3c3836", "#fbf1c7", "#ebdbb2", "#d5c4a1"],
        "one-dark" => ["#61AFEF", "#98C379", "#E5C07B", "#E06C75", "#ABB2BF", "#282C34", "#3B414D", "#4F5666"],
        "one-light" => ["#4078F2", "#50A14F", "#C18401", "#E45649", "#383A42", "#FAFAFA", "#F0F0F0", "#E0E0E0"],
        "solarized" => ["#268bd2", "#cb4b16", "#cb4b16", "#dc322f", "#839496", "#002b36", "#073642", "#0b4552"],
        "solarized-light" => ["#268bd2", "#cb4b16", "#cb4b16", "#dc322f", "#586e75", "#fdf6e3", "#eee8d5", "#e3dcc6"],
        _ => return None,
    })
}

/// Any colour the config accepts -> a terminal colour.
pub fn parse_color(value: &str) -> Color {
    if let Some(name) = value.strip_prefix("ansi_") {
        let bright = name.starts_with("bright_");
        return match (name.trim_start_matches("bright_"), bright) {
            ("black", false) => Color::Black,
            ("red", false) => Color::Red,
            ("green", false) => Color::Green,
            ("yellow", false) => Color::Yellow,
            ("blue", false) => Color::Blue,
            ("magenta", false) => Color::Magenta,
            ("cyan", false) => Color::Cyan,
            ("white", false) => Color::Gray,
            ("black", true) => Color::DarkGray,
            ("red", true) => Color::LightRed,
            ("green", true) => Color::LightGreen,
            ("yellow", true) => Color::LightYellow,
            ("blue", true) => Color::LightBlue,
            ("magenta", true) => Color::LightMagenta,
            ("cyan", true) => Color::LightCyan,
            _ => Color::White,
        };
    }
    match csscolorparser::parse(value) {
        Ok(c) => {
            let [r, g, b, _] = c.to_rgba8();
            Color::Rgb(r, g, b)
        }
        Err(_) => Color::Reset,
    }
}

fn rgb(c: Color) -> Option<(f32, f32, f32)> {
    match c {
        Color::Rgb(r, g, b) => Some((r as f32, g as f32, b as f32)),
        _ => None,
    }
}

/// `a` moved toward `b` by `t` (0..1); non-RGB colours stay as they are.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (rgb(a), rgb(b)) {
        (Some((ar, ag, ab)), Some((br, bg, bb))) => {
            let mix = |x: f32, y: f32| (x + (y - x) * t).round() as u8;
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => a,
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// `None`: the terminal's own background.
    pub bg: Option<Color>,
    pub fg: Color,
    pub muted: Color,
    pub surface: Color,
    pub panel: Color,
    pub accent: Color,
    pub warning: Color,
    pub error: Color,
    pub selection: Color,
    pub selection_accent: Color,
    pub project: Color,
    pub match_bg: Color,
    pub current_match_bg: Color,
    pub user: Color,
    agents: [(String, Color); 6],
}

impl Theme {
    pub fn agent(&self, agent: &str) -> Color {
        self.agents.iter().find(|(a, _)| a == agent).map_or(Color::White, |(_, c)| *c)
    }

    /// The theme for `name` (already resolved, see `resolve`) with the `[colors]` palette applied.
    pub fn new(name: &str, colors: &ColorsConfig) -> Theme {
        let c = |v: &str| parse_color(v);
        let defaults = ColorsConfig::default();
        let mut theme = Theme {
            bg: None,
            fg: Color::Reset,
            muted: Color::DarkGray,
            surface: Color::Reset,
            panel: Color::DarkGray,
            accent: Color::Rgb(0xff, 0xa6, 0x2b),
            warning: Color::Rgb(0xff, 0xa6, 0x2b),
            error: Color::Rgb(0xba, 0x3c, 0x5b),
            selection: c(&colors.selection),
            selection_accent: c(&colors.selection_accent),
            project: c(&colors.project),
            match_bg: c(&colors.match_),
            current_match_bg: c(&colors.current_match),
            user: Color::Cyan,
            agents: [
                ("claude".into(), c(&colors.claude)),
                ("codex".into(), c(&colors.codex)),
                ("opencode".into(), c(&colors.opencode)),
                ("copilot".into(), c(&colors.copilot)),
                ("pi".into(), c(&colors.pi)),
                ("hermes".into(), c(&colors.hermes)),
            ],
        };
        if let Some([primary, accent, warning, error, fg, bg, surface, panel]) = palette(name) {
            let (primary, bg) = (c(primary), c(bg));
            theme.bg = Some(bg);
            theme.fg = c(fg);
            theme.muted = blend(bg, theme.fg, 0.6);
            theme.surface = c(surface);
            theme.panel = c(panel);
            theme.accent = c(accent);
            theme.warning = c(warning);
            theme.error = c(error);
            // A value equal to the default (e.g. from `hh config --init`) still follows the theme.
            if colors.selection == defaults.selection {
                theme.selection = blend(bg, primary, 0.35);
            }
            if colors.selection_accent == defaults.selection_accent {
                theme.selection_accent = primary;
            }
        }
        theme
    }
}

/// The theme Herdr is configured with, when running inside Herdr and it is one we know.
fn herdr_theme() -> Option<String> {
    std::env::var("HERDR_ENV").ok().filter(|v| !v.is_empty())?;
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".config"));
    let text = std::fs::read_to_string(base.join("herdr/config.toml")).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    let name = table.get("theme")?.get("name")?.as_str()?;
    THEMES.contains(&name).then(|| name.to_string())
}

/// `auto` -> Herdr's theme inside Herdr, else `terminal`; unknown names -> `terminal`.
pub fn resolve(name: &str) -> String {
    match name {
        "auto" => herdr_theme().unwrap_or_else(|| TERMINAL.into()),
        n if THEMES.contains(&n) => n.into(),
        _ => TERMINAL.into(),
    }
}

pub fn initial(config: &Config) -> Theme {
    let choice = config.remembered.theme.as_deref().unwrap_or(&config.ui.theme);
    Theme::new(&resolve(choice), &config.colors)
}
