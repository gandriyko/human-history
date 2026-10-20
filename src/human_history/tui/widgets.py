"""Session list, conversation view and metadata panel."""

from __future__ import annotations

import re
from datetime import UTC, datetime
from pathlib import Path

from rich.console import Console, ConsoleOptions, Group, RenderResult
from rich.markdown import Markdown
from rich.measure import Measurement
from rich.segment import Segment
from rich.style import Style
from rich.text import Text
from textual.binding import Binding
from textual.color import Color as TextualColor
from textual.containers import VerticalScroll
from textual.widgets import OptionList, Static
from textual.widgets.option_list import Option

from human_history.storage.search import HL_END, HL_START, SessionRow

AGENT_LABEL = {"claude": "Claude", "codex": "Codex"}
# Rich styles for rendered text; overridden from the [colors] config by apply_colors().
AGENT_STYLE = {"claude": "bold #d97757", "codex": "bold #10a37f"}
MATCH_STYLE = Style(bgcolor="#665c00", color="#ffffff", bold=True)
CURRENT_MATCH_STYLE = Style(bgcolor="#ff9e3b", color="#000000", bold=True)  # the one n/N landed on
AGENT_DOT = {"claude": "#d97757", "codex": "#10a37f"}
PROJECT_STYLE = "#e0a86e"


def apply_colors(colors) -> None:
    """Use the configured palette (``config.ColorsConfig``) for all text rendered from now on."""
    global MATCH_STYLE, CURRENT_MATCH_STYLE, PROJECT_STYLE
    # ponytail: module-level palette, fine for the single app per process.
    hex_ = {name: TextualColor.parse(value).hex for name, value in colors.model_dump().items()}
    MATCH_STYLE = Style(bgcolor=hex_["match"], color="#ffffff", bold=True)
    CURRENT_MATCH_STYLE = Style(bgcolor=hex_["current_match"], color="#000000", bold=True)
    PROJECT_STYLE = hex_["project"]
    for agent in ("claude", "codex"):
        AGENT_DOT[agent] = hex_[agent]
        AGENT_STYLE[agent] = f"bold {hex_[agent]}"


def local_time(value: str | None, fmt: str) -> str:
    if not value:
        return "—"
    try:
        return datetime.fromisoformat(value).astimezone().strftime(fmt)
    except ValueError:
        return value


def ago(value: str | None, now: datetime | None = None) -> str:
    """Compact age of an ISO timestamp: 5m, 3h, 2d, 3w, 4mo, 1y."""
    if not value:
        return "—"
    try:
        seconds = ((now or datetime.now(UTC)) - datetime.fromisoformat(value)).total_seconds()
    except ValueError:
        return "?"
    minutes, hours, days = seconds / 60, seconds / 3600, seconds / 86400
    if minutes < 60:
        return f"{max(int(minutes), 0)}m"
    if hours < 24:
        return f"{int(hours)}h"
    if days < 14:
        return f"{int(days)}d"
    if days < 60:
        return f"{int(days // 7)}w"
    if days < 365:
        return f"{int(days // 30)}mo"
    return f"{int(days // 365)}y"


def project_name(row: SessionRow) -> str:
    """Repository name (shared by all its worktrees), else the directory name."""
    if row.repo:
        repo = Path(row.repo)
        return (repo.parent.name if repo.name == ".git" else repo.name.removesuffix(".git")) or "—"
    path = row.worktree or row.cwd
    return Path(path).name if path else "—"


def highlight_pattern(query: str) -> re.Pattern[str] | None:
    terms = [t.strip('"*') for t in re.findall(r'"[^"]+"|\S+', query) if ":" not in t.strip('"')]
    terms = [t for t in terms if t]
    if not terms:
        return None
    return re.compile("|".join(re.escape(t) for t in sorted(terms, key=len, reverse=True)), re.I)


def snippet_text(snippet: str) -> Text:
    """FTS snippet with \\x02…\\x03 markers -> highlighted one-line Text."""
    text = Text(no_wrap=True, overflow="ellipsis")
    for i, part in enumerate(re.split(f"[{HL_START}{HL_END}]", snippet.replace("\n", " "))):
        text.append(part, MATCH_STYLE if i % 2 else "")
    return text


class Highlighted:
    """Wraps a renderable and highlights regex matches in its rendered text segments.

    Every render records the line of each match in ``hit_lines`` (match order = n/N order);
    match number ``current`` gets a distinct style.
    """

    def __init__(self, renderable, pattern: re.Pattern[str], current: int | None = None) -> None:
        self.renderable, self.pattern, self.current = renderable, pattern, current
        self.hit_lines: list[int] = []

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        hit_lines: list[int] = []
        line = 0
        for seg in console.render(self.renderable, options):
            if seg.control or not seg.text or not self.pattern.search(seg.text):
                line += seg.text.count("\n")
                yield seg
                continue
            pos = 0
            for m in self.pattern.finditer(seg.text):
                if m.start() > pos:
                    yield Segment(seg.text[pos : m.start()], seg.style)
                style = CURRENT_MATCH_STYLE if len(hit_lines) == self.current else MATCH_STYLE
                yield Segment(m.group(), (seg.style or Style()) + style)
                hit_lines.append(line + seg.text.count("\n", 0, m.start()))
                pos = m.end()
            if pos < len(seg.text):
                yield Segment(seg.text[pos:], seg.style)
            line += seg.text.count("\n")
        self.hit_lines = hit_lines

    def __rich_measure__(self, console: Console, options: ConsoleOptions) -> Measurement:
        return Measurement.get(console, options, self.renderable)


class SessionList(OptionList):
    BINDINGS = [
        Binding("j", "cursor_down", "Down", show=False),
        Binding("k", "cursor_up", "Up", show=False),
        Binding("g", "first", "First", show=False),
        Binding("G", "last", "Last", show=False),
    ]

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self.rows: list[SessionRow] = []

    def show(self, rows: list[SessionRow], keep_key: str | None = None) -> None:
        self.rows = rows
        width = min(max((len(project_name(r)) for r in rows), default=8), 18)
        now = datetime.now(UTC)
        self.set_options([Option(self._prompt(r, width, now), id=r.key) for r in rows])
        if not rows:
            return
        index = next((i for i, r in enumerate(rows) if r.key == keep_key), 0)
        self.highlighted = index

    def current(self) -> SessionRow | None:
        if self.highlighted is None or not 0 <= self.highlighted < len(self.rows):
            return None
        return self.rows[self.highlighted]

    @staticmethod
    def _prompt(r: SessionRow, project_width: int, now: datetime) -> Text:
        """``  1h ● claude  project   Title`` (+ an indented matching snippet while searching)."""
        line = Text(no_wrap=True, overflow="ellipsis")
        line.append(f"{ago(r.updated_at, now):>4} ", style="dim")
        line.append("● ", style=AGENT_DOT.get(r.agent, "white"))
        line.append(f"{r.agent:<7}", style="dim")
        project = project_name(r)
        if len(project) > project_width:
            project = project[: project_width - 1] + "…"
        line.append(f"{project:<{project_width}}  ", style=PROJECT_STYLE)
        if r.favorite:
            line.append("★ ", style="bold yellow")
        line.append(r.title, style="dim" if r.title == "(untitled)" else "")
        if r.hits:
            line.append(f"  ({r.hits})", style="dim")
        if r.snippet:
            line.append("\n" + " " * (16 + project_width))  # aligned under the title column
            line.append("you: " if r.hit_role == "user" else "agent: ", style="dim cyan")
            line.append_text(snippet_text(r.snippet))
        return line


class MessageView(Static, can_focus=True):
    """One message. The role header is shown only where the speaker changes."""

    def __init__(self, message_id: int, role: str, text: str, agent: str, show_header: bool, ts: str) -> None:
        super().__init__(classes=f"message {role}" + (" first" if show_header else ""))
        self.message_id, self.role, self.text, self.agent = message_id, role, text, agent
        self.show_header, self.ts = show_header, ts
        self.pattern: re.Pattern[str] | None = None
        self.current: int | None = None  # index of the current n/N match inside this message
        self.marked = False
        self._highlighted: Highlighted | None = None
        self._renderable = None

    def on_mount(self) -> None:
        self.refresh_content()

    def refresh_content(self) -> None:
        body = Markdown(self.text, code_theme="monokai", hyperlinks=False)
        self._highlighted = None
        if self.pattern and self.pattern.search(self.text):
            body = self._highlighted = Highlighted(body, self.pattern, self.current)
        renderable = body
        if self.show_header:
            name = "USER" if self.role == "user" else AGENT_LABEL.get(self.agent, "ASSISTANT").upper()
            header = Text(name, style="bold cyan" if self.role == "user" else AGENT_STYLE.get(self.agent, "bold"))
            header.append(f"  {self.ts}", style="dim")
            renderable = Group(header, Text(), body)
        self._renderable = renderable
        self.update(renderable)

    def match_lines(self, width: int) -> list[int]:
        """Line (from the top of this widget's content) of every match, rendered at ``width``."""
        if self._highlighted is None:
            return []
        console = self.app.console
        console.render_lines(self._renderable, console.options.update_width(max(width, 10)), pad=False)
        offset = 2 if self.show_header else 0  # header line + blank line
        return [offset + line for line in self._highlighted.hit_lines]

    def set_current(self, index: int | None) -> None:
        if self.current != index:
            self.current = index
            self.refresh_content()

    def set_marked(self, marked: bool) -> None:
        self.marked = marked
        self.set_class(marked, "marked")

    def as_markdown(self) -> str:
        name = "USER" if self.role == "user" else AGENT_LABEL.get(self.agent, "ASSISTANT").upper()
        return f"**{name}**\n\n{self.text}"


class Conversation(VerticalScroll):
    BINDINGS = [
        Binding("j,down", "move(1)", "Next message", show=False),
        Binding("k,up", "move(-1)", "Previous message", show=False),
        Binding("g,home", "edge(0)", "Top", show=False),
        Binding("G,end", "edge(-1)", "Bottom", show=False),
    ]

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self.session: SessionRow | None = None
        self.pattern: re.Pattern[str] | None = None
        # Every match occurrence in reading order: (message view, n-th match inside it, line in it).
        self._hits: list[tuple[MessageView, int, int]] | None = None
        self.hit_index = -1

    def views(self) -> list[MessageView]:
        return list(self.query(MessageView))

    def on_resize(self) -> None:
        self._hits = None  # match line positions depend on the width; n/N position is kept

    async def load(self, session: SessionRow, rows, pattern, date_format: str) -> None:
        self.session = session
        self.pattern, self._hits, self.hit_index = pattern, None, -1
        await self.remove_children()
        widgets, previous = [], None
        for r in rows:
            widgets.append(
                MessageView(
                    r["id"],
                    r["role"],
                    r["text"],
                    session.agent,
                    r["role"] != previous,
                    local_time(r["ts"], date_format),
                )
            )
            widgets[-1].pattern = pattern
            previous = r["role"]
        if widgets:
            await self.mount_all(widgets)
        else:
            await self.mount(Static("No human-readable messages in this session.", classes="empty"))

    def set_pattern(self, pattern) -> None:
        """Highlight ``pattern`` everywhere and reset the n/N position."""
        self.pattern, self._hits, self.hit_index = pattern, None, -1
        for v in self.views():
            if v.pattern != pattern or v.current is not None:
                v.pattern, v.current = pattern, None
                v.refresh_content()

    def focused_view(self) -> MessageView | None:
        f = self.app.focused
        return f if isinstance(f, MessageView) and f.parent is self else None

    def action_move(self, delta: int) -> None:
        views = self.views()
        if not views:
            return
        current = self.focused_view()
        index = views.index(current) + delta if current else (0 if delta > 0 else len(views) - 1)
        self.focus_view(views[max(0, min(index, len(views) - 1))])

    def action_edge(self, index: int) -> None:
        views = self.views()
        if views:
            self.focus_view(views[index])

    def focus_view(self, view: MessageView, top: bool = True, focus: bool = True) -> None:
        if focus:
            view.focus(scroll_visible=False)
        self.scroll_to_widget(view, top=top, animate=False)

    # ------------------------------------------------------------ match navigation (vim n / N)
    def hits(self) -> list[tuple[MessageView, int, int]]:
        if self._hits is None:
            # Computed lazily: line positions need the laid-out width.
            fallback = self.scrollable_content_region.width or 80
            self._hits = [
                (view, k, line)
                for view in self.views()
                for k, line in enumerate(view.match_lines(view.content_size.width or fallback - 4))
            ]
        return self._hits

    def cursor_index(self) -> int:
        current = self.focused_view()
        return self.views().index(current) if current else 0

    def jump(self, delta: int, origin: int | None = None, focus: bool = True) -> bool:
        """Go to the next (``delta=1``) / previous match occurrence, wrapping like vim.

        Without a current match, start from the first match in message ``origin`` or later
        (default: the message under the cursor).
        """
        hits = self.hits()
        if not hits:
            return False
        if self.hit_index < 0 or origin is not None:
            views = self.views()
            start = self.cursor_index() if origin is None else origin
            after = [i for i, (v, _, _) in enumerate(hits) if views.index(v) >= start]
            self.hit_index = (after[0] if after else 0) if delta >= 0 else (after[0] - 1 if after else -1)
            self.hit_index %= len(hits)
        else:
            self.hit_index = (self.hit_index + delta) % len(hits)
        self._show(self.hit_index, focus)
        return True

    def _show(self, index: int, focus: bool) -> None:
        for view, _, _ in self.hits():
            if view.current is not None:
                view.set_current(None)
        view, k, line = self.hits()[index]
        view.set_current(k)
        if focus:
            view.focus(scroll_visible=False)
        y = view.virtual_region.y + view.styles.gutter.top + line
        self.scroll_to(y=max(0, y - self.scrollable_content_region.height // 3), animate=False)

    def position(self) -> tuple[int, int]:
        """(current match number starting at 1, total matches); (0, total) before the first jump."""
        return self.hit_index + 1, len(self.hits())

    def find(self, text: str, origin: int = 0) -> int:
        """Vim-style ``/``: highlight, scroll to the first match at/after message ``origin``.

        Focus stays in the find input while typing. Returns the number of matches.
        """
        self.set_pattern(vim_pattern(text))
        if not self.jump(1, origin=origin, focus=False):
            return 0
        return len(self.hits())

    def save_search(self) -> tuple:
        return self.pattern, self.hit_index

    def restore_search(self, state: tuple) -> None:
        pattern, index = state
        self.set_pattern(pattern)
        if index >= 0 and index < len(self.hits()):
            self.hit_index = index
            self._show(index, focus=True)


def vim_pattern(text: str) -> re.Pattern[str] | None:
    """Literal pattern with vim's smartcase: case-insensitive unless the text has a capital."""
    if not text:
        return None
    return re.compile(re.escape(text), 0 if any(c.isupper() for c in text) else re.I)


class InfoPanel(Static):
    def show_session(self, s: SessionRow | None, date_format: str) -> None:
        if s is None:
            self.update("")
            return
        t = Text()
        rows = [
            ("Title", s.title),
            ("Session ID", s.session_id),
            ("Agent", AGENT_LABEL.get(s.agent, s.agent)),
            ("Created", local_time(s.created_at, date_format)),
            ("Last activity", local_time(s.updated_at, date_format)),
            ("Directory", s.cwd or "—"),
            ("Worktree", s.worktree or "—"),
            ("Repository", s.repo.removesuffix("/.git") if s.repo else "—"),
            ("Branch", s.branch or "unknown"),
            ("Your messages", str(s.user_count)),
            ("Agent messages", str(s.assistant_count)),
            ("Log file", s.path),
        ]
        for label, value in rows:
            t.append(f"{label}\n", style="bold dim")
            t.append(f"{value}\n\n")
        self.update(t)
