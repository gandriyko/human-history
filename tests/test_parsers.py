from pathlib import Path

from conftest import CLAUDE_FILE, CODEX_FILE, FIXTURES, FORBIDDEN

from human_history.models import Role
from human_history.parsers.claude import ClaudeParser, human_prompt
from human_history.parsers.codex import CodexParser


def pairs(result):
    return [(m.role.value, m.text) for m in result.messages]


# ------------------------------------------------------------------ Claude Code
def test_claude_keeps_only_human_conversation():
    result = ClaudeParser().parse(FIXTURES / "claude" / CLAUDE_FILE)
    assert pairs(result) == [
        ("user", "Optimize SQLAlchemy queries and fix the N+1 problem in the approval service."),
        ("assistant", "I found three inefficient queries in the approval service."),
        (
            "assistant",
            "I updated the queries to use `joinedload` and added regression tests.]52;c;ZXZpbA==[31m",
        ),
        ("user", "/review PR 42"),
        ("user", "`! git status`"),
        ("user", "Are all tests passing?"),
        ("assistant", "Yes. All 24 approval service tests passed successfully."),
    ]


def test_claude_never_leaks_technical_content():
    result = ClaudeParser().parse(FIXTURES / "claude" / CLAUDE_FILE)
    blob = "\n".join(m.text for m in result.messages)
    for marker in FORBIDDEN:
        assert marker not in blob
    assert "\x1b" not in blob and "\x07" not in blob  # terminal escape injection stripped


def test_claude_metadata_and_titles():
    result = ClaudeParser().parse(FIXTURES / "claude" / CLAUDE_FILE)
    s = result.session
    assert s.session_id == "11111111-aaaa-4bbb-8ccc-000000000001"
    assert s.cwd == "/work/app"
    assert s.branch == "feature/approvals"
    assert s.title == "Approvals perf"  # custom title beats earlier and later ai-title
    assert s.created_at.isoformat().startswith("2026-10-01T09:59")
    assert s.updated_at.isoformat().startswith("2026-10-01T10:00:25")
    assert result.skipped_lines == 1
    # queued prompt: uid is the source line so a replayed copy can never duplicate it
    assert [m.uid for m in result.messages if m.text == "Are all tests passing?"] == ["u-queued-src"]


def test_claude_incremental_parse_and_partial_line(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    data = (FIXTURES / "claude" / CLAUDE_FILE).read_bytes()
    assert not data.endswith(b"\n"), "fixture must end with an unfinished line"
    path.write_bytes(data)
    parser = ClaudeParser()
    first = parser.parse(path)
    assert "still being written" not in [m.text for m in first.messages]
    assert first.offset == len(data) - len(data.splitlines()[-1])

    with path.open("ab") as fh:
        fh.write(b"\n")
    second = parser.parse(path, first.offset, first.state)
    assert pairs(second) == [("user", "still being written")]
    assert second.session.title is None  # title lines were consumed in the first run


def test_claude_prompt_classifier():
    assert human_prompt("<command-name>/clear</command-name>") == "/clear"
    assert human_prompt("[Request interrupted by user]") is None
    assert human_prompt(' <pasted_content id="ab12">big log</pasted_content id="ab12">') == "big log"
    assert human_prompt("<local-command-stdout>x</local-command-stdout>") is None
    assert human_prompt("<b>html is fine</b>") == "<b>html is fine</b>"


def test_claude_discovery_skips_subagents(tmp_path: Path):
    (tmp_path / "proj" / "sess" / "subagents").mkdir(parents=True)
    (tmp_path / "proj" / "sess.jsonl").write_text("")
    (tmp_path / "proj" / "sess" / "subagents" / "agent-1.jsonl").write_text("")
    assert [p.name for p in ClaudeParser().discover([tmp_path])] == ["sess.jsonl"]


# ------------------------------------------------------------------ Codex
def test_codex_wrapped_format_dedupes_and_filters():
    result = CodexParser().parse(FIXTURES / "codex" / "sessions" / CODEX_FILE)
    assert pairs(result) == [
        ("user", "Explain the approval workflow"),
        ("assistant", "Let me read the service first."),
        ("assistant", "The approval workflow has three levels."),
        ("user", "Add an index on approval_level"),
        ("assistant", "Added a composite index."),
    ]
    blob = "\n".join(m.text for m in result.messages)
    for marker in FORBIDDEN:
        assert marker not in blob
    s = result.session
    assert (s.session_id, s.cwd, s.branch) == ("22222222-bbbb-4ccc-8ddd-000000000002", "/work/svc", "main")
    assert result.skipped_lines == 1


def test_codex_legacy_bare_format():
    path = (
        FIXTURES
        / "codex"
        / "sessions"
        / "2025/07/01/rollout-2025-07-01T10-00-00-33333333-cccc-4ddd-8eee-000000000003.jsonl"
    )
    result = CodexParser().parse(path)
    assert pairs(result) == [("user", "Rename the legacy module"), ("assistant", "Renamed it.")]
    assert result.session.session_id == "33333333-cccc-4ddd-8eee-000000000003"
    assert result.session.cwd == "/work/old"
    assert result.session.branch == "legacy"


def test_codex_titles_latest_wins():
    titles = CodexParser().titles([FIXTURES / "codex" / "sessions"])
    assert titles == {"22222222-bbbb-4ccc-8ddd-000000000002": "Approval workflow"}


def test_codex_incremental(tmp_path: Path):
    src = (FIXTURES / "codex" / "sessions" / CODEX_FILE).read_bytes().splitlines(keepends=True)
    path = tmp_path / "rollout.jsonl"
    path.write_bytes(b"".join(src[:8]))
    parser = CodexParser()
    first = parser.parse(path)
    assert [m.role for m in first.messages] == [Role.USER]
    path.write_bytes(b"".join(src))
    rest = parser.parse(path, first.offset, first.state)
    assert rest.messages[0].text == "Let me read the service first."
    assert len(first.messages) + len(rest.messages) == 5
