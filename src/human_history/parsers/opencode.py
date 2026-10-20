"""OpenCode (1.2+): every session lives in one SQLite database, ``~/.local/share/opencode/opencode.db``.

Tables ``session`` (``directory``, ``title``, ``parent_id``), ``message`` and ``part`` (JSON in
``data``). Only ``text`` parts are kept: in ``user`` messages unless flagged ``synthetic`` or
``ignored`` (injected file contents, reminders, auto-continue), in ``assistant`` messages unless
the message is a compaction summary. Reasoning, tool, file, patch, step and subtask parts are
ignored. Child (subagent) sessions are skipped: their prompts were written by an agent.

Unlike the JSONL agents there is no byte offset: ``parse_store`` returns the sessions changed
since the ``state`` watermark, read whole (messages are de-duplicated by id when stored).
Assistant text is taken only from completed messages, since parts are rewritten while streaming.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from human_history.models import Agent, Message, ParseResult, Role, SessionPatch, sanitize

DEFAULT_TITLE_PREFIXES = ("New session - ", "Child session - ")
STALE = timedelta(minutes=10)


class OpenCodeParser:
    agent = Agent.OPENCODE
    multi_session = True  # one file holds many sessions: the indexer calls parse_store()

    def discover(self, roots: list[Path]) -> Iterator[Path]:
        for root in roots:
            if root.is_file():
                yield root
            elif root.is_dir():
                yield from root.glob("opencode*.db")

    def titles(self, roots: list[Path]) -> dict[str, str]:
        return {}  # titles come with each session

    def parse_store(self, path: Path, state: dict) -> tuple[list[ParseResult], dict]:
        since = int(state.get("since", 0))
        try:
            conn = sqlite3.connect(f"{path.resolve().as_uri()}?mode=ro", uri=True, timeout=2)
        except sqlite3.Error:
            return [], state
        conn.row_factory = sqlite3.Row
        try:
            # ponytail: sessions deleted inside OpenCode stay in the index until `hh index --rebuild`.
            changed = conn.execute(
                """SELECT * FROM session WHERE parent_id IS NULL AND (time_updated > :since
                     OR id IN (SELECT session_id FROM message WHERE time_updated > :since)
                     OR id IN (SELECT session_id FROM part WHERE time_updated > :since))""",
                {"since": since},
            ).fetchall()
            results = [self._session(conn, row) for row in changed]
            newest = conn.execute(
                "SELECT MAX(t) FROM (SELECT MAX(time_updated) t FROM session UNION ALL "
                "SELECT MAX(time_updated) FROM message UNION ALL SELECT MAX(time_updated) FROM part)"
            ).fetchone()[0]
        except sqlite3.Error:
            return [], state  # locked, or a schema this version does not know: retry next run
        finally:
            conn.close()
        # An unfinished answer is not stored yet: start from before it next time.
        pending = [p for r in results for p in r.state.get("pending", [])]
        return results, {"since": min([newest or since, *pending])}

    def _session(self, conn: sqlite3.Connection, row: sqlite3.Row) -> ParseResult:
        title = row["title"] or ""
        result = ParseResult(
            session=SessionPatch(
                session_id=row["id"],
                cwd=row["directory"] or None,
                title=None if not title or title.startswith(DEFAULT_TITLE_PREFIXES) else sanitize(title),
                created_at=_ms(row["time_created"]),
                updated_at=_ms(row["time_updated"]),
            )
        )
        parts: dict[str, list[dict[str, Any]]] = {}
        for p in conn.execute("SELECT message_id, data FROM part WHERE session_id = ? ORDER BY id", (row["id"],)):
            parts.setdefault(p["message_id"], []).append(_json(p["data"]))
        for m in conn.execute(
            "SELECT id, time_created, time_updated, data FROM message WHERE session_id = ? ORDER BY time_created, id",
            (row["id"],),
        ):
            data = _json(m["data"])
            role = data.get("role")
            texts = [
                p["text"]
                for p in parts.get(m["id"], [])
                if p.get("type") == "text"
                and isinstance(p.get("text"), str)
                and not p.get("synthetic")
                and not p.get("ignored")
            ]
            if role == "assistant":
                if data.get("summary") is True or "compaction" in (data.get("mode"), data.get("agent")):
                    continue
                if not (data.get("time") or {}).get("completed") and not data.get("error"):
                    if _ms(m["time_updated"]) > datetime.now(UTC) - STALE:  # an abandoned one never completes
                        result.state.setdefault("pending", []).append(m["time_updated"] - 1)
                    continue
            elif role != "user":
                continue
            text = "\n\n".join(t.strip() for t in texts if t.strip())
            if text:
                result.messages.append(
                    Message(
                        role=Role.USER if role == "user" else Role.ASSISTANT,
                        text=sanitize(text),
                        timestamp=_ms(m["time_created"]),
                        uid=m["id"],
                    )
                )
        return result


def _json(raw: Any) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (TypeError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _ms(value: Any) -> datetime | None:
    return datetime.fromtimestamp(value / 1000, UTC) if isinstance(value, int | float) else None
