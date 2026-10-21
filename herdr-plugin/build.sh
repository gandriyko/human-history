#!/bin/sh
set -eu

# Herdr runs this from the plugin directory when installing from GitHub.
PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if ! command -v uv >/dev/null 2>&1; then
  echo "human-history requires uv: https://docs.astral.sh/uv/getting-started/installation/" >&2
  exit 127
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
exec uv sync --locked --no-dev --project "$root"
