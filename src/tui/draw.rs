//! Rendering: home (search, header, session list), the conversation, footer, toasts and dialogs.

use chrono::Utc;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use super::markdown::{width, wrap};
use super::theme::{resolve, Theme};
use super::{
    ago, agent_filter_label, filter_label, local_time, project_name, scope_label, sort_label, App, Focus, Input,
    Modal, Select, Severity, Target,
};
use crate::config::{ProjectFilter, THEMES};
use crate::model::Agent;
use crate::resume::realpath;
use crate::search::{HL_END, HL_START};

const SEARCH_PLACEHOLDER: &str = "Search…  ↑↓ select · Enter open · Esc clear / to list (r resume, q quit)";
const FIND_PLACEHOLDER: &str = "search in this session (Enter: go, n/N: next/prev, Esc: cancel)";

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

/// Spans cut to `max` columns, ending in "…" when something was cut.
fn fit(spans: Vec<Span<'static>>, max: usize) -> Line<'static> {
    let total: usize = spans.iter().map(|s| width(&s.content)).sum();
    if total <= max {
        return Line::from(spans);
    }
    let mut out = vec![];
    let mut used = 0;
    for span in spans {
        let w = width(&span.content);
        if used + w < max {
            used += w;
            out.push(span);
            continue;
        }
        let mut cut = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if used + cw + 1 > max {
                break;
            }
            cut.push(ch);
            used += cw;
        }
        cut.push('…');
        out.push(Span::styled(cut, span.style));
        break;
    }
    Line::from(out)
}

fn put(buf: &mut Buffer, x: u16, y: u16, line: &Line, max: u16) {
    if y < buf.area.bottom() && x < buf.area.right() {
        buf.set_line(x, y, line, max.min(buf.area.right() - x));
    }
}

/// Split spans at byte ranges of their concatenated text and patch those parts with a style.
fn highlight(spans: &[Span<'static>], ranges: &[(usize, usize, Style)]) -> Vec<Span<'static>> {
    let mut out = vec![];
    let mut offset = 0;
    for span in spans {
        let text = span.content.as_ref();
        let (start, end) = (offset, offset + text.len());
        let mut pos = start;
        for &(a, b, style) in ranges.iter().filter(|(a, b, _)| *a < end && *b > start) {
            let (a, b) = (a.max(start), b.min(end));
            if a > pos {
                out.push(Span::styled(text[pos - start..a - start].to_string(), span.style));
            }
            out.push(Span::styled(text[a - start..b - start].to_string(), span.style.patch(style)));
            pos = b;
        }
        if pos < end {
            out.push(Span::styled(text[pos - start..].to_string(), span.style));
        }
        offset = end;
    }
    out
}

fn match_style(theme: &Theme, current: bool) -> Style {
    if current {
        Style::default().bg(theme.current_match_bg).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(theme.match_bg).fg(Color::White).add_modifier(Modifier::BOLD)
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.size = (area.width, area.height);
    app.targets.clear();
    let base = Style::default().fg(app.theme.fg).bg(app.theme.bg.unwrap_or(Color::Reset));
    frame.buffer_mut().set_style(area, base);
    if area.height < 4 || area.width < 20 {
        return;
    }
    if app.reading {
        draw_reading(frame, app);
    } else {
        draw_home(frame, app);
    }
    draw_footer(frame, app);
    draw_toasts(frame, app);
    draw_modal(frame, app);
}

fn draw_input(frame: &mut Frame, input: &Input, placeholder: &str, focused: bool, area: Rect, muted: Color) {
    let (x, y, w) = (area.x, area.y, area.width);
    let line = if input.text.is_empty() {
        Line::styled(placeholder.to_string(), fg(muted))
    } else {
        // keep the cursor visible in a long query
        let before: usize = input.text.chars().take(input.cursor).map(|c| c.width().unwrap_or(0)).sum();
        let skip = before.saturating_sub(w.saturating_sub(1) as usize);
        let mut used = 0;
        let visible: String = input
            .text
            .chars()
            .skip_while(|c| {
                used += c.width().unwrap_or(0);
                used <= skip
            })
            .collect();
        Line::raw(if skip > 0 { visible } else { input.text.clone() })
    };
    put(frame.buffer_mut(), x, y, &fit(line.spans, w as usize), w);
    if focused {
        let before: usize = input.text.chars().take(input.cursor).map(|c| c.width().unwrap_or(0)).sum();
        frame.set_cursor_position((x + (before as u16).min(w.saturating_sub(1)), y));
    }
}

fn draw_home(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let w = area.width.min(140);
    let x = area.x + (area.width - w) / 2 + 2;
    let w = w.saturating_sub(4);
    let theme = app.theme.clone();

    // search box
    let focused = app.focus == Focus::Search && app.modal.is_none();
    draw_input(frame, &app.input, SEARCH_PLACEHOLDER, focused, Rect::new(x, 1, w, 1), theme.muted);
    app.targets.push((Rect::new(x, 1, w, 1), Target::Search));

    // header: count + dropdowns
    let buf = frame.buffer_mut();
    let count = Line::from(vec![
        Span::styled(format!(" {} ", sort_label(app.sort)), fg(theme.muted)),
        Span::styled(app.rows.len().to_string(), fg(theme.muted).add_modifier(Modifier::BOLD)),
    ]);
    put(buf, x, 3, &count, w);
    let mut cx = x + (count.width() as u16).max(12) + 2;
    let project_label = app
        .projects
        .iter()
        .find(|(_, v)| *v == app.project_root)
        .map_or_else(|| app.project_root.clone(), |(l, _)| l.clone());
    let selects = [
        (Select::Scope, scope_label(app.scope).to_string(), 19),
        (Select::Agent, agent_filter_label(&app.agent).to_string(), 14),
        (Select::Filter, filter_label(app.project_filter).to_string(), 16),
        (Select::Project, project_label, 34),
    ];
    for (which, label, sw) in selects {
        let sw = sw.min((x + w).saturating_sub(cx));
        if sw < 4 {
            break;
        }
        // Picking a project only matters while filtering by worktree or repository.
        let disabled = which == Select::Project && app.project_filter == ProjectFilter::All;
        let style = if disabled { fg(theme.muted).add_modifier(Modifier::DIM) } else { Style::default() };
        put(buf, cx, 3, &fit(vec![Span::styled(label, style)], sw as usize - 2), sw - 2);
        put(buf, cx + sw - 1, 3, &Line::styled("▼", fg(theme.muted)), 1);
        app.targets.push((Rect::new(cx, 3, sw, 1), Target::Select(which)));
        cx += sw + 2;
    }

    // session list
    let top = 5;
    let height = area.height.saturating_sub(top + 1) as usize;
    app.list_height = height;
    if app.rows.is_empty() {
        put(buf, x + 1, top, &Line::styled("no sessions", fg(theme.muted)), w);
        return;
    }
    let row_h = |i: usize| if app.rows[i].snippet.is_some() { 2 } else { 1 };
    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    }
    while app.list_offset < app.selected && (app.list_offset..=app.selected).map(row_h).sum::<usize>() > height {
        app.list_offset += 1;
    }
    let agent_w = app.rows.iter().map(|r| r.agent.len()).max().unwrap_or(6) + 1;
    let names: Vec<String> = app.rows.iter().map(project_name).collect();
    let project_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(8).min(18);
    let now = Utc::now();
    let content_w = w.saturating_sub(2) as usize;
    let mut y = top;
    for (i, (r, name)) in app.rows.iter().zip(&names).enumerate().skip(app.list_offset) {
        if (y - top) as usize + row_h(i) > height {
            break;
        }
        let dim = fg(theme.muted);
        let mut project = name.clone();
        if project.chars().count() > project_w {
            project = project.chars().take(project_w - 1).collect::<String>() + "…";
        }
        let mut spans = vec![
            Span::styled(format!("{:>4} ", ago(r.updated_at.as_deref(), now)), dim),
            Span::styled("● ", fg(theme.agent(&r.agent))),
            Span::styled(format!("{:<agent_w$}", r.agent), dim),
            Span::styled(format!("{project:<project_w$}  "), fg(theme.project)),
        ];
        if r.favorite {
            spans.push(Span::styled("★ ", fg(Color::Yellow).add_modifier(Modifier::BOLD)));
        }
        spans.push(Span::styled(r.title.clone(), if r.title == "(untitled)" { dim } else { Style::default() }));
        if r.hits > 0 {
            spans.push(Span::styled(format!("  ({})", r.hits), dim));
        }
        let mut lines = vec![fit(spans, content_w)];
        if let Some(snippet) = &r.snippet {
            let mut spans = vec![
                Span::raw(" ".repeat(9 + agent_w + project_w)),
                Span::styled(if r.hit_role.as_deref() == Some("user") { "you: " } else { "agent: " }, fg(Color::Cyan).add_modifier(Modifier::DIM)),
            ];
            for (k, part) in snippet.replace('\n', " ").split([HL_START, HL_END]).enumerate() {
                let style = if k % 2 == 1 { match_style(&theme, false) } else { Style::default() };
                spans.push(Span::styled(part.to_string(), style));
            }
            lines.push(fit(spans, content_w));
        }
        let rect = Rect::new(x, y, w, lines.len() as u16);
        if i == app.selected {
            buf.set_style(rect, Style::default().bg(theme.selection).add_modifier(Modifier::BOLD));
        }
        for (k, line) in lines.iter().enumerate() {
            put(buf, x + 1, y + k as u16, line, w.saturating_sub(2));
        }
        app.targets.push((rect, Target::Row(i)));
        y += lines.len() as u16;
    }
}

fn draw_reading(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.ensure_conv();
    let theme = app.theme.clone();
    let conv = &app.conv;

    // title line
    if let Some(row) = &conv.session {
        let mut text = format!("{}  ·  {}  ·  {} prompts", row.title, Agent::label(&row.agent), row.user_count);
        if conv.pattern.is_some() {
            let (current, total) = conv.position();
            text += &if total > 0 { format!("  ·  [{current}/{total}]") } else { "  ·  no matches".into() };
        }
        text += "  ·  / search  n/N next/prev  Esc back";
        let line = fit(vec![Span::styled(text, fg(theme.accent).add_modifier(Modifier::BOLD))], area.width as usize - 2);
        put(frame.buffer_mut(), 1, 0, &line, area.width - 2);
    }

    let height = app.conv_height() as u16;
    let body = Rect::new(0, 1, area.width, height);
    app.targets.push((body, Target::Conversation));
    let buf = frame.buffer_mut();
    if conv.messages.is_empty() {
        let line = Line::styled("No human-readable messages in this session.", fg(theme.muted));
        put(buf, 4, 3, &line, area.width.saturating_sub(6));
    }
    let text_w = app.conv_width() as u16;
    let focused = app.focus == Focus::Conversation;
    let mut hit_iter = conv.hits.iter().enumerate().peekable();
    while hit_iter.peek().is_some_and(|(_, h)| h.line < conv.scroll) {
        hit_iter.next();
    }
    for row in 0..height {
        let index = conv.scroll + row as usize;
        let Some(line) = conv.lines.get(index) else { break };
        let y = body.y + row;
        let mut ranges = vec![];
        while let Some((k, hit)) = hit_iter.peek().filter(|(_, h)| h.line == index) {
            ranges.push((hit.start, hit.end, match_style(&theme, *k as isize == conv.hit_index)));
            hit_iter.next();
        }
        if line.sep {
            put(buf, 2, y, &Line::from(line.spans.clone()), text_w + 3);
            continue;
        }
        if let Some(i) = line.msg {
            let bar = if conv.marked.contains(&i) {
                Some(theme.warning)
            } else if focused && conv.cursor == Some(i) {
                Some(theme.selection_accent)
            } else {
                None
            };
            if let Some(color) = bar {
                put(buf, 2, y, &Line::styled("▌", fg(color)), 1);
            }
        }
        let spans = if ranges.is_empty() { line.spans.clone() } else { highlight(&line.spans, &ranges) };
        put(buf, 4, y, &Line::from(spans), text_w);
    }

    // scrollbar
    let total = conv.lines.len();
    let h = height as usize;
    if total > h && h > 0 {
        let x = area.width - 1;
        let thumb = (h * h / total).max(1);
        let pos = conv.scroll * (h - thumb) / conv.max_scroll().max(1);
        for row in 0..h {
            let color = if (pos..pos + thumb).contains(&row) { theme.selection_accent } else { theme.selection };
            buf.set_string(x, body.y + row as u16, " ", Style::default().bg(color));
        }
    }

    if app.finding {
        let y = area.height - 2;
        put(frame.buffer_mut(), 1, y, &Line::styled("/", bold()), 1);
        let focused = app.focus == Focus::Find && app.modal.is_none();
        draw_input(frame, &app.find, FIND_PLACEHOLDER, focused, Rect::new(3, y, area.width - 4, 1), theme.muted);
    }
}

fn draw_footer(frame: &mut Frame, app: &App) {
    let entries: Vec<(&str, &str)> = match (&app.modal, app.focus) {
        (Some(Modal::ResumeDir { .. }), _) => {
            return footer_line(frame, app, &[("o", "Original"), ("c", "Current"), ("m", "Remember"), ("esc", "Cancel")]);
        }
        (Some(Modal::Confirm { .. }), _) => return footer_line(frame, app, &[("y", "Yes"), ("n", "No")]),
        (Some(Modal::Dropdown { .. }), _) => vec![],
        (Some(_), _) => return footer_line(frame, app, &[("esc", "Close")]),
        (None, Focus::Find) => vec![],
        (None, Focus::Search) => vec![("cycle_scope", "Scope")],
        (None, _) if app.reading => vec![
            ("focus_search", "Search"),
            ("resume", "Resume"),
            ("favorite", "Fav"),
            ("info", "Info"),
            ("help", "Help"),
            ("quit", "Quit"),
        ],
        (None, _) => vec![
            ("focus_search", "Search"),
            ("cycle_scope", "Scope"),
            ("cycle_project_filter", "Worktree/repo/all"),
            ("choose_project", "Project"),
            ("resume", "Resume"),
            ("favorite", "Fav"),
            ("info", "Info"),
            ("help", "Help"),
            ("quit", "Quit"),
        ],
    };
    let labels: Vec<(String, &str)> = entries.iter().map(|(a, d)| (app.keymap.label(a), *d)).collect();
    let labels: Vec<(&str, &str)> = labels.iter().map(|(k, d)| (k.as_str(), *d)).collect();
    footer_line(frame, app, &labels);
}

fn footer_line(frame: &mut Frame, app: &App, entries: &[(&str, &str)]) {
    let area = frame.area();
    let mut spans = vec![Span::raw(" ")];
    for (key, desc) in entries {
        spans.push(Span::styled(key.to_string(), fg(app.theme.accent).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(format!(" {desc}  ")));
    }
    put(frame.buffer_mut(), 0, area.height - 1, &fit(spans, area.width as usize), area.width);
}

fn surface(theme: &Theme) -> Color {
    if theme.bg.is_some() { theme.surface } else { Color::Indexed(236) }
}

fn draw_toasts(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let mut y = area.height.saturating_sub(2);
    for toast in app.toasts.iter().rev() {
        if y < 4 {
            break;
        }
        let w = (width(&toast.text) as u16 + 4).min(area.width.saturating_sub(4)).min(60);
        let rect = Rect::new(area.width - w - 2, y - 3, w, 3);
        let color = match toast.severity {
            Severity::Info => app.theme.accent,
            Severity::Warning => app.theme.warning,
            Severity::Error => app.theme.error,
        };
        let buf = frame.buffer_mut();
        Widget::render(Clear, rect, buf);
        buf.set_style(rect, Style::default().bg(surface(&app.theme)).fg(app.theme.fg));
        for row in 0..3 {
            buf.set_string(rect.x, rect.y + row, "▌", fg(color));
        }
        put(buf, rect.x + 2, rect.y + 1, &fit(vec![Span::raw(toast.text.clone())], w as usize - 3), w - 3);
        y -= 4;
    }
}

/// A centered dialog of `w` x `h` (inside its border); returns the content area.
fn dialog(frame: &mut Frame, theme: &Theme, w: u16, h: u16, padding: (u16, u16), title: Option<&str>) -> Rect {
    let area = frame.area();
    let w = (w + 2 + 2 * padding.1).min(area.width * 95 / 100);
    let h = (h + 2 + 2 * padding.0).min(area.height.saturating_sub(2));
    let rect = Rect::new((area.width - w) / 2, (area.height.saturating_sub(h)) / 2, w, h);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(theme.accent))
        .style(Style::default().bg(surface(theme)).fg(theme.fg));
    if let Some(title) = title {
        block = block.title(Span::styled(format!(" {title} "), bold()));
    }
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    Rect::new(
        inner.x + padding.1,
        inner.y + padding.0,
        inner.width.saturating_sub(2 * padding.1),
        inner.height.saturating_sub(2 * padding.0),
    )
}

fn option_lines(frame: &mut Frame, app: &mut App, area: Rect, items: &[Line<'static>], cursor: usize, clickable: bool) {
    let theme = app.theme.clone();
    let visible = area.height as usize;
    let first = cursor.saturating_sub(visible.saturating_sub(1));
    for (row, (i, line)) in items.iter().enumerate().skip(first).take(visible).enumerate() {
        let rect = Rect::new(area.x, area.y + row as u16, area.width, 1);
        if i == cursor {
            frame.buffer_mut().set_style(rect, Style::default().bg(theme.selection).add_modifier(Modifier::BOLD));
        }
        put(frame.buffer_mut(), rect.x + 1, rect.y, &fit(line.spans.clone(), area.width as usize - 2), area.width - 2);
        if clickable {
            app.targets.push((rect, Target::Option(i)));
        }
    }
}

fn help_rows(app: &App) -> Vec<(String, String, bool)> {
    let k = |a: &str| app.keymap.label(a);
    let section = |name: &str| (name.to_string(), String::new(), true);
    let row = |key: String, text: &str| (key, text.to_string(), false);
    vec![
        section("Navigation"),
        row("j / k, ↑ / ↓".into(), "Move in session list or between messages"),
        row("Enter".into(), "Open conversation for reading"),
        row("Esc".into(), "Back to the list / clear search / close popup"),
        row(k("info"), "Session information"),
        (String::new(), String::new(), false),
        section("Search"),
        row(k("focus_search"), "Search box on the list; vim-style search inside a conversation"),
        row(k("cycle_scope"), "Cycle scope: prompts → answers → both → titles"),
        row(k("scope_user"), "Search my prompts only"),
        row(k("scope_assistant"), "Search agent answers only"),
        row(k("scope_both"), "Search prompts and answers"),
        row(k("cycle_project_filter"), "Worktree → repository → all projects"),
        row(k("choose_project"), "Choose the project (dropdown)"),
        row(k("cycle_agent"), "All agents → Claude → Codex → OpenCode → Copilot → Pi"),
        row(k("cycle_sort"), "Sort: last activity / created / title"),
        row(k("choose_theme"), "Theme (previewed while moving; remembered)"),
        row(format!("{} / {}", k("next_match"), k("prev_match")), "Next / previous matching message"),
        row("agent: branch: since: until: cwd:".into(), "Filters inside the search box"),
        (String::new(), String::new(), false),
        section("Sessions"),
        row(k("resume"), "Resume session"),
        row(k("favorite"), "Toggle favorite (pinned on top)"),
        row(k("copy_message"), "Copy message (or marked messages)"),
        row(k("mark_message"), "Mark message for copying a section"),
        row(k("copy_conversation"), "Copy whole conversation"),
        row(k("refresh"), "Re-index now"),
        row(k("quit"), "Quit"),
        row("ctrl+c ctrl+c".into(), "Quit from anywhere (also while typing)"),
        row("esc esc".into(), "Quit from the home page (empty search)"),
    ]
}

fn draw_modal(frame: &mut Frame, app: &mut App) {
    let Some(modal) = app.modal.take() else { return };
    let theme = app.theme.clone();
    match &modal {
        Modal::Theme { cursor, current } => {
            let mut items = vec![Line::raw(format!(
                "auto ({}){}",
                resolve("auto"),
                if current == "auto" { " ✓" } else { "" }
            ))];
            items.extend(THEMES.iter().map(|t| Line::raw(format!("{t}{}", if t == current { " ✓" } else { "" }))));
            let inner = dialog(frame, &theme, 34, items.len() as u16 + 2, (0, 1), None);
            put(frame.buffer_mut(), inner.x, inner.y, &Line::styled("Theme", bold()), inner.width);
            let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
            option_lines(frame, app, list, &items, *cursor, false);
        }
        Modal::Info(s) => {
            let fmt = &app.config.ui.date_format;
            let rows = [
                ("Title", s.title.clone()),
                ("Session ID", s.session_id.clone()),
                ("Agent", Agent::label(&s.agent).to_string()),
                ("Created", local_time(s.created_at.as_deref(), fmt)),
                ("Last activity", local_time(s.updated_at.as_deref(), fmt)),
                ("Directory", s.cwd.clone().unwrap_or_else(|| "—".into())),
                ("Worktree", s.worktree.clone().unwrap_or_else(|| "—".into())),
                ("Repository", s.repo.as_deref().map_or("—".into(), |r| r.trim_end_matches("/.git").to_string())),
                ("Branch", s.branch.clone().unwrap_or_else(|| "unknown".into())),
                ("Your messages", s.user_count.to_string()),
                ("Agent messages", s.assistant_count.to_string()),
                ("Log file", s.path.clone()),
            ];
            let inner_w = 76.min(frame.area().width.saturating_sub(10)) as usize;
            let mut lines = vec![];
            for (label, value) in rows {
                lines.push(Line::styled(label, bold().add_modifier(Modifier::DIM)));
                lines.extend(wrap(&[Span::raw(crate::model::sanitize(&value))], inner_w, &[], &[]));
                lines.push(Line::default());
            }
            lines.pop();
            let inner = dialog(frame, &theme, inner_w as u16, lines.len() as u16, (1, 2), None);
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Modal::ResumeDir { row, current, cursor, remember } => {
            let choice = |key: &str, label: &str, path: Option<&str>| {
                let mut spans = vec![
                    Span::styled(format!("{key}  "), fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{label:<10}"), bold()),
                ];
                spans.push(match path.filter(|p| !p.is_empty()) {
                    None => Span::styled("unknown", fg(Color::Red)),
                    Some(p) if std::path::Path::new(p).is_dir() => Span::styled(p.to_string(), fg(theme.muted)),
                    Some(p) => Span::styled(format!("{p}  (missing)"), fg(Color::Red)),
                });
                Line::from(spans)
            };
            let items = [choice("o", "Original", row.cwd.as_deref()), choice("c", "Current", Some(current))];
            let differs = row.cwd.as_deref().is_some_and(|c| !c.is_empty() && realpath(c) != realpath(current));
            let warn = matches!(row.agent.as_str(), "claude" | "opencode" | "pi") && differs;
            let inner = dialog(frame, &theme, 78, 6 + u16::from(warn), (0, 1), None);
            let title = fit(vec![Span::styled(format!("Resume “{}” in:", row.title), bold())], inner.width as usize);
            put(frame.buffer_mut(), inner.x, inner.y, &title, inner.width);
            option_lines(frame, app, Rect::new(inner.x, inner.y + 2, inner.width, 2), &items, *cursor, false);
            let mut y = inner.y + 5;
            if warn {
                let note = "The agent finds sessions by project directory: Current may fail.";
                put(frame.buffer_mut(), inner.x, y, &Line::styled(note, fg(theme.muted).add_modifier(Modifier::ITALIC)), inner.width);
                y += 1;
            }
            let hint = Line::from(vec![
                Span::styled("m ", fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(if *remember { "[x] " } else { "[ ] " }, if *remember { bold() } else { Style::default() }),
                Span::styled("remember   ·   Enter select   ·   Esc cancel", fg(theme.muted)),
            ]);
            put(frame.buffer_mut(), inner.x, y, &hint, inner.width);
        }
        Modal::Confirm { message, .. } => {
            let lines: Vec<Line> = message.split('\n').flat_map(|l| wrap(&[Span::raw(l.to_string())], 76, &[], &[])).collect();
            let inner = dialog(frame, &theme, 76, lines.len() as u16 + 2, (1, 2), None);
            frame.render_widget(Paragraph::new(lines.clone()), inner);
            let y = inner.y + lines.len() as u16 + 1;
            put(frame.buffer_mut(), inner.x, y, &Line::styled("[y] Yes    [n] No", bold()), inner.width);
        }
        Modal::Help { scroll } => {
            let rows = help_rows(app);
            let key_w = rows.iter().filter(|r| !r.2).map(|r| width(&r.0)).max().unwrap_or(10) + 2;
            let inner_w = 86.min((frame.area().width * 95 / 100).saturating_sub(6)) as usize;
            let lines: Vec<Line> = rows
                .into_iter()
                .flat_map(|(key, text, section)| {
                    if section {
                        return vec![Line::styled(key, bold().add_modifier(Modifier::UNDERLINED))];
                    }
                    let key = Span::styled(format!("{key:<key_w$}"), fg(Color::Cyan).add_modifier(Modifier::BOLD));
                    wrap(&[Span::raw(text)], inner_w, &[key], &[Span::raw(" ".repeat(key_w))])
                })
                .collect();
            let max_h = frame.area().height * 9 / 10;
            let inner = dialog(frame, &theme, 86, (lines.len() as u16).min(max_h.saturating_sub(4)), (1, 2), None);
            let scroll = (*scroll).min(lines.len().saturating_sub(inner.height as usize));
            app.modal = Some(Modal::Help { scroll });
            frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
            return;
        }
        Modal::Dropdown { options, cursor, anchor, .. } => {
            let area = frame.area();
            let w = options.iter().map(|(l, _)| width(l) as u16 + 4).max().unwrap_or(10).max(anchor.width).min(area.width - anchor.x);
            let h = (options.len() as u16).min(12).min(area.height.saturating_sub(anchor.y + 3)) + 2;
            let rect = Rect::new(anchor.x, anchor.y + 1, w, h);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(fg(theme.accent))
                .style(Style::default().bg(surface(&theme)).fg(theme.fg));
            let inner = block.inner(rect);
            frame.render_widget(Clear, rect);
            frame.render_widget(block, rect);
            let items: Vec<Line<'static>> = options.iter().map(|(l, _)| Line::raw(l.clone())).collect();
            option_lines(frame, app, inner, &items, *cursor, true);
        }
    }
    app.modal = Some(modal);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fit_cuts_with_ellipsis() {
        assert_eq!(text(&fit(vec![Span::raw("hello"), Span::raw(" world")], 8)), "hello w…");
        assert_eq!(text(&fit(vec![Span::raw("short")], 8)), "short");
    }

    #[test]
    fn highlight_splits_across_spans() {
        let spans = [Span::raw("ab"), Span::raw("cdef")];
        let out = highlight(&spans, &[(1, 4, bold())]);
        let parts: Vec<(&str, bool)> =
            out.iter().map(|s| (s.content.as_ref(), s.style.add_modifier.contains(Modifier::BOLD))).collect();
        assert_eq!(parts, [("a", false), ("b", true), ("cd", true), ("ef", false)]);
    }
}
