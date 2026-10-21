# Herdr plugin

[README](../README.md) · [Herdr plugin](herdr.md) · [Usage](usage.md) · [Configuration](configuration.md) · [Internals](internals.md)

Install [uv](https://docs.astral.sh/uv/getting-started/installation/) first. Herdr uses it to
prepare Python and the locked dependencies during plugin installation. Then install the plugin
directly from the public GitHub repository:

```sh
herdr plugin install gandriyko/human-history/herdr-plugin
herdr plugin action list --plugin human-history
```

The `/herdr-plugin` suffix selects the directory containing `herdr-plugin.toml`. The install
builds the CLI inside Herdr's managed checkout; a separate `uv tool install` is not required.
For local development, link this checkout instead:

```sh
herdr plugin link /path/to/human-history/herdr-plugin
```

`plugin link` skips the build, so run `uv sync --no-dev` in the checkout first.

Optionally bind a free key in `~/.config/herdr/config.toml` and run
`herdr server reload-config`:

```toml
[[keys.command]]
key = "f6"
type = "plugin_action"
command = "human-history.recent-here"
description = "Humanized AI Session History"
```

| Action | What it does |
|---|---|
| `human-history.open` | Browser using your default project filter |
| `human-history.recent-here` | Browser limited to the focused pane's repository, including its worktrees |
| `human-history.search` | Browser over all projects, for searching everything |
| `human-history.resume-last` | Resume the newest session of the focused worktree in its own directory, with no UI |
| `human-history.refresh-index` | Incremental re-index, with a Herdr notification |

The browser opens as an overlay by default. Set `[herdr] history_placement` to `popup`, `zoomed`,
`split` or `tab` to change that. When you resume, the session opens in a new tab or split
(`[resume] placement`) and the history pane exits, so no pane is left behind. If a pane is
already running that session, Herdr focuses it instead of starting a duplicate:
- Claude sessions are matched by Herdr's agent session id.
- Other agents' sessions are matched by process argv.

The plugin launcher (`herdr-plugin/hh.sh`) looks for the CLI in this order:
1. `$HUMAN_HISTORY_BIN`
2. The CLI built in the plugin checkout's `.venv`
3. `human-history` on PATH, including `~/.local/bin`
4. `uv run` in the checkout

To appear in the [Herdr plugin marketplace](https://herdr.dev/docs/marketplace/), make the
GitHub repository public and add the `herdr-plugin` repository topic. Herdr discovers a valid
manifest on the default branch automatically; no separate submission is needed.
