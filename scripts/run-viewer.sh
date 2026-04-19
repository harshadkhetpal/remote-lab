#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found. Install Rust from https://rustup.rs and re-run." >&2
  exit 1
fi

URL="${1:-ws://127.0.0.1:9753/}"

echo "Building viewer…"
cargo build --release --bin remote-viewer

exec ./target/release/remote-viewer --url "${URL}"
