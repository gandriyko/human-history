//! The open conversation: rendered lines, message cursor, marks and vim-style match navigation.

use std::collections::BTreeSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use regex::Regex;

use super::markdown;
use super::theme::Theme;
use super::local_time;
use crate::model::Agent;
use crate::search::{MessageRow, SessionRow};

pub struct CLine {
    pub spans: Vec<Span<'static>>,
    pub msg: Option<usize>,
    /// Message text (searchable), as opposed to headers, separators and spacing.
    pub body: bool,
    /// The rule above a user turn.
    pub sep: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub msg: usize,
}

#[derive(Default)]
pub struct Conversation {
    pub session: Option<SessionRow>,
    pub messages: Vec<MessageRow>,
    date_format: String,
    pub lines: Vec<CLine>,
    msg_top: Vec<usize>,
    width: usize,
    dirty: bool,
    pub pattern: Option<Regex>,
    pub hits: Vec<Hit>,
    pub hit_index: isize,
    /// The message with the reading cursor (shown with a bar while the conversation has focus).
    pub cursor: Option<usize>,
    pub marked: BTreeSet<usize>,
    pub scroll: usize,
    pub height: usize,
}

/// Literal pattern with vim's smartcase: case-insensitive unless the text has a capital.
pub fn vim_pattern(text: &str) -> Option<Regex> {
    if text.is_empty() {
        return None;
    }
    let insensitive = if text.chars().any(char::is_uppercase) { "" } else { "(?i)" };
    Regex::new(&format!("{insensitive}{}", regex::escape(text))).ok()
}

/// Search terms of a list query (filters and quotes dropped), longest first, case-insensitive.
pub fn highlight_pattern(query: &str) -> Option<Regex> {
    let token = Regex::new(r#""[^"]+"|\S+"#).unwrap();
    let mut terms: Vec<String> = token
        .find_iter(query)
        .map(|m| m.as_str())
        .filter(|t| !t.trim_matches('"').contains(':'))
        .map(|t| t.trim_matches(['"', '*']).to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }
    terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    let alternatives: Vec<String> = terms.iter().map(|t| regex::escape(t)).collect();
    Regex::new(&format!("(?i){}", alternatives.join("|"))).ok()
}

impl Conversation {
    pub fn load(&mut self, session: SessionRow, messages: Vec<MessageRow>, pattern: Option<Regex>, date_format: &str) {
        *self = Conversation {
            session: Some(session),
            messages,
            date_format: date_format.to_string(),
            pattern,
            hit_index: -1,
            dirty: true,
            height: self.height,
            ..Default::default()
        };
    }

    pub fn clear(&mut self) {
        *self = Conversation { height: self.height, ..Default::default() };
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Lay the messages out for `width` columns of text (no-op when nothing changed).
    pub fn ensure(&mut self, width: usize, theme: &Theme) {
        if !self.dirty && self.width == width {
            return;
        }
        self.width = width;
        self.dirty = false;
        self.lines.clear();
        self.msg_top.clear();
        let agent = self.session.as_ref().map(|s| s.agent.clone()).unwrap_or_default();
        let line = |spans, msg, body, sep| CLine { spans, msg, body, sep };
        for (i, m) in self.messages.iter().enumerate() {
            let header = i == 0 || self.messages[i - 1].role != m.role;
            let user = m.role == "user";
            if header {
                self.lines.push(line(vec![], None, false, false));
            }
            self.msg_top.push(self.lines.len());
            if header && user {
                let rule = Span::styled("─".repeat(width + 3), Style::default().fg(theme.panel));
                self.lines.push(line(vec![rule], Some(i), false, true));
                self.lines.push(line(vec![], Some(i), false, false));
            }
            if header {
                let (name, color) = if user {
                    ("USER".to_string(), theme.user)
                } else {
                    (Agent::label(&agent).to_uppercase(), theme.agent(&agent))
                };
                let ts = local_time(m.ts.as_deref(), &self.date_format);
                let spans = vec![
                    Span::styled(name, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {ts}"), Style::default().fg(theme.muted)),
                ];
                self.lines.push(line(spans, Some(i), false, false));
                self.lines.push(line(vec![], Some(i), false, false));
            }
            for l in markdown::render(&m.text, width, theme) {
                self.lines.push(line(l.spans, Some(i), true, false));
            }
        }
        self.compute_hits();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn compute_hits(&mut self) {
        self.hits.clear();
        if let Some(pattern) = &self.pattern {
            for (n, l) in self.lines.iter().enumerate().filter(|(_, l)| l.body) {
                let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                for m in pattern.find_iter(&text).filter(|m| !m.is_empty()) {
                    self.hits.push(Hit { line: n, start: m.start(), end: m.end(), msg: l.msg.unwrap_or(0) });
                }
            }
        }
        if self.hit_index >= self.hits.len() as isize {
            self.hit_index = -1;
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.height.max(1))
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(self.max_scroll());
    }

    pub fn scroll_to(&mut self, line: usize) {
        self.scroll = line.min(self.max_scroll());
    }

    /// Highlight `pattern` everywhere and reset the n/N position.
    pub fn set_pattern(&mut self, pattern: Option<Regex>) {
        self.pattern = pattern;
        self.hit_index = -1;
        self.compute_hits();
    }

    pub fn focus_message(&mut self, index: usize) {
        if let Some(&top) = self.msg_top.get(index) {
            self.cursor = Some(index);
            self.scroll_to(top);
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let n = self.messages.len();
        if n == 0 {
            return;
        }
        let index = match self.cursor {
            Some(c) => c.saturating_add_signed(delta).min(n - 1),
            None if delta > 0 => 0,
            None => n - 1,
        };
        self.focus_message(index);
    }

    pub fn cursor_index(&self) -> usize {
        self.cursor.unwrap_or(0)
    }

    /// Go to the next (`delta=1`) / previous match occurrence, wrapping like vim. Without a
    /// current match, start from the first match in message `origin` or later (default: cursor).
    pub fn jump(&mut self, delta: isize, origin: Option<usize>, focus: bool) -> bool {
        let n = self.hits.len() as isize;
        if n == 0 {
            return false;
        }
        if self.hit_index < 0 || origin.is_some() {
            let start = origin.unwrap_or_else(|| self.cursor_index());
            let after = self.hits.iter().position(|h| h.msg >= start).map(|i| i as isize);
            self.hit_index = if delta >= 0 { after.unwrap_or(0) } else { after.map_or(-1, |a| a - 1) };
            self.hit_index = self.hit_index.rem_euclid(n);
        } else {
            self.hit_index = (self.hit_index + delta).rem_euclid(n);
        }
        self.show(self.hit_index as usize, focus);
        true
    }

    pub fn show(&mut self, index: usize, focus: bool) {
        let hit = self.hits[index];
        if focus {
            self.cursor = Some(hit.msg);
        }
        self.scroll_to(hit.line.saturating_sub(self.height / 3));
    }

    /// (current match number starting at 1, total matches); (0, total) before the first jump.
    pub fn position(&self) -> (isize, usize) {
        (self.hit_index + 1, self.hits.len())
    }

    /// Vim-style `/`: highlight and scroll to the first match at/after message `origin`.
    pub fn find(&mut self, text: &str, origin: usize) {
        self.set_pattern(vim_pattern(text));
        self.jump(1, Some(origin), false);
    }

    pub fn as_markdown(&self, index: usize) -> String {
        let m = &self.messages[index];
        let agent = self.session.as_ref().map_or("", |s| s.agent.as_str());
        let name = if m.role == "user" { "USER".to_string() } else { Agent::label(agent).to_uppercase() };
        format!("**{name}**\n\n{}", m.text)
    }

    /// The message drawn at a screen row of the viewport.
    pub fn message_at(&self, row: usize) -> Option<usize> {
        self.lines.get(self.scroll + row).and_then(|l| l.msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorsConfig;

    fn row() -> SessionRow {
        SessionRow {
            key: "claude:x".into(),
            agent: "claude".into(),
            session_id: "x".into(),
            title: "t".into(),
            cwd: None,
            worktree: None,
            repo: None,
            branch: None,
            created_at: None,
            updated_at: None,
            user_count: 2,
            assistant_count: 1,
            path: String::new(),
            favorite: false,
            hit_message_id: None,
            hit_role: None,
            snippet: None,
            hits: 0,
            match_ids: vec![],
        }
    }

    fn msg(role: &str, text: &str) -> MessageRow {
        MessageRow { role: role.into(), ts: None, text: text.into() }
    }

    #[test]
    fn vim_search_wraps_and_respects_smartcase() {
        let mut c = Conversation { height: 10, ..Default::default() };
        let messages = vec![msg("user", "retry it"), msg("assistant", "Retry with backoff; retry again"), msg("user", "no")];
        c.load(row(), messages, None, "%H:%M");
        c.ensure(40, &Theme::new("terminal", &ColorsConfig::default()));
        c.find("retry", 0);
        assert_eq!((c.position(), c.hits[0].msg), ((1, 3), 0));
        c.find("Retry", 1); // a capital makes it case-sensitive
        assert_eq!(c.position(), (1, 1));
        c.find("retry", 1);
        assert_eq!((c.position(), c.hits[c.hit_index as usize].msg), ((2, 3), 1));
        assert!(c.jump(1, None, true) && c.jump(1, None, true));
        assert_eq!(c.position(), (1, 3)); // wrapped around
        assert_eq!(c.cursor, Some(0));
        c.find("zzz", 0);
        assert_eq!(c.position(), (0, 0));
    }

    #[test]
    fn list_query_highlights_terms_not_filters() {
        let re = highlight_pattern("\"retry policy\" agent:codex idx*").unwrap();
        assert!(re.is_match("RETRY POLICY") && re.is_match("idx") && !re.is_match("codex"));
        assert!(highlight_pattern("agent:codex").is_none());
    }
}
