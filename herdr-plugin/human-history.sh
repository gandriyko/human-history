#!/bin/sh
# Launcher for the human-history CLI from Herdr (plugin commands run with a minimal PATH).
# Order: explicit override, the binary built in the plugin checkout, then one on PATH.
PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if [ -n "$HUMAN_HISTORY_BIN" ] && [ -x "$HUMAN_HISTORY_BIN" ]; then
  exec "$HUMAN_HISTORY_BIN" "$@"
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
if [ -x "$root/target/release/human-history" ]; then
  exec "$root/target/release/human-history" "$@"
fi
if command -v human-history >/dev/null 2>&1; then
  exec human-history "$@"
fi
echo "human-history not found: run 'cargo build --release' in $root (or set HUMAN_HISTORY_BIN)" >&2
# Keep a pane open long enough to read the error.
[ "$1" = "tui" ] && sleep 5
exit 127
