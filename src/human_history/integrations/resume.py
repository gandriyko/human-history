"""Building and validating agent resume commands (no shell involved anywhere)."""

from __future__ import annotations

import os
import re
import shutil
from dataclasses import dataclass
from pathlib import Path

from human_history.config import ResumeDirectory
from human_history.storage.search import SessionRow

_SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")


class ResumeError(Exception):
    pass


@dataclass(frozen=True)
class ResumePlan:
    agent: str
    session_id: str
    directory: str
    argv: tuple[str, ...]
    title: str = ""


def resume_argv(agent: str, session_id: str) -> list[str]:
    if not _SAFE_ID.match(session_id):
        raise ResumeError(f"refusing to resume: unexpected session id {session_id!r}")
    if agent == "claude":
        return ["claude", "--resume", session_id]
    if agent == "codex":
        return ["codex", "resume", session_id]
    if agent == "opencode":
        return ["opencode", "--session", session_id]
    if agent == "copilot":
        return ["copilot", f"--resume={session_id}"]  # the value is optional: `--resume <id>` may not bind
    if agent == "pi":
        return ["pi", "--session", session_id]
    raise ResumeError(f"resume is not supported for agent {agent!r}")


def directory_for(policy: ResumeDirectory, session: SessionRow, current: str) -> str | None:
    """Target directory for a fixed policy; ``None`` means the user must be asked."""
    if policy == "original":
        return session.cwd or None
    if policy == "current":
        return current
    return None


def needs_confirmation(session: SessionRow, directory: str) -> bool:
    """Resuming anywhere but the session's own directory is never done silently."""
    return not session.cwd or os.path.realpath(directory) != os.path.realpath(session.cwd)


def build_plan(session: SessionRow, directory: str) -> ResumePlan:
    argv = resume_argv(session.agent, session.session_id)
    path = Path(directory).expanduser()
    if not path.is_dir():
        raise ResumeError(f"directory does not exist: {path}")
    if not shutil.which(argv[0]):
        raise ResumeError(f"{argv[0]!r} CLI is not installed (not found on PATH)")
    return ResumePlan(session.agent, session.session_id, str(path.resolve()), tuple(argv), session.title)


def exec_plan(plan: ResumePlan) -> None:
    """Replace this process with the agent (used outside Herdr, after the TUI exited)."""
    os.chdir(plan.directory)
    os.execvp(plan.argv[0], list(plan.argv))
