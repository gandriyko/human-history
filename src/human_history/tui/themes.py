"""Themes named like Herdr's, so ``auto`` can follow the theme Herdr is using.

``terminal`` keeps the terminal's own colours and background (ANSI mode); every other theme
paints its own background. Most map onto Textual's built-in themes.
"""

from __future__ import annotations

import dataclasses
import os
import tomllib
from pathlib import Path

from textual.theme import BUILTIN_THEMES, Theme

TERMINAL = "terminal"

# Herdr themes Textual has no built-in counterpart for: defined here from their official palettes.
_CUSTOM = {
    "gruvbox-light": Theme(
        name="gruvbox-light",
        primary="#b57614",
        secondary="#076678",
        accent="#427b58",
        warning="#af3a03",
        error="#9d0006",
        success="#79740e",
        foreground="#3c3836",
        background="#fbf1c7",
        surface="#ebdbb2",
        panel="#d5c4a1",
        dark=False,
    ),
    "tokyo-night-day": Theme(
        name="tokyo-night-day",
        primary="#2e7de9",
        secondary="#9854f1",
        accent="#007197",
        warning="#b15c00",
        error="#f52a65",
        success="#587539",
        foreground="#3760bf",
        background="#e1e2e7",
        surface="#d0d5e3",
        panel="#c4c8da",
        dark=False,
    ),
}
# Herdr theme name -> the Textual built-in theme it is copied from (renamed).
_BUILTIN = {
    TERMINAL: "textual-dark",  # with ansi_color: only accents come from it
    "catppuccin": "catppuccin-mocha",
    "catppuccin-latte": "catppuccin-latte",
    "tokyo-night": "tokyo-night",
    "dracula": "dracula",
    "nord": "nord",
    "gruvbox": "gruvbox",
    "one-dark": "atom-one-dark",
    "one-light": "atom-one-light",
    "solarized": "solarized-dark",
    "solarized-light": "solarized-light",
}
_ORDER = [  # Herdr's order
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
]
THEMES: dict[str, Theme] = {
    name: _CUSTOM[name] if name in _CUSTOM else dataclasses.replace(BUILTIN_THEMES[_BUILTIN[name]], name=name)
    for name in _ORDER
}


def herdr_theme() -> str | None:
    """The theme Herdr is configured with, when running inside Herdr and it is one we know."""
    if not os.environ.get("HERDR_ENV"):
        return None
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "herdr" / "config.toml"
    try:
        name = tomllib.loads(config.read_text(encoding="utf-8")).get("theme", {}).get("name")
    except (OSError, tomllib.TOMLDecodeError, AttributeError):
        return None
    return name if name in THEMES else None


def resolve(name: str) -> str:
    """``auto`` -> Herdr's theme inside Herdr, else ``terminal``; unknown names -> ``terminal``."""
    if name == "auto":
        return herdr_theme() or TERMINAL
    return name if name in THEMES else TERMINAL
