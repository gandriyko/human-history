"""Parser contract and shared JSONL reading.

A parser turns one agent log file into normalized human-readable messages. Parsing is
incremental: callers pass the byte offset (and opaque state) returned by the previous run.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from datetime import datetime
from pathlib import Path
from typing import Any, Protocol

from human_history.models import Agent, ParseResult


class Parser(Protocol):
    agent: Agent

    def discover(self, roots: list[Path]) -> Iterator[Path]: ...

    def parse(self, path: Path, offset: int = 0, state: dict | None = None) -> ParseResult: ...

    def titles(self, roots: list[Path]) -> dict[str, str]:
        """Agent-provided session titles keyed by session id (optional)."""
        ...

    # A parser whose file is a database of many sessions sets ``multi_session = True`` and
    # implements ``parse_store(path, state) -> (results, state)`` instead of ``parse``.


def read_records(path: Path, offset: int, result: ParseResult) -> Iterator[tuple[int, dict[str, Any]]]:
    """Yield ``(line_offset, record)`` for every complete JSON-object line after ``offset``.

    A trailing line without ``\\n`` is still being written: it is left for the next run.
    Malformed lines are counted and skipped. ``result.offset`` ends after the last complete line.
    """
    result.offset = offset
    with path.open("rb") as fh:
        fh.seek(offset)
        position = offset
        for raw in fh:
            if not raw.endswith(b"\n"):
                break
            line_offset, position = position, position + len(raw)
            result.offset = position
            if not raw.strip():
                continue
            try:
                record = json.loads(raw)
            except (json.JSONDecodeError, UnicodeDecodeError):
                result.skipped_lines += 1
                continue
            if isinstance(record, dict):
                yield line_offset, record
            else:
                result.skipped_lines += 1


def parse_time(value: Any) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def first_line_title(text: str, limit: int = 90) -> str:
    line = next((ln.strip() for ln in text.splitlines() if ln.strip()), "")
    line = line.lstrip("#>*- ").strip()
    return line if len(line) <= limit else line[: limit - 1].rstrip() + "…"
