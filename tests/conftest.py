import shutil
from pathlib import Path

import pytest

from human_history.config import Config
from human_history.storage.database import connect

FIXTURES = Path(__file__).parent / "fixtures"
CLAUDE_FILE = "-work-app/11111111-aaaa-4bbb-8ccc-000000000001.jsonl"
CODEX_FILE = "2026/10/02/rollout-2026-10-02T08-00-00-22222222-bbbb-4ccc-8ddd-000000000002.jsonl"
# Text placed only inside tool calls/results, reasoning, system/meta events. Must never be indexed or shown.
FORBIDDEN = ("TOOLSECRET", "REASONINGSECRET", "SYSTEMSECRET", "HOOKSECRET", "SIDECHAINSECRET")


@pytest.fixture
def logs(tmp_path: Path) -> Path:
    """Writable copy of the fixture logs (tests append/rewrite them)."""
    root = tmp_path / "logs"
    shutil.copytree(FIXTURES, root)
    return root


@pytest.fixture
def config(logs: Path, tmp_path: Path) -> Config:
    cfg = Config()
    cfg.paths.claude = [str(logs / "claude")]
    cfg.paths.codex = [str(logs / "codex" / "sessions")]
    cfg.index.database = str(tmp_path / "index.sqlite3")
    cfg.index.auto_refresh = False
    return cfg


@pytest.fixture
def db(config: Config):
    conn = connect(config.index.database_path())
    yield conn
    conn.close()
