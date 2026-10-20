import asyncio

from conftest import FORBIDDEN
from textual.widgets import Select

from human_history.storage.indexer import index_all
from human_history.tui.app import HistoryApp
from human_history.tui.screens import ResumeDirectoryScreen
from human_history.tui.widgets import Conversation, MessageView, SessionList


def run(coro):
    return asyncio.run(coro)


def make_app(config, db, tmp_path, **kwargs):
    index_all(db, config)
    return HistoryApp(config, str(tmp_path), project_filter="all", **kwargs)


def test_browse_read_and_search(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)

    async def scenario():
        app = make_app(config, db, tmp_path)
        async with app.run_test(size=(120, 40)) as pilot:
            await pilot.pause()
            sessions = app.query_one(SessionList)
            conv = app.query_one(Conversation)
            search = app.query_one("#search")
            home = app.query_one("#home")
            assert len(sessions.rows) == 3
            # only the list is on screen, the search box has focus; nothing is rendered until Enter
            assert home.display and not conv.display and not conv.views()
            assert search.has_focus

            # arrows move the list while typing focus stays in the search box
            await pilot.press("down", "down")
            assert sessions.highlighted == 2 and search.has_focus
            await pilot.press("up")
            assert sessions.highlighted == 1 and search.has_focus
            await pilot.press("ctrl+n")  # fzf-style alternatives
            assert sessions.highlighted == 2
            await pilot.press("ctrl+p", "ctrl+p")
            assert sessions.highlighted == 0 and search.has_focus

            # Enter opens each session full-width; nothing technical is rendered; Esc returns to search
            for index in range(3):
                sessions.highlighted = index
                await pilot.press("enter")
                await pilot.pause(0.3)
                assert conv.display and not home.display
                views = conv.views()
                assert views
                for view in views:
                    for marker in FORBIDDEN:
                        assert marker not in view.text
                await pilot.press("escape")
                assert home.display and not conv.display and search.has_focus

            # Esc: search box -> list (single-key actions work there) -> back to search, cleared
            await pilot.press(*"zzz", "escape")
            assert sessions.has_focus and search.value == "zzz"
            await pilot.press("escape")
            assert search.has_focus and search.value == ""

            # header: agent filter (a) and the project picker for worktree/repository filtering
            app._last_escape = 0  # a pause: a quick second Esc on an empty search would quit
            await pilot.press("escape")  # to the list, where single-key actions work
            await pilot.press("a")
            assert {r.agent for r in sessions.rows} == {"claude"}
            assert app.query_one("#agent", Select).value == "claude"
            await pilot.press("a")
            assert {r.agent for r in sessions.rows} == {"codex"} and len(sessions.rows) == 2
            await pilot.press("a")  # back to all agents
            assert len(sessions.rows) == 3
            app.query_one("#filter", Select).value = "current-worktree"
            app.query_one("#project", Select).value = "/work/svc"  # raises unless it is offered
            await pilot.pause(0.2)
            assert [r.cwd for r in sessions.rows] == ["/work/svc"]
            app.query_one("#filter", Select).value = "all"
            await pilot.pause(0.2)
            assert len(sessions.rows) == 3 and search.has_focus  # picking returns focus to the search box

            # scoped search: prompts only vs answers only
            await pilot.press(*"joinedload")
            await pilot.pause(0.4)
            assert [r.hits for r in sessions.rows] == [1]
            await pilot.press("ctrl+u")
            await pilot.pause(0.2)
            assert sessions.rows == []
            await pilot.press("ctrl+a")
            await pilot.pause(0.2)
            assert len(sessions.rows) == 1

            # Enter straight from the search box opens the hit; focus jumps to the matching message
            await pilot.press("enter")
            await pilot.pause(0.4)
            assert isinstance(app.focused, MessageView) and "joinedload" in app.focused.text

    run(scenario())


def test_vim_search_inside_conversation(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)

    async def scenario():
        app = make_app(config, db, tmp_path, query="SQLAlchemy")
        async with app.run_test(size=(100, 30)) as pilot:
            await pilot.pause(0.3)
            await pilot.press("enter")
            await pilot.pause(0.3)
            conv = app.query_one(Conversation)
            find, bar = app.query_one("#find"), app.query_one("#findbar")
            assert not bar.display

            # "/" opens the find bar; typing searches incrementally (smartcase: lowercase = any case)
            await pilot.press("slash")
            assert bar.display and find.has_focus
            await pilot.press(*"the")
            await pilot.pause(0.1)
            # occurrences, not messages: "fix the N+1 problem in the approval service" has two
            hits = [(v.message_id, k) for v, k, _ in conv.hits()]
            first_id = conv.views()[0].message_id
            assert len(hits) == 4 and hits[:2] == [(first_id, 0), (first_id, 1)]

            # Enter: bar closes, cursor on the first match; n / N walk every occurrence
            await pilot.press("enter")
            await pilot.pause(0.1)
            assert not bar.display
            assert conv.position() == (1, 4)
            assert isinstance(app.focused, MessageView) and app.focused.message_id == first_id
            await pilot.press("n")  # second occurrence in the SAME message
            assert conv.position() == (2, 4) and app.focused.message_id == first_id
            assert app.focused.current == 1
            await pilot.press("n")
            assert conv.position() == (3, 4) and app.focused.message_id != first_id
            await pilot.press("N", "N", "N")
            assert conv.position() == (4, 4)  # wraps around like vim
            assert "[4/4]" in str(app.query_one("#session-title").render())

            # smartcase: a capital letter makes it case-sensitive; Esc cancels and restores
            await pilot.press("slash", *"The")
            await pilot.pause(0.1)
            assert conv.hits() == []
            await pilot.press("escape")
            assert not bar.display and conv.position() == (4, 4)

            # Esc from the conversation goes back to the list
            await pilot.press("escape")
            assert app.query_one("#home").display and not conv.display

    run(scenario())


def test_resume_asks_for_directory_and_returns_plan(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)
    monkeypatch.setattr("human_history.integrations.resume.shutil.which", lambda name: f"/bin/{name}")
    project = tmp_path / "project"
    project.mkdir()

    async def scenario():
        app = HistoryApp(config, str(project), project_filter="all", query="legacy")
        index_all(db, config)
        async with app.run_test(size=(120, 40)) as pilot:
            await pilot.pause(0.3)
            await pilot.press("escape", "r")  # Esc: from the search box to the list, where r resumes
            await pilot.pause(0.2)
            assert isinstance(app.screen, ResumeDirectoryScreen)
            # original directory (/work/old) does not exist -> plan is refused with an error
            await pilot.press("o")
            await pilot.pause(0.2)
            assert app.return_value is None and not isinstance(app.screen, ResumeDirectoryScreen)
            # current worktree is explicit and exists -> app exits with the plan
            await pilot.press("r")
            await pilot.pause(0.2)
            await pilot.press("c")
            await pilot.pause(0.2)
        return app.return_value

    plan = run(scenario())
    assert plan is not None
    assert plan.argv == ("codex", "resume", "33333333-cccc-4ddd-8eee-000000000003")
    assert plan.directory == str(project.resolve())
