#!/usr/bin/env bash
# build-sidecars.sh
# Builds mkv-cli as standalone Linux sidecar executables.
# Usage (from repo root):  bash desktop-tauri/build-sidecars.sh
# or from desktop-tauri/:  bash build-sidecars.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$SCRIPT_DIR/src-tauri/binaries"
TRIPLE="x86_64-unknown-linux-gnu"

mkdir -p "$OUT_DIR"

echo "==> Building mkv-cli in release mode..."
cargo build --release -p mkv-cli
cp "$REPO_ROOT/target/release/mkv-cli" "$OUT_DIR/mkv-remux-$TRIPLE"
cp "$REPO_ROOT/target/release/mkv-cli" "$OUT_DIR/mkv-extract-$TRIPLE"
chmod +x "$OUT_DIR"/mkv-*

echo ""
echo "==> Sidecars built successfully:"
ls -lh "$OUT_DIR"/mkv-remux-$TRIPLE "$OUT_DIR"/mkv-extract-$TRIPLE 2>/dev/null || true
