#!/bin/sh
# Launcher for the human-history CLI from Herdr (plugin commands run with a minimal PATH).
# Order: explicit override, the plugin checkout's built CLI, a global CLI, then uv.
PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if [ -n "$HUMAN_HISTORY_BIN" ] && [ -x "$HUMAN_HISTORY_BIN" ]; then
  exec "$HUMAN_HISTORY_BIN" "$@"
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
if [ -x "$root/.venv/bin/human-history" ]; then
  exec "$root/.venv/bin/human-history" "$@"
fi
if command -v human-history >/dev/null 2>&1; then
  exec human-history "$@"
fi
if command -v uv >/dev/null 2>&1 && [ -f "$root/pyproject.toml" ]; then
  exec uv run --quiet --project "$root" human-history "$@"
fi
echo "human-history not found: run 'uv tool install $root' (or set HUMAN_HISTORY_BIN)" >&2
# Keep a pane open long enough to read the error.
[ "$1" = "tui" ] && sleep 5
exit 127
