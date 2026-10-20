"""Session listing, scoped FTS5 search and fuzzy title matching."""

from __future__ import annotations

import re
import sqlite3
from dataclasses import dataclass, field, replace
from pathlib import Path

from human_history.config import ProjectFilter, Scope, Sort
from human_history.utils.git import locate

HL_START, HL_END = "\x02", "\x03"  # snippet markers; control chars never survive sanitize()

ORDER = {
    "last_activity_desc": "s.updated_at DESC",
    "created_desc": "s.created_at DESC",
    "title_asc": "s.title COLLATE NOCASE ASC",
}
ROLES = {"user": ("user",), "assistant": ("assistant",), "both": ("user", "assistant")}


@dataclass
class Filters:
    project: ProjectFilter = "all"
    cwd: str | None = None  # where the user is; resolves current worktree / repository
    exact_cwd: str | None = None  # sessions started in exactly this directory
    agent: str | None = None
    branch: str | None = None
    since: str | None = None  # ISO date/time, compared with last activity
    until: str | None = None
    favorites_only: bool = False

    def where(self) -> tuple[str, list]:
        clauses, params = [], []
        if self.project != "all" and self.cwd:
            loc = locate(self.cwd)
            if self.project == "repository" and loc.repo:
                clauses.append("s.repo = ?")
                params.append(loc.repo)
            else:
                # Exact worktree. Sessions whose directory no longer exists match by path prefix.
                root = loc.worktree or self.cwd.rstrip("/")
                clauses.append("(s.worktree = ? OR (s.worktree IS NULL AND (s.cwd = ? OR substr(s.cwd, 1, ?) = ?)))")
                params += [root, root, len(root) + 1, root + "/"]
        if self.exact_cwd:
            clauses.append("s.cwd = ?")
            params.append(self.exact_cwd.rstrip("/"))
        for column, value in (("s.agent", self.agent), ("s.branch", self.branch)):
            if value:
                clauses.append(f"{column} = ?")
                params.append(value)
        if self.since:
            clauses.append("s.updated_at >= ?")
            params.append(self.since)
        if self.until:
            clauses.append("s.updated_at < ?")
            params.append(self.until)
        if self.favorites_only:
            clauses.append("f.session_key IS NOT NULL")
        return (" AND ".join(clauses) or "1"), params


@dataclass
class SessionRow:
    key: str
    agent: str
    session_id: str
    title: str
    cwd: str | None
    worktree: str | None
    repo: str | None
    branch: str | None
    created_at: str | None
    updated_at: str | None
    user_count: int
    assistant_count: int
    path: str
    favorite: bool
    # Set for search results: best matching message and its snippet.
    hit_message_id: int | None = None
    hit_role: str | None = None
    snippet: str | None = None
    hits: int = 0
    match_ids: list[int] = field(default_factory=list)


_SESSION_COLUMNS = """s.key, s.agent, s.session_id, COALESCE(s.title, '(untitled)') AS title, s.cwd, s.worktree,
    s.repo, s.branch, s.created_at, s.updated_at, s.user_count, s.assistant_count, s.path,
    f.session_key IS NOT NULL AS favorite"""
_FROM = "sessions s LEFT JOIN favorites f ON f.session_key = s.key"


def _row(r: sqlite3.Row) -> SessionRow:
    return SessionRow(**{k: r[k] for k in r.keys() if k in SessionRow.__dataclass_fields__})  # noqa: SIM118 (Row)


def list_sessions(
    conn: sqlite3.Connection, filters: Filters, sort: Sort = "last_activity_desc", limit: int = 200
) -> list[SessionRow]:
    where, params = filters.where()
    sql = (
        f"SELECT {_SESSION_COLUMNS} FROM {_FROM} WHERE {where} AND (s.user_count + s.assistant_count) > 0 "
        f"ORDER BY favorite DESC, {ORDER[sort]} LIMIT ?"
    )
    return [_row(r) for r in conn.execute(sql, [*params, limit])]


_FILTER_TOKEN = re.compile(r"(?<!\S)(agent|branch|since|until|cwd):(\S+)")


def split_filters(text: str, filters: Filters) -> tuple[str, Filters]:
    """Pull ``agent: branch: since: until: cwd:`` tokens out of a search string."""
    found = dict(_FILTER_TOKEN.findall(text))
    if not found:
        return text, filters
    rest = _FILTER_TOKEN.sub("", text).strip()
    values = {k: v for k, v in found.items() if k != "cwd"}
    if "cwd" in found:
        values["exact_cwd"] = str((Path(filters.cwd or ".") / Path(found["cwd"]).expanduser()).resolve())
    return rest, replace(filters, **values)


_TOKEN = re.compile(r'"([^"]+)"|(\S+)')


def fts_query(text: str) -> str | None:
    """User text -> safe FTS5 query: AND of quoted terms; the last bare word is a prefix."""
    parts = []
    for phrase, word in _TOKEN.findall(text):
        term = (phrase or word).replace('"', "").strip()
        if term:
            parts.append((term, bool(phrase)))
    if not parts:
        return None
    out = [f'"{t}"' for t, _ in parts]
    if not parts[-1][1]:
        out[-1] += "*"
    return " ".join(out)


def search(
    conn: sqlite3.Connection,
    query: str,
    scope: Scope,
    filters: Filters,
    sort: Sort = "last_activity_desc",
    limit: int = 200,
) -> list[SessionRow]:
    """Sessions matching ``query`` in the given scope, best hit first."""
    if scope == "title":
        return search_titles(conn, query, filters, sort, limit)
    match = fts_query(query)
    if not match:
        return list_sessions(conn, filters, sort, limit)
    where, params = filters.where()
    roles = ROLES[scope]
    sql = f"""
        SELECT m.session_key AS key, m.id, m.role,
               snippet(messages_fts, 0, '{HL_START}', '{HL_END}', '…', 16) AS snip
        FROM messages_fts
        JOIN messages m ON m.id = messages_fts.rowid
        JOIN sessions s ON s.key = m.session_key
        LEFT JOIN favorites f ON f.session_key = s.key
        WHERE messages_fts MATCH ? AND m.role IN ({",".join("?" * len(roles))}) AND {where}
        ORDER BY bm25(messages_fts)
        LIMIT 5000"""
    hits = conn.execute(sql, [match, *roles, *params]).fetchall()
    best: dict[str, sqlite3.Row] = {}
    ids: dict[str, list[int]] = {}
    for h in hits:
        best.setdefault(h["key"], h)
        ids.setdefault(h["key"], []).append(h["id"])
    if not best:
        return []
    keys = list(best)[:limit]
    rows = {
        r["key"]: _row(r)
        for r in conn.execute(
            f"SELECT {_SESSION_COLUMNS} FROM {_FROM} WHERE s.key IN ({','.join('?' * len(keys))})", keys
        )
    }
    out = []
    for key in keys:
        row = rows[key]
        h = best[key]
        row.hit_message_id, row.hit_role, row.snippet = h["id"], h["role"], h["snip"]
        row.match_ids = sorted(ids[key])
        row.hits = len(row.match_ids)
        out.append(row)
    return out


def fuzzy_score(query: str, title: str) -> int | None:
    """Higher is better; ``None`` = no match. Substrings beat in-order subsequences."""
    q, t = query.lower().strip(), title.lower()
    if not q:
        return 0
    if q in t:
        return 10_000 - t.index(q)
    words = q.split()
    if len(words) > 1 and all(w in t for w in words):
        return 5_000
    pos, gaps = -1, 0
    for ch in q.replace(" ", ""):
        i = t.find(ch, pos + 1)
        if i < 0:
            return None
        gaps += i - pos - 1
        pos = i
    return 1_000 - gaps


def search_titles(
    conn: sqlite3.Connection,
    query: str,
    filters: Filters,
    sort: Sort = "last_activity_desc",
    limit: int = 200,
) -> list[SessionRow]:
    # ponytail: scores every filtered title in Python; fine for tens of thousands of sessions.
    rows = list_sessions(conn, filters, sort, limit=100_000)
    scored = [(score, i, r) for i, r in enumerate(rows) if (score := fuzzy_score(query, r.title)) is not None]
    scored.sort(key=lambda x: (-x[0], x[1]))
    return [r for _, _, r in scored[:limit]]


def projects(conn: sqlite3.Connection) -> list[str]:
    """Distinct project roots (worktree, else cwd) that have sessions, most recently active first."""
    return [
        r[0]
        for r in conn.execute(
            """SELECT COALESCE(worktree, cwd) AS root FROM sessions
               WHERE root IS NOT NULL AND user_count + assistant_count > 0
               GROUP BY root ORDER BY MAX(updated_at) DESC"""
        )
    ]


def messages(conn: sqlite3.Connection, key: str) -> list[sqlite3.Row]:
    return conn.execute(
        "SELECT id, role, ts, text FROM messages WHERE session_key = ? ORDER BY ts, id", (key,)
    ).fetchall()


def get_session(conn: sqlite3.Connection, key: str) -> SessionRow | None:
    r = conn.execute(f"SELECT {_SESSION_COLUMNS} FROM {_FROM} WHERE s.key = ?", (key,)).fetchone()
    return _row(r) if r else None


def toggle_favorite(conn: sqlite3.Connection, key: str) -> bool:
    with conn:
        if conn.execute("DELETE FROM favorites WHERE session_key = ?", (key,)).rowcount:
            return False
        conn.execute("INSERT INTO favorites(session_key) VALUES (?)", (key,))
        return True
