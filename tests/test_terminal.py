"""The shell must never get the terminal back with mouse reporting on (e.g. after VS Code "Stop")."""

import os
import pty
import re
import select
import signal
import sys
import time
from pathlib import Path

import pytest

MOUSE_MODES = {"1000", "1002", "1003", "1006", "1015"}


def run_in_pty(tmp_path: Path, finish) -> bytes:
    env = {
        **os.environ,
        "TERM": "xterm-256color",
        "XDG_DATA_HOME": str(tmp_path / "data"),
        "CLAUDE_CONFIG_DIR": str(tmp_path / "claude"),
        "CODEX_HOME": str(tmp_path / "codex"),
        "COPILOT_HOME": str(tmp_path / "copilot"),
        "PI_CODING_AGENT_DIR": str(tmp_path / "pi"),
        "HUMAN_HISTORY_CONFIG": str(tmp_path / "none.toml"),
    }
    env.pop("HERDR_ENV", None)
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(sys.executable, [sys.executable, "-m", "human_history.cli", "tui"], env)
    out = b""

    def pump(seconds: float) -> None:
        nonlocal out
        end = time.time() + seconds
        while time.time() < end:
            if select.select([fd], [], [], 0.05)[0]:
                try:
                    out += os.read(fd, 65536)
                except OSError:
                    return

    pump(4)
    finish(pid, fd, pump)
    deadline = time.time() + 10
    while os.waitpid(pid, os.WNOHANG) == (0, 0):
        if time.time() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            pytest.fail("TUI did not exit")
        pump(0.2)
    pump(0.5)
    return out


def press(*keys: bytes):
    def finish(pid, fd, pump):
        for key in keys:  # one write per key: "\x1bq" in one read would be alt+q
            os.write(fd, key)
            pump(0.5)

    return finish


def send(sig: int):
    return lambda pid, fd, pump: os.kill(pid, sig)


def final_modes(out: bytes) -> dict[str, str]:
    state: dict[str, str] = {}
    for nums, flag in re.findall(rb"\x1b\[\?([\d;]+)([hl])", out):
        for n in nums.decode().split(";"):
            state[n] = flag.decode()
    return state


@pytest.mark.parametrize(
    "finish",
    [
        pytest.param(press(b"\x1b", b"q"), id="esc-q"),
        pytest.param(send(signal.SIGTERM), id="sigterm"),
        pytest.param(send(signal.SIGHUP), id="sighup"),
    ],
)
def test_terminal_modes_restored(tmp_path, finish):
    modes = final_modes(run_in_pty(tmp_path, finish))
    assert MOUSE_MODES & {m for m, v in modes.items() if v == "h"} == set()
    assert modes.get("1003") == "l"  # it was switched on, and is off again
