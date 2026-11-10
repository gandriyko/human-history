#!/bin/sh
set -eu

# Herdr runs this from the plugin directory when installing from GitHub.
PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if ! command -v cargo >/dev/null 2>&1; then
  echo "human-history requires a Rust toolchain: https://rustup.rs" >&2
  exit 127
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
exec cargo build --release --locked --manifest-path "$root/Cargo.toml"
