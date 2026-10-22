import json
import shlex
from pathlib import Path

import pytest

from human_history.integrations import herdr
from human_history.integrations import resume as rs
from human_history.storage.search import SessionRow


def session(agent="claude", sid="11111111-aaaa-4bbb-8ccc-000000000001", cwd="/tmp") -> SessionRow:
    return SessionRow(
        key=f"{agent}:{sid}",
        agent=agent,
        session_id=sid,
        title="T",
        cwd=cwd,
        worktree=None,
        repo=None,
        branch=None,
        created_at=None,
        updated_at=None,
        user_count=1,
        assistant_count=1,
        path="x",
        favorite=False,
    )


def test_resume_commands():
    assert rs.resume_argv("claude", "abc-1") == ["claude", "--resume", "abc-1"]
    assert rs.resume_argv("codex", "abc-1") == ["codex", "resume", "abc-1"]
    with pytest.raises(rs.ResumeError):
        rs.resume_argv("claude", "x; rm -rf /")
    with pytest.raises(rs.ResumeError):
        rs.resume_argv("claude", "--dangerously-skip-permissions")
    with pytest.raises(rs.ResumeError):
        rs.resume_argv("gemini", "abc")


def test_directory_policies_and_confirmation(tmp_path: Path):
    s = session(cwd=str(tmp_path))
    assert rs.directory_for("original", s, "/elsewhere") == str(tmp_path)
    assert rs.directory_for("current", s, "/elsewhere") == "/elsewhere"
    assert rs.directory_for("ask", s, "/elsewhere") is None
    assert not rs.needs_confirmation(s, str(tmp_path))
    assert rs.needs_confirmation(s, "/elsewhere")
    assert rs.needs_confirmation(session(cwd=None), str(tmp_path))


def test_build_plan_validates(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(rs.shutil, "which", lambda name: f"/bin/{name}")
    plan = rs.build_plan(session(cwd=str(tmp_path)), str(tmp_path))
    assert plan.argv == ("claude", "--resume", "11111111-aaaa-4bbb-8ccc-000000000001")
    assert plan.directory == str(tmp_path.resolve())
    with pytest.raises(rs.ResumeError, match="does not exist"):
        rs.build_plan(session(), str(tmp_path / "missing"))
    monkeypatch.setattr(rs.shutil, "which", lambda name: None)
    with pytest.raises(rs.ResumeError, match="not installed"):
        rs.build_plan(session(agent="codex"), str(tmp_path))


class FakeHerdr:
    def __init__(self, panes=None):
        self.calls: list[tuple[str, ...]] = []
        self.panes = panes or []
        self.open_plugin_panes: set[str] = set()

    def __call__(self, *args):
        self.calls.append(args)
        if args[:2] == ("pane", "list"):
            return {"result": {"panes": self.panes}}
        if args[:2] == ("tab", "create"):
            return {"result": {"tab": {"tab_id": "w1:t9"}, "root_pane": {"pane_id": "w1:p9"}}}
        if args[:2] == ("pane", "split"):
            return {"result": {"pane": {"pane_id": "w1:p8"}}}
        if args[:3] == ("plugin", "pane", "open"):
            return {"result": {"type": "plugin_pane_opened", "plugin_pane": {"pane": {"pane_id": "w1:p7"}}}}
        if args[:3] == ("plugin", "pane", "close") and args[3] not in self.open_plugin_panes:
            raise herdr.HerdrError("pane not found")
        if args[:2] == ("pane", "process-info"):
            return {"result": {"process_info": {"foreground_processes": [{"argv": ["codex", "resume", "S1"]}]}}}
        return {}


PLAN = rs.ResumePlan("claude", "S1", "/tmp/my dir", ("claude", "--resume", "S1"), "Title")


def test_herdr_resume_new_tab(monkeypatch):
    fake = FakeHerdr()
    monkeypatch.setattr(herdr, "call", fake)
    assert herdr.resume(PLAN, "tab", "w1", "w1:p1") == "w1:p9"
    create = next(c for c in fake.calls if c[:2] == ("tab", "create"))
    assert create[create.index("--cwd") + 1] == "/tmp/my dir" and "--focus" in create
    assert create[create.index("--workspace") + 1] == "w1"
    assert fake.calls[-1] == ("pane", "run", "w1:p9", "claude --resume S1")


def test_herdr_resume_split(monkeypatch):
    fake = FakeHerdr()
    monkeypatch.setattr(herdr, "call", fake)
    assert herdr.resume(PLAN, "split-down", "w1", "w1:p1") == "w1:p8"
    split = next(c for c in fake.calls if c[:2] == ("pane", "split"))
    assert split[2] == "w1:p1" and split[split.index("--direction") + 1] == "down"


def test_herdr_focuses_running_session_instead_of_duplicating(monkeypatch):
    fake = FakeHerdr(
        panes=[
            {"pane_id": "w1:p2", "agent": "claude", "agent_session": {"agent": "claude", "value": "OTHER"}},
            {"pane_id": "w1:p3", "agent": "claude", "agent_session": {"agent": "claude", "value": "S1"}},
        ]
    )
    monkeypatch.setattr(herdr, "call", fake)
    assert herdr.resume(PLAN, "tab", "w1", None) == "w1:p3"
    assert ("agent", "focus", "w1:p3") in fake.calls
    assert not any(c[:2] == ("tab", "create") for c in fake.calls)


def test_herdr_detects_codex_by_argv(monkeypatch):
    fake = FakeHerdr(panes=[{"pane_id": "w1:p4", "agent": "codex"}])
    monkeypatch.setattr(herdr, "call", fake)
    assert herdr.running_pane("codex", "S1") == "w1:p4"


def test_pane_run_text_is_shell_quoted():
    plan = rs.ResumePlan("claude", "S1", "/", ("claude", "--resume", "S1; touch pwned $(id)"), "")
    typed = shlex.join(plan.argv)
    assert shlex.split(typed) == list(plan.argv)


def test_open_history_overlay(monkeypatch):
    fake = FakeHerdr()
    monkeypatch.setattr(herdr, "call", fake)
    herdr.open_history("overlay", "/w", {"HH_CWD": "/w", "HH_FILTER": "current-worktree"}, "w1", "w1:p1", "90%")
    args = fake.calls[0]
    assert args[:3] == ("plugin", "pane", "open")
    assert args[args.index("--placement") + 1] == "overlay"
    assert "--target-pane" not in args and "--workspace" not in args  # Herdr rejects both for overlays
    assert "HH_FILTER=current-worktree" in args and args[-1] == "--focus"


def test_history_pane_toggles(monkeypatch, tmp_path):
    fake = FakeHerdr()
    fake.open_plugin_panes = {"w1:p7"}
    monkeypatch.setattr(herdr, "call", fake)
    monkeypatch.setenv("HERDR_PLUGIN_STATE_DIR", str(tmp_path))
    assert not herdr.close_history()
    herdr.open_history("overlay", "/w", {}, None, None, "90%")
    assert herdr.close_history()
    assert fake.calls[-1] == ("plugin", "pane", "close", "w1:p7")
    assert not herdr.close_history()  # the second press after closing opens again
    herdr.open_history("overlay", "/w", {}, None, None, "90%")
    fake.open_plugin_panes = set()  # the user quit the browser with q
    assert not herdr.close_history()


def test_plugin_context_parsing(monkeypatch):
    monkeypatch.setenv("HERDR_PLUGIN_CONTEXT_JSON", json.dumps({"focused_pane_cwd": "/a"}))
    assert herdr.plugin_context()["focused_pane_cwd"] == "/a"
    monkeypatch.setenv("HERDR_PLUGIN_CONTEXT_JSON", "not json")
    assert herdr.plugin_context() == {}


def test_plugin_manifest_actions():
    import tomllib

    manifest = tomllib.loads((Path(__file__).parents[1] / "herdr-plugin" / "herdr-plugin.toml").read_text())
    assert manifest["id"] == "human-history"
    assert {a["id"] for a in manifest["actions"]} == {
        "open",
        "recent-here",
        "search",
        "resume-last",
        "refresh-index",
    }
    from human_history.cli import HERDR_ACTIONS

    assert set(HERDR_ACTIONS) == {a["id"] for a in manifest["actions"]}
