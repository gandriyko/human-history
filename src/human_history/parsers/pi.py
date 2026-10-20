"""pi coding agent: ``~/.pi/agent/sessions/--<cwd>--/<timestamp>_<session-id>.jsonl``.

The first line is the ``session`` header (``id``, ``cwd``, ``timestamp``). Conversation entries
are ``{"type": "message", "message": {"role": ...}}``; only ``user`` text and the ``text``
blocks of ``assistant`` are kept. Thinking, tool calls/results, ``bashExecution``, ``custom``
and summary roles, and every other entry type are ignored. Branches (``/tree``) append entries,
so all messages are kept in file order. ``session_info.name`` is the user-given title.
"""

from __future__ import annotations

import re
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from human_history.models import Agent, Message, ParseResult, Role, sanitize
from human_history.parsers.base import parse_time, read_records

# Expanded `/skill:x` and `@file` arguments are prepended to the prompt the human typed.
_INJECTED_BLOCK = re.compile(r"^\s*<(skill|file)\s[^>]*>.*?</\1>\s*", re.S)


def human_prompt(text: str) -> str:
    while m := _INJECTED_BLOCK.match(text):
        text = text[m.end() :]
    return text.strip()


class PiParser:
    agent = Agent.PI

    def discover(self, roots: list[Path]) -> Iterator[Path]:
        for root in roots:
            if root.is_dir():
                yield from root.rglob("*.jsonl")

    def titles(self, roots: list[Path]) -> dict[str, str]:
        return {}  # session_info entries are read by parse()

    def parse(self, path: Path, offset: int = 0, state: dict | None = None) -> ParseResult:
        result = ParseResult(state=dict(state or {}))
        s = result.session
        for line_offset, record in read_records(path, offset, result):
            kind, ts, uid = record.get("type"), parse_time(record.get("timestamp")), f"o{line_offset}"
            if kind == "session":
                s.session_id = record.get("id") if isinstance(record.get("id"), str) else None
                s.cwd = record.get("cwd") if isinstance(record.get("cwd"), str) else None
                s.created_at = ts
                continue
            if ts:
                s.updated_at = ts
            if kind == "session_info" and isinstance(record.get("name"), str) and record["name"].strip():
                s.title = sanitize(record["name"].strip())
            message = record.get("message")
            if kind != "message" or not isinstance(message, dict):
                continue
            role = message.get("role")
            if role == "user":
                text = human_prompt("\n\n".join(_texts(message.get("content"))))
                if text:
                    result.messages.append(Message(role=Role.USER, text=sanitize(text), timestamp=ts, uid=uid))
            elif role == "assistant":
                text = "\n\n".join(_texts(message.get("content"))).strip()
                if text:
                    result.messages.append(Message(role=Role.ASSISTANT, text=sanitize(text), timestamp=ts, uid=uid))
        return result


def _texts(content: Any) -> list[str]:
    if isinstance(content, str):
        return [content]
    if not isinstance(content, list):
        return []
    return [
        b["text"] for b in content if isinstance(b, dict) and b.get("type") == "text" and isinstance(b.get("text"), str)
    ]
