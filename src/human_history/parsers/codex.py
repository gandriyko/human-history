"""OpenAI Codex CLI rollouts: ``$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl``.

Handles the wrapped format (``{"timestamp", "type", "payload"}``) and the early bare format
(first line is raw meta, then bare ResponseItems). Prompts and answers are written twice in
the wrapped format (``event_msg`` + ``response_item``); see ``_emit_user`` for de-duplication.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from human_history.models import Agent, Message, ParseResult, Role, sanitize
from human_history.parsers.base import parse_time, read_records

# Text Codex injects as role=user that the human never typed.
INJECTED_PREFIXES = (
    "# AGENTS.md instructions for ",
    "<environment_context>",
    "<user_instructions>",
    "<INSTRUCTIONS>",
    "<turn_aborted>",
    "<user_shell_command>",
    "<subagent_notification>",
    "<skill>",
)
IDE_REQUEST_MARKER = "## My request for Codex:"
_ENV_CWD = re.compile(r"<environment_context>.*?<cwd>(.*?)</cwd>", re.S)


def human_prompt(text: str) -> str | None:
    """Strip IDE context wrappers; ``None`` if the text is an injection, not a prompt."""
    stripped = text.strip()
    if not stripped or stripped.startswith(INJECTED_PREFIXES):
        return None
    if stripped.startswith("# Context from my IDE setup:") and IDE_REQUEST_MARKER in stripped:
        stripped = stripped.rsplit(IDE_REQUEST_MARKER, 1)[1].strip()
    return stripped or None


class CodexParser:
    agent = Agent.CODEX

    def discover(self, roots: list[Path]) -> Iterator[Path]:
        for root in roots:
            if root.is_dir():
                yield from root.rglob("rollout-*.jsonl")

    def titles(self, roots: list[Path]) -> dict[str, str]:
        """Short thread names from ``$CODEX_HOME/session_index.jsonl`` (latest entry wins)."""
        best: dict[str, tuple[str, str]] = {}
        for index in {root.parent / "session_index.jsonl" for root in roots}:
            try:
                lines = index.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                continue
            for line in lines:
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(row, dict):
                    continue
                sid, name, updated = row.get("id"), row.get("thread_name"), str(row.get("updated_at") or "")
                fresh = sid not in best or updated >= best[sid][0]
                if isinstance(sid, str) and isinstance(name, str) and name.strip() and fresh:
                    best[sid] = (updated, sanitize(name.strip()))
        return {sid: name for sid, (_, name) in best.items()}

    def parse(self, path: Path, offset: int = 0, state: dict | None = None) -> ParseResult:
        result = ParseResult(state=dict(state or {}))
        st = result.state
        for line_offset, record in read_records(path, offset, result):
            ts = parse_time(record.get("timestamp"))
            kind = record.get("type")
            payload = record.get("payload")
            if "record_type" in record:
                continue
            if not isinstance(payload, dict):
                if kind is None and "id" in record:  # early bare format: raw meta line
                    kind, payload = "session_meta", record
                elif isinstance(kind, str):  # early bare format: bare ResponseItem
                    kind, payload = "response_item", record
                else:
                    continue
            if ts:
                result.session.updated_at = ts
            uid = f"o{line_offset}"
            if kind == "session_meta":
                self._meta(payload, result)
            elif kind == "event_msg" and payload.get("type") == "user_message":
                st["events"] = True
                text = payload.get("message")
                if isinstance(text, str):
                    self._emit_user(text, ts, uid, result)
            elif kind == "response_item" and payload.get("type") == "message":
                role = payload.get("role")
                if role == "user" and not st.get("events"):
                    text = "\n\n".join(_blocks(payload, "input_text"))
                    if not result.session.cwd and not st.get("cwd") and (m := _ENV_CWD.search(text)):
                        result.session.cwd = m.group(1).strip()  # early rollouts: cwd only lives here
                    self._emit_user(text, ts, uid, result)
                elif role == "assistant":
                    text = "\n\n".join(_blocks(payload, "output_text")).strip()
                    if text:
                        result.messages.append(Message(role=Role.ASSISTANT, text=sanitize(text), timestamp=ts, uid=uid))
                        st["last_user"] = None
        return result

    @staticmethod
    def _meta(payload: dict[str, Any], result: ParseResult) -> None:
        s = result.session
        if isinstance(payload.get("id"), str):
            s.session_id = payload["id"]
        if isinstance(payload.get("cwd"), str):
            s.cwd = payload["cwd"]
            result.state["cwd"] = True
        git = payload.get("git")
        if isinstance(git, dict) and isinstance(git.get("branch"), str):
            s.branch = git["branch"]
        s.created_at = parse_time(payload.get("timestamp")) or s.created_at

    @staticmethod
    def _emit_user(raw: str, ts, uid: str, result: ParseResult) -> None:
        text = human_prompt(raw)
        if text is None:
            return
        text = sanitize(text)
        # The same prompt arrives as response_item then event_msg with nothing in between.
        if result.state.get("last_user") == text:
            return
        result.state["last_user"] = text
        result.messages.append(Message(role=Role.USER, text=text, timestamp=ts, uid=uid))


def _blocks(payload: dict[str, Any], block_type: str) -> list[str]:
    content = payload.get("content")
    if isinstance(content, str):
        return [content]
    if not isinstance(content, list):
        return []
    return [
        b["text"]
        for b in content
        if isinstance(b, dict) and b.get("type") == block_type and isinstance(b.get("text"), str)
    ]
