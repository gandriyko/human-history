"""Command line entry point: ``human-history`` (alias ``hh``)."""

from __future__ import annotations

import os
import shutil
import sys
from typing import Annotated, get_args

import typer
from rich.console import Console
from rich.markdown import Markdown
from rich.table import Table
from rich.text import Text

from human_history.config import (
    EXAMPLE_PATH,
    Config,
    ConfigError,
    ProjectFilter,
    Scope,
    Sort,
    config_path,
    load_config,
)
from human_history.integrations import herdr
from human_history.integrations.resume import ResumeError, build_plan, exec_plan
from human_history.storage import search as store
from human_history.storage.database import connect
from human_history.storage.indexer import index_all
from human_history.tui.widgets import AGENT_LABEL, local_time, project_name, snippet_text

app = typer.Typer(add_completion=False, help="Humanized, offline history of Claude Code and Codex sessions.")
console = Console()
err = Console(stderr=True)

FilterOpt = Annotated[str | None, typer.Option("--filter", "-f", help="current-worktree | repository | all")]
ScopeOpt = Annotated[str | None, typer.Option("--scope", "-s", help="user | assistant | both | title")]


def _choice(value: str | None, literal, name: str) -> str | None:
    if value is not None and value not in get_args(literal):
        err.print(f"[red]invalid {name} {value!r}; choose from {', '.join(get_args(literal))}[/]")
        raise typer.Exit(2)
    return value


def _config() -> Config:
    try:
        return load_config()
    except ConfigError as exc:
        err.print(f"[red]{exc}[/]")
        raise typer.Exit(2) from exc


def _cwd() -> str:
    return os.environ.get("HH_CWD") or os.getcwd()


@app.callback(invoke_without_command=True)
def main(
    ctx: typer.Context,
    project_filter: FilterOpt = None,
    scope: ScopeOpt = None,
    query: Annotated[str, typer.Option("--query", "-q", help="Initial search text")] = "",
) -> None:
    """Open the TUI (default command)."""
    if ctx.invoked_subcommand is None:
        tui(project_filter, scope, query)


@app.command()
def tui(
    project_filter: FilterOpt = None,
    scope: ScopeOpt = None,
    query: Annotated[str, typer.Option("--query", "-q")] = "",
) -> None:
    """Browse and search sessions; resume one."""
    from human_history.tui.app import run_tui

    config = _config()
    plan = run_tui(
        config,
        project_filter=_choice(project_filter or os.environ.get("HH_FILTER"), ProjectFilter, "filter"),
        scope=_choice(scope, Scope, "scope"),
        query=query,
    )
    if plan:
        console.print(f"[dim]Resuming {plan.agent} session in {plan.directory}…[/]")
        exec_plan(plan)


@app.command()
def index(
    rebuild: Annotated[bool, typer.Option(help="Drop the index and parse everything again")] = False,
) -> None:
    """Incrementally index session logs."""
    config = _config()
    path = config.index.database_path()
    if rebuild:
        for suffix in ("", "-wal", "-shm"):
            path.with_name(path.name + suffix).unlink(missing_ok=True)
    conn = connect(path)
    stats = index_all(conn, config)
    console.print(
        f"scanned {stats.scanned} files, parsed {stats.parsed}, +{stats.messages} messages, "
        f"{stats.skipped_lines} malformed lines skipped, {stats.removed_sessions} sessions removed → {path}"
    )


@app.command("search")
def search_cmd(
    query: str,
    scope: ScopeOpt = "both",
    project_filter: FilterOpt = "all",
    agent: Annotated[str | None, typer.Option(help="claude | codex")] = None,
    branch: str | None = None,
    since: Annotated[str | None, typer.Option(help="ISO date, e.g. 2026-10-01")] = None,
    until: str | None = None,
    cwd: Annotated[str | None, typer.Option(help="Only sessions started in exactly this directory")] = None,
    sort: Annotated[str, typer.Option()] = "last_activity_desc",
    limit: int = 20,
) -> None:
    """Search prompts/answers/titles from the command line."""
    config = _config()
    conn = connect(config.index.database_path())
    filters = store.Filters(
        project=_choice(project_filter, ProjectFilter, "filter"),  # type: ignore[arg-type]
        cwd=_cwd(),
        agent=agent,
        branch=branch,
        since=since,
        until=until,
        exact_cwd=os.path.abspath(cwd) if cwd else None,
    )
    rows = store.search(
        conn,
        query,
        _choice(scope, Scope, "scope"),
        filters,
        _choice(sort, Sort, "sort"),
        limit,  # type: ignore[arg-type]
    )
    table = Table(box=None, show_header=True, header_style="bold dim")
    for col in ("when", "agent", "project", "title / match"):
        table.add_column(col, overflow="fold" if col == "title / match" else "ellipsis")
    for r in rows:
        cell = Text(r.title, style="bold")
        if r.snippet:
            cell.append("\n")
            cell.append_text(snippet_text(r.snippet))
        table.add_row(local_time(r.updated_at, config.ui.date_format), AGENT_LABEL[r.agent], project_name(r), cell)
    console.print(table if rows else "[dim]no matches[/]")


@app.command()
def show(session: Annotated[str, typer.Argument(help="Session id (or unique prefix)")]) -> None:
    """Print a session as a clean Markdown conversation."""
    config = _config()
    conn = connect(config.index.database_path())
    keys = [r[0] for r in conn.execute("SELECT key FROM sessions WHERE session_id LIKE ? || '%'", (session,))]
    if len(keys) != 1:
        err.print(f"[red]{'no' if not keys else 'ambiguous'} session matching {session!r}[/]")
        raise typer.Exit(1)
    row = store.get_session(conn, keys[0])
    assert row is not None
    console.print(Text(row.title, style="bold underline"))
    previous = None
    for m in store.messages(conn, row.key):
        if m["role"] != previous:
            if m["role"] == "user" and previous:
                console.rule(style="dim")
            name = "USER" if m["role"] == "user" else AGENT_LABEL[row.agent].upper()
            console.print(Text(f"\n{name}", style="bold cyan" if m["role"] == "user" else "bold magenta"))
        console.print(Markdown(m["text"], hyperlinks=False))
        previous = m["role"]


def _resume_last(config: Config, cwd: str, workspace: str | None = None, target: str | None = None) -> None:
    conn = connect(config.index.database_path())
    index_all(conn, config)
    rows = store.list_sessions(conn, store.Filters(project="current-worktree", cwd=cwd), limit=1)
    if not rows:
        raise ResumeError(f"no sessions recorded for the worktree of {cwd}")
    # Non-interactive: always the session's own directory, so nothing lands in an unexpected place.
    plan = build_plan(rows[0], rows[0].cwd or "")
    if herdr.inside_herdr():
        herdr.resume(plan, config.resume.placement, workspace, target)
    else:
        exec_plan(plan)


@app.command("resume-last")
def resume_last() -> None:
    """Resume the most recent session of the current worktree in its original directory."""
    try:
        _resume_last(_config(), _cwd(), os.environ.get("HERDR_WORKSPACE_ID"), os.environ.get("HERDR_PANE_ID"))
    except (ResumeError, herdr.HerdrError) as exc:
        err.print(f"[red]{exc}[/]")
        raise typer.Exit(1) from exc


HERDR_ACTIONS = ("open", "recent-here", "search", "resume-last", "refresh-index")


@app.command("herdr-action", hidden=True)
def herdr_action(action: str) -> None:
    """Entry point for Herdr plugin actions (see herdr-plugin/herdr-plugin.toml)."""
    if action not in HERDR_ACTIONS:
        err.print(f"unknown action {action!r}")
        raise typer.Exit(2)
    config = _config()
    ctx = herdr.plugin_context()
    cwd = ctx.get("focused_pane_cwd") or ctx.get("workspace_cwd") or os.getcwd()
    workspace = ctx.get("workspace_id") or os.environ.get("HERDR_WORKSPACE_ID")
    target = ctx.get("focused_pane_id") or os.environ.get("HERDR_PANE_ID")
    try:
        if action == "refresh-index":
            stats = index_all(connect(config.index.database_path()), config)
            herdr.notify("human-history", f"Indexed {stats.parsed} changed files, +{stats.messages} messages")
            return
        if action == "resume-last":
            _resume_last(config, cwd, workspace, target)
            return
        env = {"HH_CWD": cwd}
        if workspace:
            env["HH_WORKSPACE"] = workspace
        if target:
            env["HH_TARGET_PANE"] = target
        if action == "recent-here":
            env["HH_FILTER"] = "current-worktree"
        if action == "search":
            env["HH_FILTER"] = "all"  # the search box is always focused; search everything
        herdr.open_history(config.herdr.history_placement, cwd, env, workspace, target, config.herdr.popup_size)
    except (ResumeError, herdr.HerdrError) as exc:
        herdr.notify("human-history", str(exc))
        err.print(str(exc))
        raise typer.Exit(1) from exc


@app.command("config")
def config_cmd(
    init: Annotated[bool, typer.Option("--init", help="Create the config file from the documented example")] = False,
    force: Annotated[bool, typer.Option(help="With --init: overwrite an existing file")] = False,
) -> None:
    """Validate and print the effective configuration (or create it with --init)."""
    path = config_path()
    if init:
        if path.exists() and not force:
            err.print(f"[red]{path} already exists (use --force to overwrite)[/]")
            raise typer.Exit(1)
        path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(EXAMPLE_PATH, path)
        console.print(f"created {path} — edit it, then check with `hh config`")
        return
    config = _config()
    console.print(f"[dim]config file: {path} ({'found' if path.exists() else 'not found, defaults'})[/]")
    console.print(f"[dim]database:    {config.index.database_path()}[/]")
    console.print_json(config.model_dump_json())


if __name__ == "__main__":
    sys.exit(app())
