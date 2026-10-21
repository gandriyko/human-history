"""TOML configuration (``~/.config/human-history/config.toml``), validated strictly."""

from __future__ import annotations

import fcntl
import json
import os
import re
import sqlite3
import stat
import tempfile
import tomllib
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator
from textual.color import Color, ColorParseError


def _xdg(var: str, default: str) -> Path:
    return Path(os.environ.get(var) or Path.home() / default)


CONFIG_PATH = _xdg("XDG_CONFIG_HOME", ".config") / "human-history" / "config.toml"
EXAMPLE_PATH = Path(__file__).with_name("config.example.toml")  # every option, with defaults
DATA_DIR = _xdg("XDG_DATA_HOME", ".local/share") / "human-history"

Scope = Literal["user", "assistant", "both", "title"]
ProjectFilter = Literal["current-worktree", "repository", "all"]
Sort = Literal["last_activity_desc", "created_desc", "title_asc"]
ResumeDirectory = Literal["ask", "current", "original"]


class _Section(BaseModel):
    model_config = ConfigDict(extra="forbid")


class UIConfig(_Section):
    # auto: Herdr's theme inside Herdr, otherwise "terminal" (the terminal's own colours).
    theme: str = "auto"
    date_format: str = "%Y-%m-%d %H:%M"
    # Accepted so the documented example validates; technical events are never displayed.
    show_tool_calls: Literal[False] = False
    show_tool_results: Literal[False] = False
    show_reasoning: Literal[False] = False
    show_system_messages: Literal[False] = False

    @field_validator("theme")
    @classmethod
    def _theme(cls, value: str) -> str:
        from human_history.tui.themes import THEMES  # textual themes; imported only when validating

        if value != "auto" and value not in THEMES:
            raise ValueError(f"unknown theme {value!r}; one of: auto, {', '.join(THEMES)}")
        return value


class ColorsConfig(_Section):
    """Any CSS colour: "#rrggbb", "rgb(…)", or a name such as "teal"."""

    selection: str = "#4d2a22"  # selected row / option background
    selection_accent: str = "#d97757"  # conversation scrollbar thumb, focused-message bar
    project: str = "#e0a86e"  # project names in the session list
    claude: str = "#d97757"  # agent dot and header
    codex: str = "#10a37f"
    opencode: str = "#5fafff"
    copilot: str = "#a371f7"
    pi: str = "#56b6c2"
    match: str = "#665c00"  # search matches (background)
    current_match: str = "#ff9e3b"  # the match n/N is on (background)

    @field_validator("*")
    @classmethod
    def _color(cls, value: str) -> str:
        try:
            Color.parse(value)
        except ColorParseError as exc:
            raise ValueError(f"not a colour: {value!r}") from exc
        return value


class SearchConfig(_Section):
    default_scope: Scope = "both"
    default_project_filter: ProjectFilter = "repository"
    sort: Sort = "last_activity_desc"
    limit: int = Field(200, ge=10, le=5000)


class RememberedConfig(_Section):
    """Choices made in the TUI; these override the configured defaults."""

    project_filter: ProjectFilter | None = None
    sort: Sort | None = None
    theme: str | None = None
    resume_directory: ResumeDirectory | None = None

    @field_validator("theme")
    @classmethod
    def _theme(cls, value: str | None) -> str | None:
        return UIConfig._theme(value) if value is not None else None


class ResumeConfig(_Section):
    directory: ResumeDirectory = "ask"
    # Where Herdr opens the resumed agent.
    placement: Literal["tab", "split-right", "split-down"] = "tab"
    close_history_on_resume: bool = True


class HerdrConfig(_Section):
    # How plugin actions show the history browser.
    history_placement: Literal["overlay", "popup", "zoomed", "split", "tab"] = "overlay"
    popup_size: str = "90%"


class IndexConfig(_Section):
    auto_refresh: bool = True
    refresh_interval_seconds: int = Field(30, ge=5)
    database: str | None = None

    def database_path(self) -> Path:
        return Path(self.database).expanduser() if self.database else DATA_DIR / "index.sqlite3"


def _claude_default() -> list[str]:
    return [str(Path(os.environ.get("CLAUDE_CONFIG_DIR", "~/.claude")) / "projects")]


def _codex_default() -> list[str]:
    home = Path(os.environ.get("CODEX_HOME", "~/.codex"))
    return [str(home / "sessions"), str(home / "archived_sessions")]


def _opencode_default() -> list[str]:
    return [os.environ.get("OPENCODE_DB") or str(_xdg("XDG_DATA_HOME", ".local/share") / "opencode")]


def _copilot_default() -> list[str]:
    return [str(Path(os.environ.get("COPILOT_HOME", "~/.copilot")) / "session-state")]


def _pi_default() -> list[str]:
    sessions = Path(os.environ.get("PI_CODING_AGENT_DIR", "~/.pi/agent")) / "sessions"
    return [str(sessions), *filter(None, [os.environ.get("PI_CODING_AGENT_SESSION_DIR")])]


class PathsConfig(_Section):
    claude: list[str] = Field(default_factory=_claude_default)
    codex: list[str] = Field(default_factory=_codex_default)
    opencode: list[str] = Field(default_factory=_opencode_default)  # the data dir, or opencode.db itself
    copilot: list[str] = Field(default_factory=_copilot_default)
    pi: list[str] = Field(default_factory=_pi_default)


# Action id -> comma-separated Textual key names. Unlisted actions keep their defaults.
KEY_ACTIONS = {
    "focus_search": "slash",
    "cycle_scope": "tab",
    "scope_user": "ctrl+u",
    "scope_assistant": "ctrl+a",
    "scope_both": "ctrl+shift+a",
    "cycle_project_filter": "p",
    "choose_project": "o",
    "cycle_agent": "a",
    "cycle_sort": "s",
    "choose_theme": "t",
    "resume": "r",
    "favorite": "f",
    "info": "i",
    "next_match": "n",
    "prev_match": "N",
    "copy_message": "y",
    "copy_conversation": "Y",
    "mark_message": "space",
    "refresh": "ctrl+r",
    "help": "question_mark",
    "quit": "q",
}


class Config(_Section):
    ui: UIConfig = Field(default_factory=UIConfig)
    colors: ColorsConfig = Field(default_factory=ColorsConfig)
    search: SearchConfig = Field(default_factory=SearchConfig)
    remembered: RememberedConfig = Field(default_factory=RememberedConfig)
    resume: ResumeConfig = Field(default_factory=ResumeConfig)
    index: IndexConfig = Field(default_factory=IndexConfig)
    herdr: HerdrConfig = Field(default_factory=HerdrConfig)
    paths: PathsConfig = Field(default_factory=PathsConfig)
    keys: dict[str, str] = Field(default_factory=dict)

    @field_validator("keys")
    @classmethod
    def _known_keys(cls, value: dict[str, str]) -> dict[str, str]:
        unknown = sorted(set(value) - set(KEY_ACTIONS))
        if unknown:
            raise ValueError(f"unknown key action(s) {unknown}; valid: {sorted(KEY_ACTIONS)}")
        return value

    def keymap(self) -> dict[str, str]:
        return {**KEY_ACTIONS, **self.keys}


class ConfigError(Exception):
    pass


def config_path() -> Path:
    return Path(os.environ.get("HUMAN_HISTORY_CONFIG") or CONFIG_PATH).expanduser()


def load_config(path: Path | None = None) -> Config:
    path = path or config_path()
    if not path.exists():
        return Config()
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        return Config.model_validate(data)
    except tomllib.TOMLDecodeError as exc:
        raise ConfigError(f"{path}: invalid TOML: {exc}") from exc
    except ValidationError as exc:
        lines = [f"{path}: invalid configuration"]
        for err in exc.errors():
            where = ".".join(str(p) for p in err["loc"])
            lines.append(f"  {where}: {err['msg']}")
        raise ConfigError("\n".join(lines)) from exc


_TABLE = re.compile(r"^\s*\[+[^\]]+\]+\s*(?:#.*)?$")
_REMEMBERED = re.compile(r"^\s*\[remembered\]\s*(?:#.*)?$")


def save_remembered(key: str, value: str, *, only_if_missing: bool = False) -> bool:
    """Write one TUI choice without rewriting unrelated TOML or losing concurrent choices."""
    try:
        RememberedConfig.model_validate({key: value})
    except ValidationError as exc:
        raise ConfigError(f"invalid remembered {key}: {value!r}") from exc
    if key not in RememberedConfig.model_fields:
        raise ConfigError(f"unknown remembered setting: {key}")

    path = config_path()
    if path.is_symlink():
        path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = path.with_name(path.name + ".lock")
    with lock_path.open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        original = path.read_text(encoding="utf-8") if path.exists() else ""
        data = tomllib.loads(original)
        Config.model_validate(data)
        if only_if_missing and key in data.get("remembered", {}):
            return False

        lines = original.splitlines(keepends=True)
        start = next((i for i, line in enumerate(lines) if _REMEMBERED.fullmatch(line.strip())), None)
        assignment = f"{key} = {json.dumps(value)}\n"
        if start is None:
            prefix = "" if not original else ("\n" if original.endswith("\n") else "\n\n")
            updated = original + prefix + "[remembered]\n" + assignment
        else:
            end = next((i for i in range(start + 1, len(lines)) if _TABLE.fullmatch(lines[i].strip())), len(lines))
            field = re.compile(rf"^\s*{re.escape(key)}\s*=")
            position = next((i for i in range(start + 1, end) if field.match(lines[i])), None)
            if position is None:
                if not lines[start].endswith("\n"):
                    lines[start] += "\n"
                lines.insert(start + 1, assignment)
            else:
                old = lines[position]
                comment = re.search(r"\s+#.*$", old.rstrip("\r\n"))
                lines[position] = assignment.rstrip("\n") + (comment.group() if comment else "") + "\n"
            updated = "".join(lines)

        Config.model_validate(tomllib.loads(updated))
        fd, temp_name = tempfile.mkstemp(prefix=".config.toml.", dir=path.parent)
        try:
            os.fchmod(fd, stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as temp:
                temp.write(updated)
            os.replace(temp_name, path)
        finally:
            if os.path.exists(temp_name):
                os.unlink(temp_name)
        return True


def migrate_legacy_preferences(conn: sqlite3.Connection) -> bool:
    """Move old index database choices into the config without overriding existing choices."""
    rows = conn.execute(
        "SELECT key, value FROM kv WHERE key IN ('project_filter', 'sort', 'theme', 'resume_directory')"
    ).fetchall()
    changed = False
    for key, value in rows:
        changed = save_remembered(key, value, only_if_missing=True) or changed
    if rows:
        with conn:
            conn.executemany("DELETE FROM kv WHERE key = ?", [(key,) for key, _ in rows])
    return changed
