"""Git worktree/repository identity without spawning git.

* worktree: the top-level directory of the checkout containing ``cwd``.
* repo: the shared ``.git`` directory; identical for every worktree of one repository.
"""

from __future__ import annotations

import os
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple


class GitLocation(NamedTuple):
    worktree: str | None
    repo: str | None


@lru_cache(maxsize=4096)
def locate(cwd: str | None) -> GitLocation:
    if not cwd:
        return GitLocation(None, None)
    path = Path(cwd).expanduser()
    for directory in (path, *path.parents):
        dotgit = directory / ".git"
        try:
            if dotgit.is_dir():
                return GitLocation(str(directory), _real(dotgit))
            if dotgit.is_file():
                return GitLocation(str(directory), _common_dir(dotgit))
        except OSError:
            return GitLocation(None, None)
    return GitLocation(None, None)


def _real(path: Path) -> str:
    return os.path.realpath(path)


def _common_dir(dotgit_file: Path) -> str | None:
    """Resolve a linked worktree's ``.git`` file (``gitdir: …``) to the shared git dir."""
    try:
        content = dotgit_file.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None
    if not content.startswith("gitdir:"):
        return None
    gitdir = Path(content.removeprefix("gitdir:").strip())
    if not gitdir.is_absolute():
        gitdir = dotgit_file.parent / gitdir
    commondir = gitdir / "commondir"
    try:
        common = Path(commondir.read_text(encoding="utf-8").strip())
        return _real(common if common.is_absolute() else gitdir / common)
    except OSError:
        # Submodules have a gitdir but no commondir: the gitdir itself is the repository.
        return _real(gitdir)


def current_branch(worktree: str | None) -> str | None:
    """Branch checked out in ``worktree`` right now (reads HEAD directly)."""
    if not worktree:
        return None
    dotgit = Path(worktree) / ".git"
    try:
        if dotgit.is_file():
            gitdir = Path(dotgit.read_text(encoding="utf-8").strip().removeprefix("gitdir:").strip())
            if not gitdir.is_absolute():
                gitdir = dotgit.parent / gitdir
        else:
            gitdir = dotgit
        head = (gitdir / "HEAD").read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return head.removeprefix("ref: refs/heads/") if head.startswith("ref: refs/heads/") else None
