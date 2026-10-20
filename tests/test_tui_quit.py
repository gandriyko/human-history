import asyncio

from human_history.storage.indexer import index_all
from human_history.tui.app import HistoryApp
from human_history.tui.screens import HelpScreen


def test_ctrl_c_twice_quits(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)
    index_all(db, config)

    async def scenario():
        app = HistoryApp(config, str(tmp_path), project_filter="all")
        async with app.run_test(size=(100, 30)) as pilot:
            await pilot.pause(0.2)
            await pilot.press("ctrl+c")  # focus is in the search input, which would normally take Ctrl+C
            await pilot.pause(0.1)
            assert app.is_running
            app._last_ctrl_c -= 5  # too slow: a lone press after the window does not quit
            await pilot.press("ctrl+c")
            await pilot.pause(0.1)
            assert app.is_running
            app.push_screen(HelpScreen(config.keymap()))  # also works with a dialog open
            await pilot.pause(0.1)
            await pilot.press("ctrl+c")
            await pilot.pause(0.1)
            assert not app.is_running

    asyncio.run(scenario())


def test_escape_twice_on_home_quits(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)
    index_all(db, config)

    async def scenario():
        app = HistoryApp(config, str(tmp_path), project_filter="all")
        async with app.run_test(size=(100, 30)) as pilot:
            await pilot.pause(0.2)
            # Esc steps back first: out of the conversation, then clears the search (neither arms quitting).
            await pilot.press("enter")
            await pilot.pause(0.3)
            await pilot.press("escape", *"zz", "escape")  # conversation -> home, then clears "zz"
            assert app.is_running and app.query_one("#home").display
            assert app.query_one("#search").value == ""
            await pilot.press("escape")  # empty search: first press only warns
            assert app.is_running
            await pilot.press("escape")  # second within 1.5 s quits
            await pilot.pause(0.1)
            assert not app.is_running

    asyncio.run(scenario())
