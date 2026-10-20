"""GitHub Copilot CLI: ``$COPILOT_HOME/session-state/<session-id>/events.jsonl``.

Every line is ``{type, data, id, timestamp, parentId}``. Only ``user.message`` (``data.content``,
not ``transformedContent`` which carries injected reminders) and the visible ``content`` of
``assistant.message`` are kept; tool requests, reasoning fields, tool/hook/skill/system events
are ignored. Titles live in ``workspace.yaml`` (``name``, or ``summary`` in older versions).
"""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

from human_history.models import Agent, Message, ParseResult, Role, sanitize
from human_history.parsers.base import parse_time, read_records

INJECTED_PREFIXES = ("<system_reminder>", "<system-reminder>", "<current_datetime>")


class CopilotParser:
    agent = Agent.COPILOT

    def discover(self, roots: list[Path]) -> Iterator[Path]:
        for root in roots:
            if root.is_dir():
                yield from root.glob("*/events.jsonl")

    def titles(self, roots: list[Path]) -> dict[str, str]:
        titles = {}
        for root in roots:
            for meta in root.glob("*/workspace.yaml") if root.is_dir() else ():
                fields = _flat_yaml(meta)
                title = fields.get("name") or fields.get("summary")
                if title:
                    titles[meta.parent.name] = sanitize(title)
        return titles

    def parse(self, path: Path, offset: int = 0, state: dict | None = None) -> ParseResult:
        result = ParseResult(state=dict(state or {}))
        s = result.session
        s.session_id = path.parent.name
        for line_offset, record in read_records(path, offset, result):
            data = record.get("data")
            if not isinstance(data, dict):
                continue
            kind, ts, uid = record.get("type"), parse_time(record.get("timestamp")), f"o{line_offset}"
            if ts:
                s.updated_at = ts
            if kind in ("session.start", "session.resume"):
                context = data.get("context")
                if isinstance(context, dict):
                    s.cwd = context.get("cwd") if isinstance(context.get("cwd"), str) else s.cwd
                    s.branch = context.get("branch") if isinstance(context.get("branch"), str) else s.branch
                if kind == "session.start":
                    s.created_at = parse_time(data.get("startTime")) or ts
            elif kind == "user.message":
                text = data.get("content")
                if data.get("source") or not isinstance(text, str):  # e.g. "instruction-discovery"
                    continue
                text = text.strip()
                if text and not text.startswith(INJECTED_PREFIXES):
                    result.messages.append(Message(role=Role.USER, text=sanitize(text), timestamp=ts, uid=uid))
            elif kind == "assistant.message":
                text = data.get("content")
                if isinstance(text, str) and text.strip():
                    result.messages.append(
                        Message(role=Role.ASSISTANT, text=sanitize(text.strip()), timestamp=ts, uid=uid)
                    )
        return result


def _flat_yaml(path: Path) -> dict[str, str]:
    """``workspace.yaml`` is flat ``key: value``; enough for ``name``/``summary`` without a YAML dependency."""
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return {}
    fields = {}
    for line in lines:
        key, sep, value = line.partition(": ")
        if sep and not line.startswith(" "):
            fields[key.strip()] = value.strip().strip("'\"")
    return fields
