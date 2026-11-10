//! Markdown -> wrapped, styled terminal lines (what `rich.markdown` did for the Python TUI).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::Theme;

pub fn width(s: &str) -> usize {
    s.width()
}

/// Greedy word wrap of styled text. `first`/`rest` prefix the first and the following lines.
pub fn wrap(spans: &[Span<'static>], width: usize, first: &[Span<'static>], rest: &[Span<'static>]) -> Vec<Line<'static>> {
    let prefix_w = |p: &[Span]| p.iter().map(|s| s.content.width()).sum::<usize>();
    let width = width.max(prefix_w(first).max(prefix_w(rest)) + 10);
    let mut lines = vec![];
    let mut line: Vec<Span<'static>> = first.to_vec();
    let mut start = prefix_w(first);
    let mut col = start;
    let mut push = |line: &mut Vec<Span<'static>>, col: &mut usize, start: &mut usize| {
        lines.push(Line::from(std::mem::replace(line, rest.to_vec())));
        *start = prefix_w(rest);
        *col = *start;
    };
    for span in spans {
        let text = span.content.replace('\t', "    ");
        let mut rest_text = text.as_str();
        while !rest_text.is_empty() {
            // the next token: a newline, a run of spaces, or a run of non-space characters
            let end = match rest_text.chars().next().unwrap() {
                '\n' => 1,
                c if c.is_whitespace() => rest_text.find(|c: char| !c.is_whitespace() || c == '\n').unwrap_or(rest_text.len()),
                _ => rest_text.find(char::is_whitespace).unwrap_or(rest_text.len()),
            };
            let (token, tail) = rest_text.split_at(end);
            rest_text = tail;
            let w = token.width();
            if token == "\n" {
                push(&mut line, &mut col, &mut start);
            } else if token.starts_with(char::is_whitespace) {
                if col == start {
                    continue; // no leading spaces on a wrapped line
                }
                if col + w > width {
                    push(&mut line, &mut col, &mut start);
                } else {
                    line.push(Span::styled(token.to_string(), span.style));
                    col += w;
                }
            } else {
                if col + w > width && col > start {
                    push(&mut line, &mut col, &mut start);
                }
                if col + w <= width {
                    line.push(Span::styled(token.to_string(), span.style));
                    col += w;
                    continue;
                }
                // longer than a whole line: hard-split by characters
                let mut piece = String::new();
                for ch in token.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if col + cw > width && col > start {
                        line.push(Span::styled(std::mem::take(&mut piece), span.style));
                        push(&mut line, &mut col, &mut start);
                    }
                    piece.push(ch);
                    col += cw;
                }
                line.push(Span::styled(piece, span.style));
            }
        }
    }
    lines.push(Line::from(line));
    lines
}

struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    quote: usize,
    /// One entry per open list: the next number (ordered) or None (bullets).
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    /// The first block of a list item is not separated by a blank line.
    item_start: bool,
    code: Option<String>,
    links: Vec<(String, usize)>,
    table: Option<Vec<Vec<String>>>,
}

impl Renderer<'_> {
    fn style(&self) -> Style {
        *self.styles.last().unwrap_or(&Style::default())
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        self.styles.push(f(self.style()));
    }

    fn text(&mut self, text: &str) {
        if let Some(code) = &mut self.code {
            code.push_str(text);
        } else if let Some(table) = &mut self.table {
            if let Some(cell) = table.last_mut().and_then(|row| row.last_mut()) {
                cell.push_str(text);
            }
        } else {
            self.spans.push(Span::styled(text.to_string(), self.style()));
        }
    }

    fn gap(&mut self) {
        if self.item_start {
            self.item_start = false;
            return;
        }
        if self.out.last().is_some_and(|l| l.width() > 0) {
            self.out.push(Line::default());
        }
    }

    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut base = vec![];
        if self.quote > 0 {
            base.push(Span::styled("▌ ".repeat(self.quote), Style::default().fg(self.theme.muted)));
        }
        if self.lists.is_empty() {
            return (base.clone(), base);
        }
        base.push(Span::raw("   ".repeat(self.lists.len() - 1)));
        let marker = self.marker.take();
        let marker_w = marker.as_deref().map_or(3, width);
        let mut first = base.clone();
        first.push(Span::raw(marker.unwrap_or_else(|| " ".repeat(marker_w))));
        let mut rest = base;
        rest.push(Span::raw(" ".repeat(marker_w)));
        (first, rest)
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let (first, rest) = self.prefixes();
        self.out.extend(wrap(&spans, self.width, &first, &rest));
    }

    fn code_block(&mut self, code: &str) {
        let style = Style::default().fg(Color::Rgb(248, 248, 242)).bg(Color::Rgb(39, 40, 34));
        let (first, _) = self.prefixes();
        let inner = self.width.saturating_sub(2).max(10);
        for raw in code.trim_end_matches('\n').split('\n') {
            let raw = raw.replace('\t', "    ");
            // ponytail: no syntax highlighting; long lines wrap by character.
            let mut chunks = vec![String::new()];
            for ch in raw.chars() {
                if width(chunks.last().unwrap()) + ch.width().unwrap_or(0) > inner {
                    chunks.push(String::new());
                }
                chunks.last_mut().unwrap().push(ch);
            }
            for chunk in chunks {
                let pad = inner.saturating_sub(width(&chunk));
                let mut spans = first.clone();
                spans.push(Span::styled(format!(" {chunk}{} ", " ".repeat(pad)), style));
                self.out.push(Line::from(spans));
            }
        }
    }

    fn table(&mut self, rows: Vec<Vec<String>>) {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0; cols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width(cell.trim()));
            }
        }
        let fits = widths.iter().sum::<usize>() + 3 * cols.saturating_sub(1) <= self.width;
        let sep = Style::default().fg(self.theme.muted);
        for (r, row) in rows.iter().enumerate() {
            let mut spans = vec![];
            for (i, cell) in row.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", sep));
                }
                let cell = cell.trim();
                let pad = if fits && i + 1 < row.len() { widths[i].saturating_sub(width(cell)) } else { 0 };
                let style = if r == 0 { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
                spans.push(Span::styled(format!("{cell}{}", " ".repeat(pad)), style));
            }
            self.out.extend(wrap(&spans, self.width, &[], &[]));
            if r == 0 && fits {
                let rule = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─");
                self.out.push(Line::styled(rule, sep));
            }
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => self.gap(),
                Tag::Heading { level, .. } => {
                    self.gap();
                    let underline = matches!(level, HeadingLevel::H1 | HeadingLevel::H2);
                    self.push_style(|s| {
                        let s = s.add_modifier(Modifier::BOLD);
                        if underline { s.add_modifier(Modifier::UNDERLINED) } else { s }
                    });
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.gap();
                    self.quote += 1;
                }
                Tag::CodeBlock(_) => {
                    self.flush();
                    self.gap();
                    self.code = Some(String::new());
                }
                Tag::List(start) => {
                    self.flush();
                    if self.lists.is_empty() {
                        self.gap();
                    }
                    self.item_start = false;
                    self.lists.push(start);
                }
                Tag::Item => {
                    self.flush();
                    let marker = match self.lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            format!(" {}. ", *n - 1)
                        }
                        _ => " • ".to_string(),
                    };
                    self.marker = Some(marker);
                    self.item_start = true;
                }
                Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    self.links.push((dest_url.to_string(), self.spans.len()));
                    self.push_style(|s| s.add_modifier(Modifier::UNDERLINED));
                }
                Tag::Image { .. } => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
                Tag::Table(_) => {
                    self.flush();
                    self.gap();
                    self.table = Some(vec![]);
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(t) = &mut self.table {
                        t.push(vec![]);
                    }
                }
                Tag::TableCell => {
                    if let Some(row) = self.table.as_mut().and_then(|t| t.last_mut()) {
                        row.push(String::new());
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => self.flush(),
                TagEnd::Heading(_) => {
                    self.flush();
                    self.styles.pop();
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.quote -= 1;
                }
                TagEnd::CodeBlock => {
                    if let Some(code) = self.code.take() {
                        self.code_block(&code);
                    }
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                }
                TagEnd::Item => {
                    self.flush();
                    self.item_start = false;
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Image => {
                    self.styles.pop();
                }
                TagEnd::Link => {
                    self.styles.pop();
                    if let Some((url, from)) = self.links.pop() {
                        let label: String = self.spans[from.min(self.spans.len())..].iter().map(|s| s.content.as_ref()).collect();
                        if !url.is_empty() && label != url {
                            self.spans.push(Span::styled(format!(" ({url})"), Style::default().fg(Color::Blue)));
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(rows) = self.table.take() {
                        self.table(rows);
                    }
                }
                _ => {}
            },
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) | Event::InlineMath(t) | Event::DisplayMath(t) => {
                self.text(&t)
            }
            Event::Code(t) => {
                if let Some(table) = &mut self.table {
                    if let Some(cell) = table.last_mut().and_then(|row| row.last_mut()) {
                        cell.push_str(&t);
                    }
                } else {
                    let style = self.style().fg(Color::Cyan).bg(Color::Rgb(54, 58, 64)).add_modifier(Modifier::BOLD);
                    self.spans.push(Span::styled(t.to_string(), style));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.flush();
                self.gap();
                self.out.push(Line::styled("─".repeat(self.width), Style::default().fg(self.theme.muted)));
            }
            Event::TaskListMarker(done) => self.text(if done { "☑ " } else { "☐ " }),
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]")),
        }
    }
}

pub fn render(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut r = Renderer {
        width: width.max(10),
        theme,
        out: vec![],
        spans: vec![],
        styles: vec![],
        quote: 0,
        lists: vec![],
        marker: None,
        item_start: false,
        code: None,
        links: vec![],
        table: None,
    };
    for event in Parser::new_ext(text, options) {
        r.event(event);
    }
    r.flush();
    while r.out.last().is_some_and(|l| l.width() == 0) {
        r.out.pop();
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorsConfig;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn renders_blocks_and_wraps() {
        let theme = Theme::new("terminal", &ColorsConfig::default());
        let md = "I added `charge()`:\n\n- up to **3** retries\n- an idempotency key so a retried charge is never applied twice\n\n```py\nx = 1\n```\nDone.";
        let out = plain(&render(md, 30, &theme));
        assert_eq!(
            out,
            [
                "I added charge():",
                "",
                " • up to 3 retries",
                " • an idempotency key so a",
                "   retried charge is never",
                "   applied twice",
                "",
                " x = 1",
                "",
                "Done.",
            ]
        );
    }

    #[test]
    fn hard_splits_long_words() {
        let spans = [Span::raw("x".repeat(25))];
        let out = plain(&wrap(&spans, 10, &[], &[]));
        assert_eq!(out, ["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]);
    }
}
