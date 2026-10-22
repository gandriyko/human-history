"""Herdr integration through its CLI (verified against herdr 0.9.5).

Used commands: ``pane list``, ``pane process-info``, ``agent focus``, ``tab create``,
``pane split``, ``pane run``, ``plugin pane open``, ``notification show``.
All calls pass argv lists; the only text typed into a shell is ``shlex.join`` of a validated argv.
"""

from __future__ import annotations

import contextlib
import json
import os
import shlex
import shutil
import subprocess
from pathlib import Path
from typing import Any

from human_history.integrations.resume import ResumePlan

PLUGIN_ID = "human-history"
ENTRYPOINT = "browser"


class HerdrError(Exception):
    pass


def herdr_bin() -> str | None:
    return os.environ.get("HERDR_BIN_PATH") or shutil.which("herdr")


def inside_herdr() -> bool:
    return os.environ.get("HERDR_ENV") == "1" and herdr_bin() is not None


def call(*args: str) -> dict[str, Any]:
    binary = herdr_bin()
    if not binary:
        raise HerdrError("herdr CLI not found")
    try:
        proc = subprocess.run([binary, *args], capture_output=True, text=True, timeout=15, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise HerdrError(f"herdr {args[0]} failed: {exc}") from exc
    if proc.returncode != 0:
        raise HerdrError(f"herdr {' '.join(args[:2])} failed: {proc.stderr.strip()[:300]}")
    try:
        return json.loads(proc.stdout) if proc.stdout.strip() else {}
    except json.JSONDecodeError:
        return {}


def plugin_context() -> dict[str, Any]:
    try:
        data = json.loads(os.environ.get("HERDR_PLUGIN_CONTEXT_JSON") or "{}")
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def running_pane(agent: str, session_id: str) -> str | None:
    """Pane already running this session, if Herdr can tell."""
    panes = call("pane", "list").get("result", {}).get("panes", [])
    for pane in panes:
        sess = pane.get("agent_session") or {}
        if sess.get("value") == session_id and (sess.get("agent") or pane.get("agent")) == agent:
            return pane.get("pane_id")
    # Without a Herdr agent integration the session id is only visible in the process argv.
    for pane in panes:
        if pane.get("agent") != agent or not pane.get("pane_id"):
            continue
        try:
            info = call("pane", "process-info", "--pane", pane["pane_id"])
        except HerdrError:
            continue
        procs = info.get("result", {}).get("process_info", {}).get("foreground_processes", [])
        if any(session_id in (p.get("argv") or []) for p in procs):
            return pane["pane_id"]
    return None


def resume(plan: ResumePlan, placement: str, workspace: str | None, target_pane: str | None) -> str:
    """Focus the pane already running the session, or start it in a new tab/split. Returns pane id."""
    existing = running_pane(plan.agent, plan.session_id)
    if existing:
        call("agent", "focus", existing)
        return existing
    if placement == "tab" or not target_pane:
        args = [
            "tab",
            "create",
            "--cwd",
            plan.directory,
            "--label",
            (plan.title or plan.agent)[:40],
            "--focus",
        ]
        if workspace:
            args += ["--workspace", workspace]
        pane_id = call(*args).get("result", {}).get("root_pane", {}).get("pane_id")
    else:
        direction = "down" if placement == "split-down" else "right"
        out = call("pane", "split", target_pane, "--direction", direction, "--cwd", plan.directory, "--focus")
        pane_id = out.get("result", {}).get("pane", {}).get("pane_id")
    if not pane_id:
        raise HerdrError("herdr did not return the new pane id")
    call("pane", "run", pane_id, shlex.join(plan.argv))
    return pane_id


def _pane_file() -> Path | None:
    state_dir = os.environ.get("HERDR_PLUGIN_STATE_DIR")
    return Path(state_dir) / "browser-pane" if state_dir else None


def close_history() -> bool:
    """Close the history pane opened by a previous action. False when none is open."""
    pane_file = _pane_file()
    if not pane_file or not pane_file.is_file():
        return False
    pane_id = pane_file.read_text().strip()
    pane_file.unlink()
    try:
        call("plugin", "pane", "close", pane_id)  # fails once the user has quit the browser
    except HerdrError:
        return False
    return True


def open_history(
    placement: str, cwd: str, env: dict[str, str], workspace: str | None, target_pane: str | None, size: str
) -> None:
    args = [
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        ENTRYPOINT,
        "--placement",
        placement,
    ]
    if placement in ("split", "zoomed") and target_pane:
        args += ["--target-pane", target_pane]
    if placement == "split":
        args += ["--direction", "right"]
    if placement == "popup":
        args += ["--width", size, "--height", size]
    if workspace and placement != "overlay":  # Herdr rejects a workspace for overlays: they open on the active pane
        args += ["--workspace", workspace]
    args += ["--cwd", cwd]
    for key, value in env.items():
        args += ["--env", f"{key}={value}"]
    pane_id = call(*args, "--focus").get("result", {}).get("plugin_pane", {}).get("pane", {}).get("pane_id")
    pane_file = _pane_file()
    if pane_id and pane_file:  # popups have no pane id, so they cannot be toggled
        pane_file.parent.mkdir(parents=True, exist_ok=True)
        pane_file.write_text(pane_id)


def notify(title: str, body: str) -> None:
    with contextlib.suppress(HerdrError):  # notifications are best effort
        call("notification", "show", title, "--body", body)
