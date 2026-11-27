#!/usr/bin/env bash
# Builds the Slint host's Linux AppImage without bundling shared libraries
# (ADR-0017 decision 7 and 2026-11-27 amendment).
#
# Usage:
#   desktop-slint/packaging/build-appimage.sh <binary-path> <version> <output-dir>

set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 <binary-path> <version> <output-dir>" >&2
  exit 2
fi

binary="$1"
version="$2"
out_dir="$3"

if [ ! -f "$binary" ]; then
  echo "::error::binary not found: $binary" >&2
  exit 1
fi
if [ -z "$version" ] || [[ "$version" == */* ]] || [[ "$version" =~ [[:space:]] ]]; then
  echo "::error::invalid version: $version" >&2
  exit 1
fi

readonly APP_ID="mkv-anime-batch-slint"
readonly APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
readonly APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
readonly RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
readonly RUNTIME_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
slint_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$slint_dir/.." && pwd)"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

tools_dir="$work_dir/tools"
mkdir -p "$tools_dir"

fetch_pinned() {
  local url="$1" sha="$2" dest="$3"
  curl --retry 3 -fsSL -o "$dest" "$url"
  printf '%s  %s\n' "$sha" "$dest" | sha256sum -c - >&2
}

appimagetool="$tools_dir/appimagetool-x86_64.AppImage"
runtime="$tools_dir/runtime-x86_64"
fetch_pinned "$APPIMAGETOOL_URL" "$APPIMAGETOOL_SHA256" "$appimagetool"
fetch_pinned "$RUNTIME_URL" "$RUNTIME_SHA256" "$runtime"
chmod +x "$appimagetool"

appdir="$work_dir/${APP_ID}.AppDir"
mkdir -p \
  "$appdir/usr/bin" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/32x32/apps" \
  "$appdir/usr/share/icons/hicolor/128x128/apps" \
  "$appdir/usr/share/icons/hicolor/256x256/apps" \
  "$appdir/usr/share/icons/hicolor/512x512/apps" \
  "$appdir/usr/share/icons/hicolor/scalable/apps" \
  "$appdir/usr/share/doc/$APP_ID"

cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
set -eu
# Resolve the binary next to this AppRun, never from an inherited APPDIR (another AppImage's).
# The environment is passed on untouched.
here="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
exec "$here/usr/bin/mkv-anime-batch-slint" "$@"
EOF
chmod 0755 "$appdir/AppRun"

desktop="$script_dir/${APP_ID}.desktop"
install -m 0755 "$binary" "$appdir/usr/bin/$APP_ID"
install -m 0644 "$desktop" "$appdir/${APP_ID}.desktop"
install -m 0644 "$desktop" "$appdir/usr/share/applications/${APP_ID}.desktop"
install -m 0644 "$slint_dir/icons/128x128@2x.png" "$appdir/${APP_ID}.png"
install -m 0644 "$slint_dir/icons/32x32.png" "$appdir/usr/share/icons/hicolor/32x32/apps/${APP_ID}.png"
install -m 0644 "$slint_dir/icons/128x128.png" "$appdir/usr/share/icons/hicolor/128x128/apps/${APP_ID}.png"
install -m 0644 "$slint_dir/icons/128x128@2x.png" "$appdir/usr/share/icons/hicolor/256x256/apps/${APP_ID}.png"
install -m 0644 "$slint_dir/icons/icon.png" "$appdir/usr/share/icons/hicolor/512x512/apps/${APP_ID}.png"
install -m 0644 "$slint_dir/icons/icon.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
install -m 0644 "$repo_root/LICENSE" "$appdir/usr/share/doc/$APP_ID/LICENSE"
install -m 0644 "$slint_dir/ui/fonts/OFL-JetBrainsMono.txt" "$appdir/usr/share/doc/$APP_ID/OFL-JetBrainsMono.txt"
install -m 0644 "$slint_dir/ui/fonts/OFL-Onest.txt" "$appdir/usr/share/doc/$APP_ID/OFL-Onest.txt"
install -m 0644 "$slint_dir/ui/fonts/OFL-Unbounded.txt" "$appdir/usr/share/doc/$APP_ID/OFL-Unbounded.txt"

mkdir -p "$out_dir"
out_file="$out_dir/${APP_ID}-${version}-x86_64.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$appimagetool" --no-appstream --runtime-file "$runtime" "$appdir" "$out_file"
chmod 0755 "$out_file"
printf '%s\n' "$out_file"
