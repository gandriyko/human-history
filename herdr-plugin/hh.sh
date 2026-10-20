#!/bin/sh
# Launcher for the human-history CLI from Herdr (plugin commands run with a minimal PATH).
# Order: $HUMAN_HISTORY_BIN, an installed `human-history` (uv tool / pipx), then the checkout via uv.
PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if [ -n "$HUMAN_HISTORY_BIN" ] && [ -x "$HUMAN_HISTORY_BIN" ]; then
  exec "$HUMAN_HISTORY_BIN" "$@"
fi
if command -v human-history >/dev/null 2>&1; then
  exec human-history "$@"
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
if command -v uv >/dev/null 2>&1 && [ -f "$root/pyproject.toml" ]; then
  exec uv run --quiet --project "$root" human-history "$@"
fi
echo "human-history not found: run 'uv tool install $root' (or set HUMAN_HISTORY_BIN)" >&2
# Keep a pane open long enough to read the error.
[ "$1" = "tui" ] && sleep 5
exit 127
