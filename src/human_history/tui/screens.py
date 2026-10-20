"""Modal dialogs: resume directory choice, confirmation, session info, theme, keyboard help."""

from __future__ import annotations

import os
from collections.abc import Callable

from rich.table import Table
from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import OptionList, Static
from textual.widgets.option_list import Option

from human_history.storage.search import SessionRow
from human_history.tui.themes import THEMES, resolve
from human_history.tui.widgets import InfoPanel


class ThemeScreen(ModalScreen[str | None]):
    """Pick a theme; the highlighted one is previewed live. Result: the choice, or None on Esc."""

    BINDINGS = [Binding("escape,q", "dismiss(None)", "Cancel")]

    def __init__(self, current: str, preview: Callable[[str], None]) -> None:
        super().__init__()
        self.current, self.preview = current, preview

    def compose(self) -> ComposeResult:
        names = ["auto", *THEMES]
        options = [Option(f"{name} ✓" if name == self.current else name, id=name) for name in names]
        options[0] = Option(f"auto ({resolve('auto')})" + (" ✓" if self.current == "auto" else ""), id="auto")
        with Vertical(classes="dialog compact theme"):
            yield Static(Text("Theme", style="bold"))
            yield OptionList(*options)

    def on_mount(self) -> None:
        options = self.query_one(OptionList)
        options.highlighted = options.get_option_index(self.current) if self.current in THEMES else 0

    def on_option_list_option_highlighted(self, event: OptionList.OptionHighlighted) -> None:
        self.preview(event.option.id or "auto")

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self.dismiss(event.option.id)


class InfoScreen(ModalScreen[None]):
    BINDINGS = [Binding("escape,q,i", "dismiss", "Close")]

    def __init__(self, session: SessionRow, date_format: str) -> None:
        super().__init__()
        self.session, self.date_format = session, date_format

    def compose(self) -> ComposeResult:
        with Vertical(classes="dialog"):
            yield InfoPanel()

    def on_mount(self) -> None:
        self.query_one(InfoPanel).show_session(self.session, self.date_format)


def _choice(key: str, label: str, path: str | None) -> Text:
    t = Text(no_wrap=True, overflow="ellipsis")
    t.append(f"{key}  ", style="bold cyan")
    t.append(f"{label:<10}", style="bold")
    if not path:
        t.append("unknown", style="red")
    elif os.path.isdir(path):
        t.append(path, style="dim")
    else:
        t.append(f"{path}  (missing)", style="red")
    return t


class ResumeDirectoryScreen(ModalScreen[tuple[str, str | None] | None]):
    """Ask where to resume. Result: (directory, policy to remember or None)."""

    BINDINGS = [
        Binding("o", "pick('original')", "Original"),
        Binding("c", "pick('current')", "Current"),
        Binding("m", "toggle_remember", "Remember"),
        Binding("escape,q", "cancel", "Cancel"),
    ]

    def __init__(self, session: SessionRow, current: str) -> None:
        super().__init__()
        self.session, self.current = session, current
        self.remember = False

    def compose(self) -> ComposeResult:
        with Vertical(classes="dialog compact"):
            yield Static(Text(f"Resume “{self.session.title}” in:", style="bold", no_wrap=True, overflow="ellipsis"))
            yield OptionList(
                Option(_choice("o", "Original", self.session.cwd), id="original"),
                Option(_choice("c", "Current", self.current), id="current"),
            )
            differs = self.session.cwd and os.path.realpath(self.session.cwd) != os.path.realpath(self.current)
            if self.session.agent in ("claude", "opencode", "pi") and differs:
                yield Static(
                    Text("The agent finds sessions by project directory: Current may fail.", style="dim italic")
                )
            yield Static(id="hint")

    def on_mount(self) -> None:
        self.refresh_hint()

    def refresh_hint(self) -> None:
        hint = Text(style="dim")
        hint.append("m ", style="bold cyan")
        hint.append("[x] " if self.remember else "[ ] ", style="bold" if self.remember else "")
        hint.append("remember   ·   Enter select   ·   Esc cancel")
        self.query_one("#hint", Static).update(hint)

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self.action_pick(event.option.id or "original")

    def action_toggle_remember(self) -> None:
        self.remember = not self.remember
        self.refresh_hint()

    def action_pick(self, which: str) -> None:
        directory = self.session.cwd if which == "original" else self.current
        if not directory:
            self.notify("The session has no recorded directory.", severity="error")
            return
        self.dismiss((directory, which if self.remember else None))

    def action_cancel(self) -> None:
        self.dismiss(None)


class ConfirmScreen(ModalScreen[bool]):
    BINDINGS = [Binding("y,enter", "answer(True)", "Yes"), Binding("n,escape,q", "answer(False)", "No")]

    def __init__(self, message: str) -> None:
        super().__init__()
        self.message = message

    def compose(self) -> ComposeResult:
        with Vertical(classes="dialog"):
            yield Static(Text(self.message))
            yield Static(Text("[y] Yes    [n] No", style="bold"))

    def action_answer(self, value: bool) -> None:
        self.dismiss(value)


HELP = [
    (
        "Navigation",
        [
            ("j / k, ↑ / ↓", "Move in session list or between messages"),
            ("Enter", "Open conversation for reading"),
            ("Esc", "Back to the list / clear search / close popup"),
            ("{info}", "Session information"),
        ],
    ),
    (
        "Search",
        [
            ("{focus_search}", "Search box on the list; vim-style search inside a conversation"),
            ("{cycle_scope}", "Cycle scope: prompts → answers → both → titles"),
            ("{scope_user}", "Search my prompts only"),
            ("{scope_assistant}", "Search agent answers only"),
            ("{scope_both}", "Search prompts and answers"),
            ("{cycle_project_filter}", "Worktree → repository → all projects"),
            ("{choose_project}", "Choose the project (dropdown)"),
            ("{cycle_agent}", "All agents → Claude → Codex → OpenCode → Copilot → Pi"),
            ("{cycle_sort}", "Sort: last activity / created / title"),
            ("{choose_theme}", "Theme (previewed while moving; remembered)"),
            ("{next_match} / {prev_match}", "Next / previous matching message"),
            ("agent: branch: since: until: cwd:", "Filters inside the search box"),
        ],
    ),
    (
        "Sessions",
        [
            ("{resume}", "Resume session"),
            ("{favorite}", "Toggle favorite (pinned on top)"),
            ("{copy_message}", "Copy message (or marked messages)"),
            ("{mark_message}", "Mark message for copying a section"),
            ("{copy_conversation}", "Copy whole conversation"),
            ("{refresh}", "Re-index now"),
            ("{quit}", "Quit"),
            ("ctrl+c ctrl+c", "Quit from anywhere (also while typing)"),
            ("esc esc", "Quit from the home page (empty search)"),
        ],
    ),
]


class HelpScreen(ModalScreen[None]):
    BINDINGS = [Binding("escape,q,question_mark", "dismiss", "Close")]

    def __init__(self, keymap: dict[str, str]) -> None:
        super().__init__()
        self.keymap = {k: v.replace("question_mark", "?").replace("slash", "/") for k, v in keymap.items()}

    def compose(self) -> ComposeResult:
        table = Table(box=None, show_header=False, padding=(0, 2))
        table.add_column(style="bold cyan", no_wrap=True)
        table.add_column()
        for section, rows in HELP:
            table.add_row(Text(section, style="bold underline"), "")
            for keys, text in rows:
                table.add_row(keys.format(**self.keymap), text)
            table.add_row("", "")
        with Vertical(classes="dialog help"):
            yield Static(table)
