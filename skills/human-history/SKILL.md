---
name: human-history
description: Search and read the user's past coding-agent conversations (Claude Code, Codex CLI, OpenCode, GitHub Copilot CLI, pi, Hermes Agent) offline with the `hh` CLI and its JSON output. Use when the user refers to earlier work or decisions ("як ми це робили раніше", "what did we decide about X", "find the session where I fixed Y", "what did I ask Codex last week"), or when context from a previous session would help.
---

# human-history (`hh`)

`hh` (same as `human-history`) indexes local agent session logs into SQLite and searches only the
human conversation: the user's prompts and the agents' visible answers. Tool calls, tool output,
reasoning and system messages are never indexed, so they cannot be found here.

Never run `hh` without a subcommand, `hh tui` or `hh resume-last`: the first two open an
interactive TUI, the last one replaces the process with an agent session.

## 1. Refresh the index

`hh search` reads the index as it is. Update it first (incremental, usually well under a second):

```sh
hh index
```

## 2. Search with `--json`

```sh
hh search "QUERY" --json [options]
```

Query syntax: words are ANDed, the last bare word matches as a prefix (`retr` finds `retry`),
`"quoted phrases"` match exactly. Case and diacritics are ignored.

| Option | Values (default first) | Meaning |
|---|---|---|
| `-s, --scope` | `both`, `user`, `assistant`, `title` | Search prompts+answers, only the user's prompts, only agent answers, or session titles (fuzzy) |
| `-f, --filter` | `all`, `repository`, `current-worktree` | Limit to the git repository (all its worktrees) or worktree of the **current directory**; run from the project directory |
| `--agent` | `claude`, `codex`, `opencode`, `copilot`, `pi`, `hermes` | One agent only |
| `--branch` | name | Sessions recorded on this git branch |
| `--since`, `--until` | ISO date or time, e.g. `2026-10-01` | Last activity in `[since, until)` (UTC) |
| `--cwd` | path | Sessions started in exactly this directory |
| `--limit` | `20` | Maximum sessions returned |

Text scopes rank sessions by relevance (best match first); `title` ranks by fuzzy score. `--sort`
does not change the order of a text search.

The result is a JSON array (`[]` when nothing matches), one object per session:

| Field | Meaning |
|---|---|
| `session_id` | Pass it (or a unique prefix) to `hh show` |
| `key` | `agent:session_id`, unique across agents |
| `agent`, `title` | Agent name and session title |
| `cwd`, `worktree`, `repo`, `branch` | Where the session ran (`null` when unknown) |
| `created_at`, `updated_at` | UTC, ISO 8601 |
| `user_count`, `assistant_count` | Number of prompts and answers |
| `snippet` | Text around the best match, plain text that may span lines (`…` marks cuts) |
| `hit_role` | `user` or `assistant`: who wrote the best match |
| `hits`, `match_ids` | How many messages match, and their ids |
| `favorite`, `path` | Pinned in the TUI; the agent's log file or database |

With `-s title`, `snippet` and `hit_role` are `null` and `hits` is `0`.

## 3. Read a conversation

```sh
hh show SESSION_ID_OR_PREFIX
```

Prints the title, then the conversation as Markdown under `USER` / `<AGENT>` headers, oldest
first. Conversations can be long: pipe through `grep -n` or `sed -n` and read the relevant part
instead of the whole output.

## Recipes

```sh
# What did we decide about retries in this repository? (run from the project directory)
hh index && hh search "retry backoff" -f repository --json --limit 5 \
  | jq -r '.[] | "\(.session_id)  \(.updated_at[:10])  \(.title)\n    \(.snippet)"'

# Prompts I typed to Codex since October
hh search "migration" -s user --agent codex --since 2026-10-01 --json

# Find a session by its title, then read it
id=$(hh search "approval workflow" -s title --json --limit 1 | jq -r '.[0].session_id // empty')
[ -n "$id" ] && hh show "$id" | sed -n '1,120p'
```

## Errors

Exit code `0` on success (also when nothing matches), `1` for runtime errors (e.g. `hh show` with
no or an ambiguous match), `2` for invalid options or an invalid config; the message is on stderr.
