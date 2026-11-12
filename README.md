# human-history

**Search and resume your coding-agent sessions: offline, from the keyboard.**

Works with Claude Code, Codex CLI, OpenCode, GitHub Copilot CLI, pi and Hermes Agent. It shows only
the human conversation (your prompts and the agents' visible answers), never tool calls, reasoning
or raw JSON.

![Session list: age, agent, project and title of every session](docs/screenshots/home.png)

- **One Rust binary.** TUI plus a SQLite full-text index. No network, no telemetry, no AI calls.
- **Search what matters.** Your prompts, agent answers, both, or session titles (fuzzy).
- **Project-aware.** Starts in the current git repository; `p` switches between worktree, repository and all projects.
- **Resume anywhere.** In the session's original directory or the current worktree.
- **Agent-ready.** A skill and an MCP server let agents search your past sessions.

## Install

You need Rust 1.89+ ([rustup](https://rustup.rs)).

```sh
cargo install --locked --git https://github.com/gandriyko/human-history
human-history          # / search · Enter read · r resume
```

With [Herdr](https://herdr.dev): `herdr plugin install gandriyko/human-history/herdr-plugin`
([setup](docs/herdr.md)).

## For agents

**MCP server.** Exposes three tools: `search_sessions`, `read_session` and `set_favorite`.

```sh
claude mcp add human-history -- human-history mcp
```

```toml
# Codex: ~/.codex/config.toml
[mcp_servers.human-history]
command = "human-history"
args = ["mcp"]
```

**Skill.** For agents with a shell, [`SKILL.md`](skills/human-history/SKILL.md) teaches them
`human-history search --json` and `human-history show`:

```sh
ln -s "$PWD/skills/human-history" ~/.claude/skills/human-history
```

## Docs

[Usage and keys](docs/usage.md) · [Configuration and themes](docs/configuration.md) ·
[Internals and development](docs/internals.md)

The earlier Python version lives on the `python` branch and uses the same config and index.
