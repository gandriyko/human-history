# AGENTS.md

Instructions for coding agents working on this repository. User-facing docs live in `README.md`
and `docs/`; this file is about changing the code.

## What this is

`human-history` is a single Rust binary: a ratatui TUI, a CLI and an MCP server over a SQLite FTS5
index of coding-agent session logs (Claude Code, Codex, OpenCode, Copilot CLI, pi, Hermes).

## Commands

```sh
cargo build                    # target/debug/human-history
cargo test                     # unit tests, TUI scenarios, end-to-end tests in tests/cli.rs
cargo clippy --all-targets     # must stay warning-free
```

Run all three before calling a change done.

## Layout

| Path | What |
|---|---|
| `src/cli.rs` | clap commands, terminal output, exit codes |
| `src/mcp.rs` | MCP server: hand-written JSON-RPC over stdio, tool schemas and handlers |
| `src/search.rs` | search, filters, session/message queries, favorites |
| `src/indexer.rs`, `src/db.rs` | incremental indexing, SQLite schema |
| `src/parsers/` | one parser per agent; `mod.rs` wires discovery, titles and parsing |
| `src/tui/` | the TUI; `app_tests.rs` drives it with key events in an in-memory terminal |
| `src/config.rs` | config file, defaults, enums shared by CLI, TUI and MCP |
| `tests/cli.rs` | end-to-end tests of the binary in an isolated `Env` (own config, index, fake executables) |
| `skills/human-history/SKILL.md` | agent skill that documents the CLI's `--json` output |
| `herdr-plugin/` | Herdr plugin manifest and launcher |

To add an agent, follow the checklist at the end of `docs/internals.md`.

## Rules that must not break

- **Only the human conversation.** Index and show user prompts and visible agent answers. Never
  tool calls, tool output, reasoning, system or hook events.
- **Offline.** No network access, telemetry or AI calls at runtime.
- **stdout belongs to the caller.** Only `cli.rs` and `mcp.rs` write to stdout (and the TUI, through
  its terminal). Search, indexing, parsers and config never print: the MCP server speaks JSON-RPC
  on stdout, and one stray line breaks it. Diagnostics go to stderr.
- **Stable machine output.** `search --json`, `show --json` and the MCP tool results are a
  contract with agents. Change fields only deliberately, and update `SKILL.md` and the tests.
- **Unix only.** The code uses `std::os::unix`; macOS and Linux are supported.
- **No dependency without a reason.** The MCP server is hand-written to avoid `rmcp` + `tokio`.
  If a dependency raises the minimum Rust version, update `rust-version` in `Cargo.toml` (now 1.89,
  set by `rusqlite`).

## MCP server

- Each tool's JSON Schema in `tools()` sits next to its `#[serde(deny_unknown_fields)]` args
  struct. Change both together, or agents will not see the new parameter.
- Argument errors are tool results with `isError: true`, so the model can correct its call. Only
  malformed requests and unknown tools/methods are JSON-RPC errors.
- The server's working directory is not the user's project: project filters come only from the
  `project_dir` argument.
- Cover new behaviour in `mcp_server_speaks_json_rpc_over_stdio` (`tests/cli.rs`).

## Style

- Compact code in the style of the surrounding file; `///` docs on public items, few other
  comments.
- Tests never touch the real config, index or Herdr: use `Env` in `tests/cli.rs` and
  `src/testutil.rs`.
- When behaviour changes, update the docs in the same commit: `README.md`, `docs/usage.md`,
  `docs/configuration.md`, `SKILL.md`.

## Git

- Commit directly on `main`. Commit only when asked, and never push without being asked.
- Subjects are short and imperative, without prefixes: `Add human-history mcp, an MCP server for AI
  agents`, `Require Rust 1.89, the minimum of rusqlite`.
