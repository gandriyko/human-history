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
uv sync
uv run pytest
uv run ruff check src tests && uv run ruff format --check src tests
```

To add an agent, implement `parsers/base.Parser` (`discover`, `parse`, `titles`), register it in
`parsers/__init__.py`, and add a resume command in `integrations/resume.py`.
