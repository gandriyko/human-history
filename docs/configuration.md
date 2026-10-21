# Configuration

[README](../README.md) · [Herdr plugin](herdr.md) · [Usage](usage.md) · [Configuration](configuration.md) · [Internals](internals.md)

The settings live in a TOML file: `~/.config/human-history/config.toml`, or the path in
`$HUMAN_HISTORY_CONFIG`.

```sh
hh config --init      # writes the fully documented example (refuses to overwrite; --force does)
$EDITOR ~/.config/human-history/config.toml
hh config             # validates it and prints the effective settings
```

- Every key is optional, so keep only what you change.
- Unknown keys and invalid values stop the app with the exact location, for example
  `colors.selection: not a colour: 'not-a-colour'`.
- Changes apply the next time the app starts.

The complete file, with every option set to its default, is
[`src/human_history/config.example.toml`](../src/human_history/config.example.toml). Its sections:

| Section | What it controls |
|---|---|
| `[ui]` | Theme (see [Themes](#themes)) and date format |
| `[colors]` | Selection, scrollbar, project/agent and search-match colours |
| `[search]` | Default scope, project filter, sort order and list size |
| `[remembered]` | Choices saved from the TUI: project filter, sort, theme and resume directory |
| `[resume]` | Directory policy (`ask` / `current` / `original`), Herdr placement, auto-close |
| `[herdr]` | How the plugin opens the browser: overlay, popup, zoomed, split or tab |
| `[index]` | Auto-refresh interval and database location |
| `[paths]` | Where each agent's logs are read from |
| `[keys]` | Any shortcut (`action = "key,other-key"`, Textual key names) |

## Examples

Change only the colours. Values can be `#hex`, `rgb(…)` or colour names:

```toml
[colors]
selection = "#263a2f"          # sage-green selected row
selection_accent = "#7fb88f"   # matching scrollbar thumb and focused-message bar
project = "teal"
```

Search only your own prompts across all projects by default, newest sessions first:

```toml
[search]
default_scope = "user"
default_project_filter = "all"
sort = "created_desc"
```

Always resume in the session's own directory, in a split next to the current pane:

```toml
[resume]
directory = "original"
placement = "split-right"
```

Open the browser as a centered popup in Herdr instead of an overlay:

```toml
[herdr]
history_placement = "popup"
popup_size = "80%"
```

Read logs from extra locations, such as a second Claude config dir or a synced backup:

```toml
[paths]
claude = ["~/.claude/projects", "~/.claude-work/projects"]
codex = ["~/.codex/sessions", "/mnt/backup/codex/sessions"]
```

Rebind shortcuts. A key can have several names, separated by commas:

```toml
[keys]
scope_both = "ctrl+b"          # for terminals that cannot send Ctrl+Shift+A
resume = "r,ctrl+o"
quit = "q,ctrl+q"
```

Use a fixed theme and fewer background refreshes:

```toml
[ui]
theme = "gruvbox-light"

[index]
refresh_interval_seconds = 120
```

## Themes

The themes have the same names as Herdr's: `terminal`, `catppuccin`, `catppuccin-latte`,
`tokyo-night`, `tokyo-night-day`, `dracula`, `nord`, `gruvbox`, `gruvbox-light`, `one-dark`,
`one-light`, `solarized`, `solarized-light`.
- The default, `auto`, follows Herdr's `[theme] name` when running inside Herdr. Elsewhere it uses
  `terminal`.
- `terminal` keeps the terminal's own colours and background. Every other theme paints its own.
- `t` opens a picker that previews the highlighted theme. Enter keeps it, and the choice is
  remembered over `[ui] theme`. Esc restores the previous theme.
- Under a named theme, the selection and scrollbar colours come from the theme, unless `[colors]`
  sets `selection` or `selection_accent` to something other than the default.

`show_tool_calls`, `show_tool_results`, `show_reasoning` and `show_system_messages` are accepted
under `[ui]` only as `false`, because technical events are never displayed.

The TUI saves the chosen project filter, sort, theme and remembered resume directory in
`~/.config/human-history/config.toml` (or `$HUMAN_HISTORY_CONFIG`) under `[remembered]`. It creates
the file on the first saved choice and moves any previously saved choices from the index database
on the next launch. These choices override the defaults in `[search]`, `[ui]` and `[resume]`.
To be asked where to resume again, delete `resume_directory` from `[remembered]`.
`hh index --rebuild` does not reset these choices, although it still clears favorites.
