"""Claude Code transcripts: ``~/.claude/projects/<encoded-cwd>/<session-id>.jsonl``.

Tool results are logged as ``type=user`` lines and every assistant content block (text,
thinking, tool_use) gets its own line, so classification is by block type and metadata,
never by role alone. Subagent transcripts (``<session>/subagents/*.jsonl``) are not human
conversations and are not discovered.
"""

from __future__ import annotations

import re
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from human_history.models import Agent, Message, ParseResult, Role, sanitize
from human_history.parsers.base import parse_time, read_records

# User-role text the human did not type as a conversational prompt.
NOT_HUMAN_PREFIXES = (
    "<local-command-",
    "<bash-stdout",
    "<bash-stderr",
    "<task-notification",
    "<system-reminder",
    "<user-prompt-submit-hook",
    "[Request interrupted by user",
    "This session is being continued from a previous conversation",
    "Caveat: The messages below were generated",
)
_TAG = r"<{0}>(.*?)</{0}>"
_COMMAND_NAME = re.compile(_TAG.format("command-name"), re.S)
_COMMAND_ARGS = re.compile(_TAG.format("command-args"), re.S)
_BASH_INPUT = re.compile(_TAG.format("bash-input"), re.S)
_PASTED_TAG = re.compile(r'</?pasted_content(?: id="[^"]*")?>')


def human_prompt(text: str) -> str | None:
    """Readable form of a user-line text, or ``None`` if it is not a human prompt."""
    text = text.strip()
    if not text or text.startswith(NOT_HUMAN_PREFIXES):
        return None
    if text.startswith(("<command-name>", "<command-message>")):
        name = _COMMAND_NAME.search(text)
        if not name:
            return None
        args = _COMMAND_ARGS.search(text)
        return f"{name.group(1).strip()} {args.group(1).strip() if args else ''}".strip()
    if text.startswith("<bash-input>"):
        cmd = _BASH_INPUT.search(text)
        return f"`! {cmd.group(1).strip()}`" if cmd else None
    return _PASTED_TAG.sub("", text).strip() or None


def _texts(content: Any) -> list[str] | None:
    """Text blocks of a message; ``None`` if it carries a tool result (not human input)."""
    if isinstance(content, str):
        return [content]
    if not isinstance(content, list):
        return []
    texts = []
    for block in content:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "tool_result":
            return None
        if block.get("type") == "text" and isinstance(block.get("text"), str):
            texts.append(block["text"])
    return texts


class ClaudeParser:
    agent = Agent.CLAUDE

    def discover(self, roots: list[Path]) -> Iterator[Path]:
        for root in roots:
            if root.is_dir():
                yield from root.glob("*/*.jsonl")

    def titles(self, roots: list[Path]) -> dict[str, str]:
        return {}  # titles are inside the transcript (custom-title / ai-title lines)

    def parse(self, path: Path, offset: int = 0, state: dict | None = None) -> ParseResult:
        result = ParseResult(state=dict(state or {}))
        st, s = result.state, result.session
        for _, rec in read_records(path, offset, result):
            kind = rec.get("type")
            if kind == "custom-title" and isinstance(rec.get("customTitle"), str):
                st["custom_title"] = True
                s.title = sanitize(rec["customTitle"].strip()) or s.title
                continue
            if kind == "ai-title" and isinstance(rec.get("aiTitle"), str) and not st.get("custom_title"):
                s.title = sanitize(rec["aiTitle"].strip()) or s.title
                continue
            if kind not in ("user", "assistant", "attachment") or rec.get("isSidechain"):
                continue

            ts = parse_time(rec.get("timestamp"))
            if ts:
                s.updated_at = ts
                if not st.get("created"):
                    st["created"] = True
                    s.created_at = ts
            if isinstance(rec.get("sessionId"), str):
                s.session_id = rec["sessionId"]
            if isinstance(rec.get("cwd"), str) and not st.get("cwd"):
                st["cwd"] = True  # the first cwd is where the session lives (and resumes)
                s.cwd = rec["cwd"]
            if isinstance(rec.get("gitBranch"), str) and rec["gitBranch"] and rec["gitBranch"] != "HEAD":
                s.branch = rec["gitBranch"]

            uid = rec.get("uuid")
            if not isinstance(uid, str):
                continue
            if kind == "assistant":
                self._assistant(rec, ts, uid, result)
            elif kind == "user":
                self._user(rec, ts, uid, result)
            else:
                self._queued(rec, ts, uid, result)
        return result

    @staticmethod
    def _assistant(rec: dict, ts, uid: str, result: ParseResult) -> None:
        message = rec.get("message")
        if not isinstance(message, dict) or message.get("model") == "<synthetic>" or rec.get("isApiErrorMessage"):
            return
        texts = _texts(message.get("content")) or []
        text = "\n\n".join(t.strip() for t in texts if t.strip())
        if text:
            result.messages.append(Message(role=Role.ASSISTANT, text=sanitize(text), timestamp=ts, uid=uid))
            result.state["last_user"] = None

    @staticmethod
    def _user(rec: dict, ts, uid: str, result: ParseResult) -> None:
        if rec.get("isMeta") or rec.get("isCompactSummary") or rec.get("isVisibleInTranscriptOnly"):
            return
        origin = rec.get("origin")
        if isinstance(origin, dict) and origin.get("kind") not in (None, "human"):
            return
        message = rec.get("message")
        texts = _texts(message.get("content")) if isinstance(message, dict) else None
        if not texts:
            return
        text = "\n\n".join(texts)
        if rec.get("promptSource") == "queued" and _seen_queued(text, result):
            return
        _emit_user(text, ts, uid, result)

    @staticmethod
    def _queued(rec: dict, ts, uid: str, result: ParseResult) -> None:
        """A prompt typed while the agent was busy is logged only as an attachment."""
        att = rec.get("attachment")
        if not isinstance(att, dict) or att.get("type") != "queued_command" or rec.get("isMeta"):
            return
        if att.get("commandMode", "prompt") != "prompt":
            return
        origin = att.get("origin")
        if isinstance(origin, dict) and origin.get("kind") not in (None, "human"):
            return
        if isinstance(att.get("prompt"), str) and not _seen_queued(att["prompt"], result):
            # source_uuid is the user line written if the prompt is later replayed: same uid -> no duplicate.
            _emit_user(att["prompt"], ts, att.get("source_uuid") or uid, result)


def _seen_queued(text: str, result: ParseResult) -> bool:
    """A queued prompt may be logged twice (queued user line + delivery attachment), in either order.

    Returns True for the second copy. Each first copy can absorb exactly one later copy.
    """
    pending: list[str] = result.state.setdefault("queued", [])
    key = text.strip()
    if key in pending:
        pending.remove(key)
        return True
    pending.append(key)
    del pending[:-20]
    return False


def _emit_user(raw: str, ts, uid: str, result: ParseResult) -> None:
    text = human_prompt(raw)
    if text is None:
        return
    text = sanitize(text)
    # A slash command is logged as a wrapper and as raw "/cmd" text back to back.
    if result.state.get("last_user") == text:
        return
    result.state["last_user"] = text
    result.messages.append(Message(role=Role.USER, text=text, timestamp=ts, uid=uid))
