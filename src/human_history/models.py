"""Normalized data shared by parsers, storage and UI."""

from __future__ import annotations

import re
from datetime import datetime
from enum import StrEnum

from pydantic import BaseModel, Field


class Role(StrEnum):
    USER = "user"
    ASSISTANT = "assistant"


class Agent(StrEnum):
    CLAUDE = "claude"
    CODEX = "codex"


class Message(BaseModel):
    role: Role
    text: str
    timestamp: datetime | None = None
    # Stable per-session identity used to drop duplicates (streaming chunks, replays, resumed copies).
    uid: str


class SessionPatch(BaseModel):
    """Session metadata discovered while parsing; ``None`` means "unknown / keep previous"."""

    session_id: str | None = None
    cwd: str | None = None
    branch: str | None = None
    title: str | None = None
    created_at: datetime | None = None
    updated_at: datetime | None = None


class ParseResult(BaseModel):
    session: SessionPatch = Field(default_factory=SessionPatch)
    messages: list[Message] = Field(default_factory=list)
    offset: int = 0
    # Parser-private carry-over between incremental runs (must be JSON-serializable).
    state: dict = Field(default_factory=dict)
    skipped_lines: int = 0


# C0/C1 control characters except \t and \n. Removes ESC, so no terminal escape sequence survives.
_CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]")


def sanitize(text: str) -> str:
    """Make untrusted session text safe to render in a terminal."""
    return _CONTROL.sub("", text.replace("\r\n", "\n"))
