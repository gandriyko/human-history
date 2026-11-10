# Internals

[README](../README.md) · [Herdr plugin](herdr.md) · [Usage](usage.md) · [Configuration](configuration.md) · [Internals](internals.md)

## How the parsing works

**Claude Code** (`~/.claude/projects/<encoded-cwd>/<session>.jsonl`):
- Tool results arrive as `type=user` lines. A user line counts as a prompt only if it has none of:
  `tool_result` blocks, `isMeta`, `isCompactSummary`, `isSidechain`, or a non-human `origin`. It
  must also not be a wrapper: `<local-command-stdout>`, `<system-reminder>`,
  `<task-notification>`, interruptions, or compact summaries.
- Slash commands become `/name args`.
- Prompts typed while the agent is busy appear only as `attachment.queued_command`, so they are
  included. When one is also logged as a `user` line, the copy is dropped.
- Every assistant block has its own line. Only `text` blocks are kept, so text written next to a
  tool call survives. `thinking`, `tool_use` and `<synthetic>` messages are dropped.
- Titles come from `custom-title`, then `ai-title`, then the first prompt.
- Subagent transcripts (`<session>/subagents/`) are not indexed.

**Codex** (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`):
- Prompts and answers are written twice, as `event_msg` and as `response_item`. Prompts come from
  the first copy and the immediate duplicate is skipped. Answers come from `response_item` only.
- Dropped: AGENTS.md / `<environment_context>` / `<user_instructions>` injections,
  developer/system messages, reasoning, `function_call(_output)`, `custom_tool_call` and token counts.
- The IDE wrapper (`## My request for Codex:`) is stripped.
- Older rollouts without the `{"type","payload"}` wrapper are supported.
- Titles come from `session_index.jsonl` (`thread_name`).

**OpenCode** 1.2+ (`~/.local/share/opencode/opencode.db`, SQLite, opened read-only):
- From the `session`, `message` and `part` tables only `text` parts are kept. User parts flagged
  `synthetic` or `ignored` (attached file contents, reminders, auto-continue) are dropped, and so
  are compaction summaries. `reasoning`, `tool`, `file`, `patch`, `step-*` and `subtask` parts are
  ignored.
- Child (subagent) sessions are skipped.
- There is no byte offset: each run re-reads the sessions changed since the last one. An answer
  still streaming is picked up once it completes.
- The older JSON-file storage (OpenCode 1.1 and earlier) is not read.

**GitHub Copilot CLI** (`~/.copilot/session-state/<id>/events.jsonl`):
- `user.message.content` is the prompt (`transformedContent` carries injected reminders).
  Instruction-discovery messages (`source` set) are dropped.
- Only the `content` of `assistant.message` is kept. `toolRequests`, `reasoningText`, tool, hook,
  skill and system events are ignored.
- Titles come from `workspace.yaml` (`name`, or `summary` in older versions).
- VS Code Copilot Chat is not indexed: it cannot be resumed from a terminal.

**pi** (`~/.pi/agent/sessions/--<cwd>--/<timestamp>_<id>.jsonl`):
- Only `user` text and the `text` blocks of `assistant` messages are kept. `thinking`,
  `toolCall`, `toolResult`, `bashExecution`, custom and summary entries are dropped.
- Expanded `/skill:` and `@file` blocks are stripped from the prompt.
- All branches are shown in file order. The title is the latest `session_info` name.

**Hermes Agent** (`$HERMES_HOME/state.db`, default `~/.hermes`, plus `profiles/<name>/state.db`;
SQLite, opened read-only):
- Only `user` text and the `content` of `assistant` messages are kept. Tool calls, `tool` rows,
  reasoning columns and `system` messages are ignored.
- Also dropped: compression summaries (`_compressed_summary`), display-only carriers
  (`display_kind`, e.g. `hidden`), and the background-review harness prompts with the reply that
  followed them. Structured content stored as `\x00json:` keeps its text parts.
- Subagent, cron, kanban, tool, batch, one-shot and recovered sessions are skipped. Compression
  splits (`parent_session_id`) are indexed as their own sessions (`name #2`).
- Like OpenCode there is no byte offset: each run re-reads the sessions changed since the last one.
- Sessions from a profile resume with `hermes -p <profile>`.

**Indexing**:
- Each file's size, mtime, inode, first bytes and the byte offset of its last complete line are
  stored.
- Unchanged files cost one `stat`. Appended files are read from the stored offset.
- A half-written last line waits for the next run.
- A truncated or rewritten file triggers a clean re-parse of its session.
- Malformed lines are counted and skipped.
- Sessions whose log files were deleted are removed from the index.
- The index uses WAL mode, batched commits, FTS5 (`unicode61`, diacritics folded) and
  `user_version` migrations.
- The original logs are only ever opened read-only.

**Safety**:
- C0/C1 control characters, including ESC, are stripped from all session text before it is stored
  or rendered.
- Markdown links are not turned into terminal hyperlinks.
- Nothing in conversation text is ever executed.

## Development

```sh
cargo build --release          # target/release/human-history and target/release/hh
cargo test                     # unit tests, TUI scenarios and end-to-end CLI tests (tests/cli.rs)
cargo clippy --all-targets
```

The end-to-end tests run the binary with its own config, index and fake `claude` / `herdr`
executables; TUI tests drive the app with key events into an in-memory terminal. Neither touches
your real config, index or Herdr.

To add an agent: add a variant to `Agent` (`src/model.rs`), write `src/parsers/<agent>.rs` with a
`parse` function, wire it into `discover`, `titles` and `parse` in `src/parsers/mod.rs`, give it
default log paths in `PathsConfig` (`src/config.rs`) and a resume command in `src/resume.rs`.
