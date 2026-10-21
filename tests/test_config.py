import re
from pathlib import Path

import pytest
from typer.testing import CliRunner

from human_history.cli import app as cli_app
from human_history.config import EXAMPLE_PATH, KEY_ACTIONS, Config, ConfigError, load_config


def test_shipped_example_lists_every_option_with_its_default():
    example = load_config(EXAMPLE_PATH)
    defaults = Config()
    assert example.model_dump(exclude={"paths", "keys"}) == defaults.model_dump(exclude={"paths", "keys"})
    assert example.keys == KEY_ACTIONS  # every action documented
    for section in Config.model_fields:
        assert f"[{section}]" in EXAMPLE_PATH.read_text()


def test_config_init_writes_the_example(tmp_path, monkeypatch):
    target = tmp_path / "cfg" / "config.toml"
    monkeypatch.setenv("HUMAN_HISTORY_CONFIG", str(target))
    runner = CliRunner()
    assert runner.invoke(cli_app, ["config", "--init"]).exit_code == 0
    assert target.read_text() == EXAMPLE_PATH.read_text()
    assert runner.invoke(cli_app, ["config", "--init"]).exit_code == 1  # never overwrites silently
    assert runner.invoke(cli_app, ["config", "--init", "--force"]).exit_code == 0


def test_documented_toml_examples_are_valid(tmp_path):
    documentation = (Path(__file__).parents[1] / "docs" / "configuration.md").read_text()
    blocks = re.findall(r"```toml\n(.*?)```", documentation, re.S)
    assert len(blocks) >= 7
    for block in blocks:
        load_config(write(tmp_path, block))  # raises ConfigError on any invalid example


def test_custom_colors(tmp_path):
    cfg = load_config(write(tmp_path, '[colors]\nselection = "rgb(38, 58, 47)"\nproject = "teal"'))
    assert cfg.colors.selection == "rgb(38, 58, 47)" and cfg.colors.codex == "#10a37f"


EXAMPLE = """
[ui]
theme = "nord"
show_tool_calls = false
show_tool_results = false
show_reasoning = false
show_system_messages = false

[search]
default_scope = "user"
default_project_filter = "current-worktree"
sort = "last_activity_desc"

[resume]
directory = "ask"
placement = "tab"
close_history_on_resume = true

[index]
auto_refresh = true
refresh_interval_seconds = 30

[keys]
scope_both = "ctrl+b"
"""


def write(tmp_path: Path, text: str) -> Path:
    path = tmp_path / "config.toml"
    path.write_text(text)
    return path


def test_documented_example_is_valid(tmp_path):
    cfg = load_config(write(tmp_path, EXAMPLE))
    assert cfg.search.default_scope == "user" and cfg.resume.directory == "ask"
    assert cfg.keymap()["scope_both"] == "ctrl+b" and cfg.keymap()["resume"] == "r"


def test_missing_file_gives_defaults(tmp_path):
    assert load_config(tmp_path / "nope.toml").search.default_project_filter == "repository"


@pytest.mark.parametrize(
    ("text", "fragment"),
    [
        ("[ui]\nthemee = 'dark'", "ui.themee"),
        ("[resume]\ndirectory = 'home'", "resume.directory"),
        ("[ui]\nshow_tool_calls = true", "ui.show_tool_calls"),
        ("[keys]\nexplode = 'x'", "unknown key action"),
        ("[index]\nrefresh_interval_seconds = 1", "refresh_interval_seconds"),
        ("[colors]\nselection = 'not-a-colour'", "colors.selection"),
        ("[bogus]\nx = 1", "bogus"),
        ("not = = toml", "invalid TOML"),
    ],
)
def test_invalid_config_is_reported(tmp_path, text, fragment):
    with pytest.raises(ConfigError, match=fragment):
        load_config(write(tmp_path, text))
