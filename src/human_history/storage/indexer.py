"""Incremental indexing of agent log files into SQLite.

Per file we remember size, mtime, inode, the first bytes and the byte offset of the last
complete line. Unchanged files are skipped by ``stat`` alone; appended files are parsed from
the stored offset; truncated or rewritten files are re-parsed from scratch.
"""

from __future__ import annotations

import json
import os
import sqlite3
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from human_history.config import Config
from human_history.models import Agent, Message, ParseResult
from human_history.parsers import PARSERS
from human_history.parsers.base import Parser, first_line_title
from human_history.storage.database import iso
from human_history.utils.git import locate

HEAD_BYTES = 256
BATCH_FILES = 200


@dataclass
class IndexStats:
    scanned: int = 0
    parsed: int = 0
    messages: int = 0
    skipped_lines: int = 0
    removed_sessions: int = 0


def roots_for(config: Config, agent: Agent) -> list[Path]:
    return [Path(p).expanduser() for p in getattr(config.paths, agent.value)]


def index_all(
    conn: sqlite3.Connection,
    config: Config,
    progress: Callable[[IndexStats], None] | None = None,
) -> IndexStats:
    stats = IndexStats()
    seen: set[str] = set()
    for agent, parser in PARSERS.items():
        roots = roots_for(config, agent)
        pending = 0
        for path in parser.discover(roots):
            seen.add(str(path))
            stats.scanned += 1
            if index_file(conn, parser, path, stats):
                pending += 1
                if pending >= BATCH_FILES:
                    conn.commit()
                    pending = 0
                    if progress:
                        progress(stats)
        _apply_agent_titles(conn, agent, parser.titles(roots))
        conn.commit()
    stats.removed_sessions = _forget_missing(conn, seen)
    conn.commit()
    if progress:
        progress(stats)
    return stats


def index_file(conn: sqlite3.Connection, parser: Parser, path: Path, stats: IndexStats) -> bool:
    """Index new content of one file. Returns True if anything was parsed. Caller commits."""
    try:
        st = path.stat()
        with path.open("rb") as fh:
            head = fh.read(HEAD_BYTES)
    except OSError:
        return False
    row = conn.execute("SELECT * FROM files WHERE path = ?", (str(path),)).fetchone()
    if row and row["size"] == st.st_size and row["mtime_ns"] == st.st_mtime_ns and row["inode"] == st.st_ino:
        return False

    offset, state, key = 0, {}, None
    if row:
        key = row["session_key"]
        old_head = bytes(row["head"] or b"")
        rewritten = (
            st.st_size < row["offset"] or row["inode"] != st.st_ino or head[: len(old_head)] != old_head[: len(head)]
        )
        if rewritten and key:
            _reset_session(conn, key)
        elif not rewritten:
            offset, state = row["offset"], json.loads(row["state"] or "{}")

    result = parser.parse(path, offset, state)
    stats.parsed += 1
    stats.skipped_lines += result.skipped_lines
    key = _session_key(parser.agent, result, key, path)
    stats.messages += _store(conn, parser.agent, key, path, result)
    conn.execute(
        """INSERT OR REPLACE INTO files(path, agent, session_key, size, mtime_ns, inode, offset, head, state)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        (
            str(path),
            parser.agent.value,
            key,
            st.st_size,
            st.st_mtime_ns,
            st.st_ino,
            result.offset,
            head,
            json.dumps(result.state),
        ),
    )
    return True


def _session_key(agent: Agent, result: ParseResult, previous: str | None, path: Path) -> str:
    if result.session.session_id:
        return f"{agent.value}:{result.session.session_id}"
    return previous or f"{agent.value}:{path.stem}"


def _reset_session(conn: sqlite3.Connection, key: str) -> None:
    """A file of this session was rewritten: drop its messages and re-read all its files."""
    conn.execute("DELETE FROM messages WHERE session_key = ?", (key,))
    conn.execute("UPDATE files SET offset = 0, size = -1, state = '{}', head = NULL WHERE session_key = ?", (key,))


def _store(conn: sqlite3.Connection, agent: Agent, key: str, path: Path, result: ParseResult) -> int:
    s = result.session
    session_id = key.split(":", 1)[1]
    exists = conn.execute("SELECT 1 FROM sessions WHERE key = ?", (key,)).fetchone()
    if not exists:
        conn.execute(
            "INSERT INTO sessions(key, agent, session_id, path) VALUES (?, ?, ?, ?)",
            (key, agent.value, session_id, str(path)),
        )
    if s.cwd:
        loc = locate(s.cwd)
        conn.execute(
            "UPDATE sessions SET cwd = ?, worktree = ?, repo = ? WHERE key = ?",
            (s.cwd, loc.worktree, loc.repo, key),
        )
    conn.execute(
        """UPDATE sessions SET
             path = ?,
             branch = COALESCE(?, branch),
             agent_title = COALESCE(?, agent_title),
             created_at = CASE WHEN created_at IS NULL OR ? < created_at THEN ? ELSE created_at END,
             updated_at = CASE WHEN updated_at IS NULL OR ? > updated_at THEN ? ELSE updated_at END
           WHERE key = ?""",
        (
            str(path),
            s.branch,
            s.title,
            iso(s.created_at or _first_ts(result.messages)),
            iso(s.created_at or _first_ts(result.messages)),
            iso(s.updated_at),
            iso(s.updated_at),
            key,
        ),
    )
    inserted = 0
    for m in result.messages:
        cur = conn.execute(
            "INSERT OR IGNORE INTO messages(session_key, role, ts, uid, text) VALUES (?, ?, ?, ?, ?)",
            (key, m.role.value, iso(m.timestamp), m.uid, m.text),
        )
        inserted += cur.rowcount
    if inserted or not exists or s.title:
        _refresh_derived(conn, key)
    return inserted


def _first_ts(messages: list[Message]):
    return next((m.timestamp for m in messages if m.timestamp), None)


def _refresh_derived(conn: sqlite3.Connection, key: str) -> None:
    counts = dict(
        conn.execute("SELECT role, COUNT(*) FROM messages WHERE session_key = ? GROUP BY role", (key,)).fetchall()
    )
    first = conn.execute(
        "SELECT text FROM messages WHERE session_key = ? AND role = 'user' ORDER BY ts, id LIMIT 1", (key,)
    ).fetchone()
    conn.execute(
        """UPDATE sessions SET user_count = ?, assistant_count = ?,
             title = COALESCE(agent_title, ?, title) WHERE key = ?""",
        (
            counts.get("user", 0),
            counts.get("assistant", 0),
            first_line_title(first[0]) if first else None,
            key,
        ),
    )


def _apply_agent_titles(conn: sqlite3.Connection, agent: Agent, titles: dict[str, str]) -> None:
    for session_id, title in titles.items():
        conn.execute(
            "UPDATE sessions SET agent_title = ?, title = ? WHERE key = ? AND agent_title IS NOT ?",
            (title, title, f"{agent.value}:{session_id}", title),
        )


def _forget_missing(conn: sqlite3.Connection, seen: set[str]) -> int:
    """Drop index entries whose source files disappeared (original history was deleted)."""
    gone = [r[0] for r in conn.execute("SELECT path FROM files") if r[0] not in seen and not os.path.exists(r[0])]
    if not gone:
        return 0
    conn.executemany("DELETE FROM files WHERE path = ?", [(p,) for p in gone])
    orphans = [
        r[0]
        for r in conn.execute(
            "SELECT key FROM sessions WHERE key NOT IN (SELECT session_key FROM files WHERE session_key IS NOT NULL)"
        )
    ]
    for key in orphans:
        conn.execute("DELETE FROM messages WHERE session_key = ?", (key,))
        conn.execute("DELETE FROM sessions WHERE key = ?", (key,))
    return len(orphans)
