import asyncio

from textual import events
from textual.widgets import Select

from human_history.storage.indexer import index_all
from human_history.tui.app import HistoryApp
from human_history.tui.layout import latin_key
from human_history.tui.screens import HelpScreen


def key(name: str, char: str | None, typing: bool = False) -> tuple[str, str | None]:
    out = latin_key(events.Key(name, char), typing)
    return out.key, out.character


def test_cyrillic_keys_map_to_latin_positions():
    assert key("й", "й") == ("q", "q")
    assert key("к", "к") == ("r", "r")
    assert key("і", "і") == ("s", "s")  # Ukrainian
    assert key("ы", "ы") == ("s", "s")  # Russian
    assert key("Т", "Т") == ("N", "N")  # Shift+n
    assert key("full_stop", ".") == ("slash", "/")  # the "/" key on ЙЦУКЕН
    assert key("comma", ",") == ("question_mark", "?")
    assert key("ctrl+г", None) == ("ctrl+u", None)
    assert key("q", "q") == ("q", "q")


def test_typing_keeps_cyrillic_but_maps_ctrl_combos():
    assert key("й", "й", typing=True) == ("й", "й")
    assert key("ctrl+ф", None, typing=True) == ("ctrl+a", None)


def test_shortcuts_work_on_cyrillic_layout(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)
    index_all(db, config)

    async def scenario():
        app = HistoryApp(config, str(tmp_path), project_filter="all")
        async with app.run_test(size=(100, 30)) as pilot:
            await pilot.pause(0.2)
            search = app.query_one("#search")
            await pilot.press("full_stop")  # "/" on the Cyrillic layout: into the search box
            await pilot.press("п", "р")  # typed into the search box as Cyrillic text
            assert search.value == "пр"
            await pilot.press("ctrl+г")  # Ctrl+U on the Cyrillic layout: scope = my prompts
            assert app.scope == "user"
            await pilot.press("escape", "escape")  # clears the search, then to the list
            await pilot.press("ф")  # "a": cycle agent
            assert app.query_one("#agent", Select).value == "claude"
            await pilot.press("comma")  # "?": help
            await pilot.pause(0.1)
            assert isinstance(app.screen, HelpScreen)
            await pilot.press("й")  # "q" closes the help…
            await pilot.pause(0.1)
            await pilot.press("й")  # …and quits the app
            await pilot.pause(0.1)
            assert not app.is_running

    asyncio.run(scenario())
