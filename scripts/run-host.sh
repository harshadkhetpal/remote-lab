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

PORT="${PORT:-9753}"
FPS="${FPS:-12}"
QUALITY="${QUALITY:-72}"
MONITOR="${MONITOR:-0}"

IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || echo 127.0.0.1)"

echo "Building (first time will take a few minutes)…"
cargo build --release --bin remote-host

echo
echo "==============================================="
echo " Open this URL on your phone (same Wi-Fi):"
echo "   http://${IP}:${PORT}/"
echo "==============================================="
echo

exec ./target/release/remote-host \
  --bind "0.0.0.0:${PORT}" \
  --monitor "${MONITOR}" \
  --fps "${FPS}" \
  --jpeg-quality "${QUALITY}"
