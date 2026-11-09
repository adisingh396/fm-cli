#!/usr/bin/env bash
# Run the Crouton Alpha backend on a cloud computer where this repository is
# already cloned, for example inside a Box workspace.
set -euo pipefail

PORT="${PORT:-8080}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  if [ -f "$HOME/.cargo/bin/cargo" ]; then
    . "$HOME/.cargo/env"
  else
    echo "Rust/cargo is not installed. Install Rust first (https://rustup.rs), then rerun."
    exit 1
  fi
fi

# The API will build the world, load data/players.csv, and expose the live
# JSON API plus the web dashboard from web/.
PORT="$PORT" exec cargo run --release -- serve
