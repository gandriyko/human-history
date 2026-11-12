<div align="center">

# human-history

**Search and resume your coding-agent sessions: offline, from the keyboard.**

[![Rust 1.89+](https://img.shields.io/badge/Rust-1.89%2B-CE422B?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![macOS | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-555?logo=apple&logoColor=white)](#install)
[![SQLite FTS5](https://img.shields.io/badge/SQLite-FTS5-003B57?logo=sqlite&logoColor=white)](docs/internals.md)
[![ratatui](https://img.shields.io/badge/TUI-ratatui-7C3AED)](https://ratatui.rs)
[![MCP server](https://img.shields.io/badge/MCP-server-000?logo=modelcontextprotocol&logoColor=white)](#for-agents)
![Offline](https://img.shields.io/badge/network-none-2EA043)
[![MIT license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?logo=claude&logoColor=white)](https://claude.com/claude-code)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991)](https://github.com/openai/codex)
[![OpenCode](https://img.shields.io/badge/OpenCode-222)](https://opencode.ai)
[![Copilot CLI](https://img.shields.io/badge/Copilot_CLI-24292F?logo=githubcopilot&logoColor=white)](https://github.com/github/copilot-cli)
![pi](https://img.shields.io/badge/pi-444)
![Hermes Agent](https://img.shields.io/badge/Hermes_Agent-444)

</div>

It shows only the human conversation (your prompts and the agents' visible answers), never tool
calls, reasoning or raw JSON.

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

## License

[MIT](LICENSE)
