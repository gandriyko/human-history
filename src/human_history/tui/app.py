"""Textual application: a centered session list; Enter opens the conversation full-width."""

from __future__ import annotations

import asyncio
import contextlib
import os
import signal
import sqlite3
import sys
import time

from textual import events, work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.color import Color
from textual.containers import Horizontal, Vertical
from textual.timer import Timer
from textual.widgets import Footer, Input, OptionList, Select, Static

from human_history.config import Config
from human_history.integrations import herdr
from human_history.integrations.resume import (
    ResumeError,
    ResumePlan,
    build_plan,
    directory_for,
    needs_confirmation,
)
from human_history.storage import search as store
from human_history.storage.database import connect, get_kv, set_kv
from human_history.storage.indexer import index_all
from human_history.tui.layout import latin_key
from human_history.tui.screens import ConfirmScreen, HelpScreen, InfoScreen, ResumeDirectoryScreen
from human_history.tui.widgets import (
    AGENT_LABEL,
    Conversation,
    MessageView,
    SessionList,
    apply_colors,
    highlight_pattern,
)
from human_history.utils.git import locate

SCOPES = ["user", "assistant", "both", "title"]
SCOPE_LABEL = {"user": "my prompts", "assistant": "agent answers", "both": "prompts+answers", "title": "titles"}
AGENTS = ["all", "claude", "codex"]
AGENT_FILTER_LABEL = {"all": "all agents", "claude": "claude", "codex": "codex"}
FILTERS = ["current-worktree", "repository", "all"]
FILTER_LABEL = {"current-worktree": "worktree", "repository": "repository", "all": "all projects"}
SORTS = ["last_activity_desc", "created_desc", "title_asc"]
SORT_LABEL = {"last_activity_desc": "recent", "created_desc": "created", "title_asc": "title"}


def project_labels(roots: list[str], here: str) -> list[tuple[str, str]]:
    """(label, root) options for the project picker: the current one first, then by recent activity."""
    roots = [here, *(r for r in roots if r != here)]
    names = [os.path.basename(r.rstrip("/")) or r for r in roots]
    out = []
    for root, name in zip(roots, names, strict=True):
        label = name if names.count(name) == 1 else f"{name} ({os.path.basename(os.path.dirname(root))})"
        out.append((f"{label} (here)" if root == here else label, root))
    return out


class SearchInput(Input):
    """Session search box that also drives the list (fzf-style): arrows move, Enter opens."""

    BINDINGS = [
        Binding("up,ctrl+p", "app.list('cursor_up')", show=False),
        Binding("down,ctrl+n", "app.list('cursor_down')", show=False),
        Binding("pageup", "app.list('page_up')", show=False),
        Binding("pagedown", "app.list('page_down')", show=False),
    ]


class HistoryApp(App[ResumePlan | None]):
    CSS_PATH = "app.tcss"
    TITLE = "human-history"
    ENABLE_COMMAND_PALETTE = False

    BINDINGS = [
        Binding("slash", "focus_search", "Search", id="focus_search"),
        Binding("tab", "cycle_scope", "Scope", priority=True, id="cycle_scope"),
        Binding("ctrl+u", "scope('user')", "Prompts", priority=True, show=False, id="scope_user"),
        Binding("ctrl+a", "scope('assistant')", "Answers", priority=True, show=False, id="scope_assistant"),
        Binding("ctrl+shift+a", "scope('both')", "Both", priority=True, show=False, id="scope_both"),
        Binding("p", "cycle_project_filter", "Worktree/repo/all", id="cycle_project_filter"),
        Binding("o", "choose_project", "Project", id="choose_project"),
        Binding("a", "cycle_agent", "Agent", show=False, id="cycle_agent"),
        Binding("s", "cycle_sort", "Sort", show=False, id="cycle_sort"),
        Binding("r", "resume", "Resume", id="resume"),
        Binding("f", "favorite", "Fav", id="favorite"),
        Binding("i", "info", "Info", id="info"),
        Binding("n", "match(1)", "Next match", show=False, id="next_match"),
        Binding("N", "match(-1)", "Prev match", show=False, id="prev_match"),
        Binding("y", "copy_message", "Copy", show=False, id="copy_message"),
        Binding("Y", "copy_conversation", "Copy all", show=False, id="copy_conversation"),
        Binding("space", "mark_message", "Mark", show=False, id="mark_message"),
        Binding("ctrl+r", "refresh", "Reindex", show=False, id="refresh"),
        Binding("question_mark", "help", "Help", id="help"),
        Binding("q", "quit", "Quit", id="quit"),
        Binding("escape", "back", "Back", show=False),
        # Priority: otherwise a focused input or the screen claims Ctrl+C for copying.
        Binding("ctrl+c", "ctrl_c", "Quit (twice)", show=False, priority=True),
    ]

    def __init__(
        self,
        config: Config,
        cwd: str,
        project_filter: str | None = None,
        scope: str | None = None,
        query: str = "",
    ) -> None:
        self.config = config  # before super().__init__: get_css_variables() reads it
        apply_colors(config.colors)
        # ansi_color: keep the terminal's own background (theme colors are still used for accents).
        super().__init__(ansi_color=True)
        self.cwd = cwd
        self.db: sqlite3.Connection = connect(config.index.database_path())
        self.project_filter = (
            project_filter or get_kv(self.db, "project_filter") or config.search.default_project_filter
        )
        self.sort = get_kv(self.db, "sort") or config.search.sort
        self.scope = scope or config.search.default_scope
        self.agent = "all"
        # The project the worktree/repository filters refer to; starts as the one we were launched in.
        self.here = locate(cwd).worktree or cwd
        self.project_root = self.here
        self.initial_query = query
        self.current_key: str | None = None
        # Where Esc from a conversation returns: the search box or the list, whichever opened it.
        self._return_to: str = "#search"
        self.scroll_memory: dict[str, float] = {}
        self._search_timer: Timer | None = None
        # In-conversation "/" search: where it started and what to restore on Esc.
        self._find_origin = 0
        self._find_saved: tuple = (None, -1)
        self._last_ctrl_c = 0.0

    # ---------------------------------------------------------------- layout
    def compose(self) -> ComposeResult:
        with Vertical(id="home"):
            yield SearchInput(
                placeholder="Search…  ↑↓ select · Enter open · Esc to list (r resume, q quit)",
                id="search",
                compact=True,
            )
            with Horizontal(id="header"):
                yield Static(id="count")
                for select_id, labels, value in (
                    ("scope", SCOPE_LABEL, self.scope),
                    ("agent", AGENT_FILTER_LABEL, self.agent),
                    ("filter", FILTER_LABEL, self.project_filter),
                ):
                    options = [(label, key) for key, label in labels.items()]
                    yield Select(options, id=select_id, value=value, compact=True, allow_blank=False)
                yield Select(
                    project_labels([], self.here), id="project", value=self.here, compact=True, allow_blank=False
                )
            yield SessionList(id="sessions")
        yield Static(id="session-title")
        yield Conversation(id="conversation")
        with Horizontal(id="findbar"):
            yield Static("/", id="find-prompt")
            yield Input(placeholder="search in this session (Enter: go, n/N: next/prev, Esc: cancel)", id="find")
        yield Footer()

    def get_css_variables(self) -> dict[str, str]:
        """Theme variables plus the configurable selection colours used in app.tcss."""
        colors = self.config.colors
        accent = Color.parse(colors.selection_accent)
        return {
            **super().get_css_variables(),
            "selection": Color.parse(colors.selection).hex,
            "selection-accent": accent.hex,
            "selection-accent-bright": accent.lighten(0.1).hex,
        }

    async def on_event(self, event: events.Event) -> None:
        # Shortcuts work on a Cyrillic layout too: map keys to their Latin twins before bindings run.
        if isinstance(event, events.Key) and not event.is_forwarded:
            event = latin_key(event, typing=isinstance(self.focused, Input))
        await super().on_event(event)

    def on_mount(self) -> None:
        self.theme = {"dark": "textual-dark", "light": "textual-light"}.get(self.config.ui.theme, self.config.ui.theme)
        if self.config.ui.theme not in ("dark", "light") and self.config.ui.theme not in self.available_themes:
            self.notify(f"Unknown theme {self.config.ui.theme!r}; using textual-dark", severity="warning")
            self.theme = "textual-dark"
        self.set_keymap(self.config.keymap())
        # SIGTERM (VS Code "Stop", kill) / SIGHUP (terminal closed) would otherwise kill the process
        # before Textual switches mouse tracking off, leaving the shell flooded with mouse reports.
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGTERM, signal.SIGHUP):
            with contextlib.suppress(NotImplementedError, RuntimeError, ValueError):
                loop.add_signal_handler(sig, self.exit)
        search = self.query_one("#search", Input)
        search.value = self.initial_query
        self.refresh_projects()
        self.reload()
        search.focus()
        self.reindex()
        if self.config.index.auto_refresh:
            self.set_interval(self.config.index.refresh_interval_seconds, self.reindex)

    def refresh_projects(self) -> None:
        select = self.query_one("#project", Select)
        with select.prevent(Select.Changed):
            select.set_options(project_labels(store.projects(self.db), self.here))
            select.value = self.project_root

    def sync_header(self, count: int) -> None:
        """Reflect the current state in the header; changes made here must not re-trigger reloads."""
        self.query_one("#count", Static).update(f"{SORT_LABEL[self.sort]} [b]{count}[/b]")
        for select_id, value in (("scope", self.scope), ("agent", self.agent), ("filter", self.project_filter)):
            select = self.query_one(f"#{select_id}", Select)
            if select.value != value:
                with select.prevent(Select.Changed):
                    select.value = value
        # Picking a project only matters while filtering by worktree or repository.
        self.query_one("#project", Select).disabled = self.project_filter == "all"

    def on_select_changed(self, event: Select.Changed) -> None:
        if event.value is Select.BLANK:
            return
        value = str(event.value)
        state = {"scope": self.scope, "agent": self.agent, "filter": self.project_filter, "project": self.project_root}
        if state.get(event.select.id or "") == value:
            return  # mount-time / programmatic echo, nothing changed
        if event.select.id == "scope":
            self.scope = value
        elif event.select.id == "agent":
            self.agent = value
        elif event.select.id == "filter":
            self.project_filter = value
            set_kv(self.db, "project_filter", value)
        elif event.select.id == "project":
            self.project_root = value
        self.reload()
        self.query_one("#search", Input).focus()  # back to typing after picking

    # ---------------------------------------------------------------- data
    def filters(self) -> tuple[str, store.Filters]:
        base = store.Filters(
            project=self.project_filter,  # type: ignore[arg-type]
            cwd=self.project_root,
            agent=None if self.agent == "all" else self.agent,
        )
        return store.split_filters(self.query_one("#search", Input).value, base)

    def reload(self, keep_cursor: bool = False) -> None:
        self._search_timer = None
        query, filters = self.filters()
        limit = self.config.search.limit
        if query.strip():
            rows = store.search(self.db, query, self.scope, filters, self.sort, limit)  # type: ignore[arg-type]
        else:
            rows = store.list_sessions(self.db, filters, self.sort, limit)  # type: ignore[arg-type]
        sessions = self.query_one(SessionList)
        current = sessions.current()
        # A background refresh keeps the cursor on the same session; a new query starts at the top.
        sessions.show(rows, current.key if keep_cursor and current else None)
        self.sync_header(len(rows))
        if not rows:
            self.current_key = None
            self.query_one(Conversation).remove_children()

    def pattern(self):
        query, _ = self.filters()
        return highlight_pattern(query) if query.strip() and self.scope != "title" else None

    def on_input_changed(self, event: Input.Changed) -> None:
        if event.input.id == "find":  # incremental, like vim's incsearch
            self.query_one(Conversation).find(event.value, self._find_origin)
            return
        if self._search_timer:
            self._search_timer.stop()
        self._search_timer = self.set_timer(0.12, self.reload)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        if event.input.id == "find":
            conv = self.query_one(Conversation)
            if event.value and not conv.hits():
                self.notify(f"Pattern not found: {event.value}", severity="warning", timeout=2)
            self.close_find()
            if conv.hit_index >= 0:
                conv.hits()[conv.hit_index][0].focus(scroll_visible=False)
            self.update_title()
            return
        if self._search_timer:  # typed and hit Enter before the debounce fired
            self._search_timer.stop()
            self.reload()
        self._return_to = "#search"
        self.open_current(focus=True)

    def action_list(self, action: str) -> None:
        """Move the session list cursor while the search box keeps focus."""
        getattr(self.query_one(SessionList), f"action_{action}")()

    def close_find(self) -> None:
        find = self.query_one("#find", Input)
        self.screen.remove_class("-finding")
        if find.has_focus:
            self.query_one(Conversation).focus(scroll_visible=False)

    def cancel_find(self) -> None:
        conv = self.query_one(Conversation)
        self.close_find()
        conv.restore_search(self._find_saved)
        if conv.hit_index < 0:
            views = conv.views()
            if views:
                conv.focus_view(views[min(self._find_origin, len(views) - 1)])
        self.update_title()

    def update_title(self) -> None:
        """Session title line; shows vim-like [current/total] while a search is active."""
        conv = self.query_one(Conversation)
        row = conv.session
        if row is None:
            return
        text = f"{row.title}  ·  {AGENT_LABEL.get(row.agent, row.agent)}  ·  {row.user_count} prompts"
        if conv.pattern is not None:
            current, total = conv.position()
            text += f"  ·  [{current}/{total}]" if total else "  ·  no matches"
        self.query_one("#session-title", Static).update(text + "  ·  / search  n/N next/prev  Esc back")

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self._return_to = "#sessions"
        self.open_current(focus=True)

    @work(exclusive=True, group="open")
    async def open_current(self, focus: bool = False) -> None:
        """Render the highlighted session; with ``focus`` switch from the list to the conversation."""
        row = self.query_one(SessionList).current()
        conv = self.query_one(Conversation)
        if row is None:
            return
        same = row.key == self.current_key and conv.session is not None
        if not same:
            if self.current_key:
                self.scroll_memory[self.current_key] = conv.scroll_y
            self.current_key = row.key
            await conv.load(row, store.messages(self.db, row.key), self.pattern(), self.config.ui.date_format)
            conv.call_after_refresh(conv.scroll_to, y=self.scroll_memory.get(row.key, 0), animate=False)
        else:
            conv.set_pattern(self.pattern())
        conv.session = row
        if focus:
            self.screen.add_class("-reading")
            if not conv.focused_view():
                conv.focus(scroll_visible=False)
            self.update_title()
            # Match positions need the laid-out conversation: jump to the first hit after the refresh.
            if conv.pattern is not None:
                conv.call_after_refresh(self._jump_first_match)

    def _jump_first_match(self) -> None:
        self.query_one(Conversation).jump(1, origin=0)
        self.update_title()

    @work(thread=True, exclusive=True, group="index")
    def reindex(self) -> None:
        conn = connect(self.config.index.database_path())
        try:
            stats = index_all(conn, self.config)
        finally:
            conn.close()
        if stats.parsed:
            self.call_from_thread(self.after_reindex)

    def after_reindex(self) -> None:
        conv = self.query_one(Conversation)
        key, at_bottom = self.current_key, conv.scroll_y >= conv.max_scroll_y - 1
        self.refresh_projects()
        self.reload(keep_cursor=True)
        row = self.query_one(SessionList).current()
        if (
            self.screen.has_class("-reading")
            and key
            and row
            and row.key == key
            and conv.session
            and (row.user_count + row.assistant_count != conv.session.user_count + conv.session.assistant_count)
        ):
            self.scroll_memory[key] = float("inf") if at_bottom else conv.scroll_y
            self.current_key = None  # force re-render with new messages
            self.open_current()

    # ---------------------------------------------------------------- actions
    LIST_ONLY_ACTIONS = (
        "cycle_scope",
        "scope",
        "cycle_project_filter",
        "cycle_sort",
        "cycle_agent",
        "choose_project",
    )

    def check_action(self, action: str, parameters: tuple[object, ...]) -> bool | None:
        # While a dialog is open only it reacts to keys (App bindings would otherwise still fire).
        if len(self.screen_stack) > 1:
            return action in ("quit", "ctrl_c", "command_palette")
        # Session-list search controls do nothing useful while reading; let inputs keep e.g. Ctrl+U.
        return not (self.screen.has_class("-reading") and action in self.LIST_ONLY_ACTIONS)

    def action_ctrl_c(self) -> None:
        """Ctrl+C twice (within 1.5 s) quits; a single press still copies any selected text."""
        now = time.monotonic()
        if now - self._last_ctrl_c < 1.5:
            self.exit()
            return
        self._last_ctrl_c = now
        focused = self.focused
        selected = focused.selected_text if isinstance(focused, Input) else self.screen.get_selected_text()
        if selected:
            self.copy_to_clipboard(selected)
        self.notify("Press Ctrl+C again to quit", timeout=1.5)

    def action_focus_search(self) -> None:
        if self.screen.has_class("-reading"):
            # vim-style "/" inside the open conversation
            conv = self.query_one(Conversation)
            self._find_saved = conv.save_search()
            self._find_origin = conv.cursor_index()
            find = self.query_one("#find", Input)
            find.value = ""
            self.screen.add_class("-finding")
            find.focus()
            return
        self.query_one("#search", Input).focus()

    def action_back(self) -> None:
        """Esc: cancel in-session find → leave the conversation → search box ⇄ list (2nd Esc clears)."""
        search = self.query_one("#search", Input)
        if self.focused is self.query_one("#find", Input):
            self.cancel_find()
        elif self.screen.has_class("-reading") or isinstance(self.focused, MessageView):
            self.screen.remove_class("-reading")
            self.query_one(self._return_to).focus()
        elif self.focused is search:
            self.query_one(SessionList).focus()  # keeps the results; single-key actions (r, f, q…) work here
        elif isinstance(self.focused, Select):
            search.focus()
        else:
            search.value = ""
            search.focus()

    def action_cycle_scope(self) -> None:
        self.action_scope(SCOPES[(SCOPES.index(self.scope) + 1) % len(SCOPES)])

    def action_scope(self, scope: str) -> None:
        self.scope = scope
        self.reload()

    def action_cycle_agent(self) -> None:
        self.agent = AGENTS[(AGENTS.index(self.agent) + 1) % len(AGENTS)]
        self.reload()

    def action_choose_project(self) -> None:
        if self.project_filter == "all":  # a project only narrows worktree/repository views
            self.project_filter = "current-worktree"
            self.reload()
        select = self.query_one("#project", Select)
        select.focus()
        select.expanded = True

    def action_cycle_project_filter(self) -> None:
        self.project_filter = FILTERS[(FILTERS.index(self.project_filter) + 1) % len(FILTERS)]
        set_kv(self.db, "project_filter", self.project_filter)
        self.reload()

    def action_cycle_sort(self) -> None:
        self.sort = SORTS[(SORTS.index(self.sort) + 1) % len(SORTS)]
        set_kv(self.db, "sort", self.sort)
        self.reload()

    def action_favorite(self) -> None:
        row = self.query_one(SessionList).current()
        if row:
            added = store.toggle_favorite(self.db, row.key)
            self.notify("Pinned" if added else "Unpinned", timeout=1.5)
            self.reload(keep_cursor=True)

    def action_info(self) -> None:
        row = self.query_one(SessionList).current()
        if row:
            self.push_screen(InfoScreen(row, self.config.ui.date_format))

    def action_match(self, delta: int) -> None:
        if self.query_one(Conversation).jump(delta):
            self.update_title()
        else:
            self.notify("No matches (press / to search this session)", timeout=1.5)

    def action_mark_message(self) -> None:
        view = self.query_one(Conversation).focused_view()
        if view:
            view.set_marked(not view.marked)
            self.query_one(Conversation).action_move(1)

    def action_copy_message(self) -> None:
        conv = self.query_one(Conversation)
        marked = [v for v in conv.views() if v.marked]
        if marked:
            self._copy("\n\n---\n\n".join(v.as_markdown() for v in marked), f"{len(marked)} messages")
            for v in marked:
                v.set_marked(False)
        elif view := conv.focused_view():
            self._copy(view.text, "message")
        else:
            self.notify("Focus a message first (Enter, then j/k)", severity="warning")

    def action_copy_conversation(self) -> None:
        views = self.query_one(Conversation).views()
        if views:
            self._copy("\n\n".join(v.as_markdown() for v in views), "conversation")

    def _copy(self, text: str, what: str) -> None:
        self.copy_to_clipboard(text)  # OSC 52: works over SSH in Kitty/Ghostty/Herdr
        self.notify(f"Copied {what}", timeout=1.5)

    def action_refresh(self) -> None:
        self.notify("Re-indexing…", timeout=1)
        self.reindex()

    def action_help(self) -> None:
        self.push_screen(HelpScreen(self.config.keymap()))

    # ---------------------------------------------------------------- resume
    def current_worktree(self) -> str:
        return locate(self.cwd).worktree or self.cwd

    @work(exclusive=True, group="resume")
    async def action_resume(self) -> None:
        row = self.query_one(SessionList).current()
        if row is None:
            return
        current = self.current_worktree()
        policy = get_kv(self.db, "resume_directory") or self.config.resume.directory
        directory = directory_for(policy, row, current)  # type: ignore[arg-type]
        if directory is None:
            choice = await self.push_screen_wait(ResumeDirectoryScreen(row, current))
            if choice is None:
                return
            directory, remember = choice
            if remember:
                set_kv(self.db, "resume_directory", remember)
        elif needs_confirmation(row, directory):
            ok = await self.push_screen_wait(
                ConfirmScreen(
                    f"Resume in {directory}?\n\nThe session was recorded in {row.cwd or 'an unknown directory'}."
                )
            )
            if not ok:
                return
        try:
            plan = build_plan(row, directory)
        except ResumeError as exc:
            self.notify(str(exc), severity="error", timeout=6)
            return
        if not herdr.inside_herdr():
            self.exit(plan)  # the CLI execs the agent after the TUI has restored the terminal
            return
        try:
            await self._herdr_resume(plan)
        except herdr.HerdrError as exc:
            self.notify(str(exc), severity="error", timeout=8)
            return
        if self.config.resume.close_history_on_resume:
            self.exit(None)
        else:
            self.notify(f"Resumed {plan.agent} session in Herdr", timeout=2)

    async def _herdr_resume(self, plan: ResumePlan) -> None:
        workspace = os.environ.get("HH_WORKSPACE") or os.environ.get("HERDR_WORKSPACE_ID")
        target = os.environ.get("HH_TARGET_PANE") or os.environ.get("HERDR_PANE_ID")
        await asyncio.to_thread(herdr.resume, plan, self.config.resume.placement, workspace, target)


# Mouse tracking (all modes), focus events, bracketed paste off; cursor visible. Harmless if already off.
TERMINAL_RESET = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[?25h"


def run_tui(config: Config, cwd: str | None = None, **kwargs) -> ResumePlan | None:
    app = HistoryApp(config, cwd or os.environ.get("HH_CWD") or os.getcwd(), **kwargs)
    try:
        return app.run()
    finally:
        # Belt and braces for crashes: never hand the shell back with mouse reporting still on.
        if sys.__stdout__ and sys.__stdout__.isatty():
            sys.__stdout__.write(TERMINAL_RESET)
            sys.__stdout__.flush()
