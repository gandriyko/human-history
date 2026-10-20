"""OpenCode, GitHub Copilot CLI and pi: only human prompts and visible answers reach the index."""

import json
import sqlite3
from datetime import UTC, datetime

from conftest import FORBIDDEN

from human_history.integrations.resume import resume_argv
from human_history.storage.indexer import index_all

NOW = int(datetime.now(UTC).timestamp() * 1000)


def opencode_db(path):
    conn = sqlite3.connect(path)
    conn.executescript(
        """PRAGMA journal_mode=WAL;
        CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, title TEXT,
                              time_created INTEGER, time_updated INTEGER);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER,
                              data TEXT);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER,
                           time_updated INTEGER, data TEXT);"""
    )
    return conn


def add(conn, table, row):
    row = {k: json.dumps(v) if k == "data" else v for k, v in row.items()}
    conn.execute(
        f"INSERT OR REPLACE INTO {table} ({', '.join(row)}) VALUES ({', '.join('?' * len(row))})", list(row.values())
    )
    conn.commit()


def seed_opencode(conn):
    t = NOW - 60_000
    add(
        conn,
        "session",
        {"id": "ses_1", "directory": "/work/app", "title": "Fix login", "time_created": t, "time_updated": t},
    )
    add(
        conn,
        "session",
        {
            "id": "ses_child",
            "parent_id": "ses_1",
            "directory": "/work/app",
            "title": "Child session - x",
            "time_created": t,
            "time_updated": t,
        },
    )
    add(
        conn,
        "message",
        {
            "id": "msg_1",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"role": "user", "time": {"created": t}},
        },
    )
    add(
        conn,
        "part",
        {
            "id": "prt_1",
            "message_id": "msg_1",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"type": "text", "text": "fix the login bug"},
        },
    )
    add(
        conn,
        "part",
        {
            "id": "prt_2",
            "message_id": "msg_1",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"type": "text", "text": "Called the Read tool SYSTEMSECRET", "synthetic": True},
        },
    )
    add(
        conn,
        "part",
        {
            "id": "prt_3",
            "message_id": "msg_1",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"type": "file", "filename": "TOOLSECRET"},
        },
    )
    add(
        conn,
        "message",
        {
            "id": "msg_2",
            "session_id": "ses_1",
            "time_created": t + 1,
            "time_updated": t + 2,
            "data": {"role": "assistant", "time": {"created": t + 1, "completed": t + 2}},
        },
    )
    for i, part in enumerate(
        [
            {"type": "reasoning", "text": "REASONINGSECRET"},
            {"type": "tool", "tool": "bash", "state": {"input": "TOOLSECRET", "output": "TOOLSECRET"}},
            {"type": "text", "text": "The token was never refreshed; fixed."},
        ]
    ):
        add(
            conn,
            "part",
            {
                "id": f"prt_a{i}",
                "message_id": "msg_2",
                "session_id": "ses_1",
                "time_created": t + 1,
                "time_updated": t + 1,
                "data": part,
            },
        )
    add(
        conn,
        "message",
        {
            "id": "msg_c",
            "session_id": "ses_child",
            "time_created": t,
            "time_updated": t,
            "data": {"role": "user", "time": {"created": t}},
        },
    )
    add(
        conn,
        "part",
        {
            "id": "prt_c",
            "message_id": "msg_c",
            "session_id": "ses_child",
            "time_created": t,
            "time_updated": t,
            "data": {"type": "text", "text": "SIDECHAINSECRET"},
        },
    )


def conversation(db, key):
    return [
        (r["role"], r["text"])
        for r in db.execute("SELECT role, text FROM messages WHERE session_key = ? ORDER BY ts, id", (key,))
    ]


def test_new_agents_index_only_human_conversation(config, db, logs, tmp_path):
    config.paths.copilot = [str(logs / "copilot" / "session-state")]
    config.paths.pi = [str(logs / "pi" / "sessions")]
    config.paths.opencode = [str(tmp_path / "opencode")]
    (tmp_path / "opencode").mkdir()
    oc = opencode_db(tmp_path / "opencode" / "opencode.db")
    seed_opencode(oc)
    index_all(db, config)

    blob = "\n".join(r[0] for r in db.execute("SELECT text FROM messages"))
    blob += "\n".join(r[0] or "" for r in db.execute("SELECT title FROM sessions"))
    for marker in FORBIDDEN:
        assert marker not in blob

    assert conversation(db, "copilot:44444444-dddd-4eee-8fff-000000000004") == [
        ("user", "Where do we store the SAP version?"),
        ("assistant", "It is stored in the `erp_config` table."),
    ]
    assert conversation(db, "pi:55555555-eeee-4fff-8aaa-000000000005") == [
        ("user", "review the login flow"),
        ("assistant", "Reading the auth module first."),
        ("assistant", "The session token is never refreshed."),
        ("user", "fix it"),
    ]
    assert conversation(db, "opencode:ses_1") == [
        ("user", "fix the login bug"),
        ("assistant", "The token was never refreshed; fixed."),
    ]
    sessions = {r["key"]: r for r in db.execute("SELECT * FROM sessions")}
    assert "opencode:ses_child" not in sessions  # subagent prompts are not human
    assert sessions["copilot:44444444-dddd-4eee-8fff-000000000004"]["title"] == "Find SAP Version Storage"
    assert sessions["pi:55555555-eeee-4fff-8aaa-000000000005"]["title"] == "Login flow review"
    assert sessions["opencode:ses_1"]["title"] == "Fix login"
    assert {s["cwd"] for s in sessions.values()} >= {"/work/app"}
    assert sessions["copilot:44444444-dddd-4eee-8fff-000000000004"]["branch"] == "main"

    # OpenCode incremental: an answer still streaming is held back until it completes
    t = NOW
    add(
        oc,
        "message",
        {
            "id": "msg_3",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"role": "user", "time": {"created": t}},
        },
    )
    add(
        oc,
        "part",
        {
            "id": "prt_5",
            "message_id": "msg_3",
            "session_id": "ses_1",
            "time_created": t,
            "time_updated": t,
            "data": {"type": "text", "text": "and add a test"},
        },
    )
    add(
        oc,
        "message",
        {
            "id": "msg_4",
            "session_id": "ses_1",
            "time_created": t + 1,
            "time_updated": t + 1,
            "data": {"role": "assistant", "time": {"created": t + 1}},
        },
    )
    add(
        oc,
        "part",
        {
            "id": "prt_6",
            "message_id": "msg_4",
            "session_id": "ses_1",
            "time_created": t + 1,
            "time_updated": t + 1,
            "data": {"type": "text", "text": "Adding"},
        },
    )
    index_all(db, config)
    assert conversation(db, "opencode:ses_1")[-1] == ("user", "and add a test")
    add(
        oc,
        "part",
        {
            "id": "prt_6",
            "message_id": "msg_4",
            "session_id": "ses_1",
            "time_created": t + 1,
            "time_updated": t + 3,
            "data": {"type": "text", "text": "Added a regression test."},
        },
    )
    add(
        oc,
        "message",
        {
            "id": "msg_4",
            "session_id": "ses_1",
            "time_created": t + 1,
            "time_updated": t + 3,
            "data": {"role": "assistant", "time": {"created": t + 1, "completed": t + 3}},
        },
    )
    index_all(db, config)
    assert conversation(db, "opencode:ses_1")[-1] == ("assistant", "Added a regression test.")
    assert len(conversation(db, "opencode:ses_1")) == 4

    # the database stays indexed across runs (its sessions are not mistaken for orphans)
    (logs / "copilot" / "session-state" / "44444444-dddd-4eee-8fff-000000000004" / "events.jsonl").unlink()
    index_all(db, config)
    assert db.execute("SELECT COUNT(*) FROM sessions WHERE agent = 'opencode'").fetchone()[0] == 1
    assert db.execute("SELECT COUNT(*) FROM sessions WHERE agent = 'copilot'").fetchone()[0] == 0


def test_resume_commands():
    assert resume_argv("opencode", "ses_1") == ["opencode", "--session", "ses_1"]
    assert resume_argv("copilot", "4444-dd") == ["copilot", "--resume=4444-dd"]
    assert resume_argv("pi", "5555-ee") == ["pi", "--session", "5555-ee"]
