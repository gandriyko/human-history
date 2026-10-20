import asyncio

from textual.widgets import OptionList

from human_history.storage.database import get_kv
from human_history.tui.app import HistoryApp
from human_history.tui.screens import ThemeScreen
from human_history.tui.themes import THEMES, resolve


def test_auto_follows_herdr_theme_only_inside_herdr(tmp_path, monkeypatch):
    (tmp_path / "herdr").mkdir()
    (tmp_path / "herdr" / "config.toml").write_text('[theme]\nname = "gruvbox"\n')
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    monkeypatch.delenv("HERDR_ENV", raising=False)
    assert resolve("auto") == "terminal"
    monkeypatch.setenv("HERDR_ENV", "1")
    assert resolve("auto") == "gruvbox"
    assert resolve("no-such-theme") == "terminal" and resolve("nord") == "nord"
    assert len(THEMES) == 13


def test_theme_picker_previews_and_remembers(config, db, tmp_path, monkeypatch):
    monkeypatch.delenv("HERDR_ENV", raising=False)

    async def scenario():
        app = HistoryApp(config, str(tmp_path), project_filter="all")
        async with app.run_test(size=(100, 30)) as pilot:
            await pilot.pause(0.2)
            assert app.theme == "terminal" and app.ansi_color
            await pilot.press("escape", "t")  # from the search box to the list, where t works
            await pilot.pause(0.1)
            assert isinstance(app.screen, ThemeScreen)
            await pilot.press("down", "down")  # auto -> terminal -> catppuccin: previewed live
            await pilot.pause(0.1)
            assert app.theme == "catppuccin" and not app.ansi_color
            selection = app.get_css_variables()["selection"]
            await pilot.press("escape")  # cancel restores
            await pilot.pause(0.1)
            assert app.theme == "terminal" and get_kv(db, "theme") is None
            assert app.get_css_variables()["selection"] != selection  # tinted from the theme, not [colors]

            await pilot.press("t")
            await pilot.pause(0.1)
            app.screen.query_one(OptionList).highlighted = list(THEMES).index("nord") + 1
            await pilot.press("enter")
            await pilot.pause(0.1)
            assert app.theme == "nord" and get_kv(db, "theme") == "nord"

    asyncio.run(scenario())
