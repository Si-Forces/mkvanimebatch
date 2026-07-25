#!/usr/bin/env bash
# build-sidecars.sh
# Builds mkv_remux_fast.py and mkv_extract_fast.py as standalone Linux executables.
# Usage (from repo root):  bash desktop-tauri/build-sidecars.sh
# or from desktop-tauri/:  bash build-sidecars.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$SCRIPT_DIR/src-tauri/binaries"
TRIPLE="x86_64-unknown-linux-gnu"

mkdir -p "$OUT_DIR"

echo "==> Building mkv-remux sidecar..."
python3 -m PyInstaller \
    --onefile --console \
    --name "mkv-remux-$TRIPLE" \
    --distpath "$OUT_DIR" \
    --workpath "/tmp/pyinstaller-work-remux" \
    --specpath "/tmp/pyinstaller-spec-remux" \
    --noconfirm \
    "$REPO_ROOT/mkv_remux_fast.py"

echo "==> Building mkv-extract sidecar..."
python3 -m PyInstaller \
    --onefile --console \
    --name "mkv-extract-$TRIPLE" \
    --distpath "$OUT_DIR" \
    --workpath "/tmp/pyinstaller-work-extract" \
    --specpath "/tmp/pyinstaller-spec-extract" \
    --noconfirm \
    "$REPO_ROOT/mkv_extract_fast.py"

# Ensure binaries have executable permissions
chmod +x "$OUT_DIR"/mkv-* 2>/dev/null || true

echo ""
echo "==> Sidecars built successfully:"
ls -lh "$OUT_DIR"/mkv-remux-$TRIPLE "$OUT_DIR"/mkv-extract-$TRIPLE 2>/dev/null || true
