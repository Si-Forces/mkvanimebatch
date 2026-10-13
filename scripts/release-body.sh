#!/usr/bin/env bash
# Builds the GitHub release page for a tag from docs/release-notes.md.
# Used by .github/workflows/release.yml (the gate and the Linux build job).
#
# Usage:
#   scripts/release-body.sh notes <version> [notes-file]
#       Print the notes of the "## <version>" section: the intro and the
#       "### Downloads" subsection are dropped, other "###" headings become "##".
#   scripts/release-body.sh page <version> <bundle-dir> [notes-file]
#       Print the full release page. Needs WEBKIT, JSC and GTK in the environment.
#
# <version> has no leading "v" and is matched literally against the heading.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Print the promoted notes of one section. Exit 1 with an ::error:: if the
# section is missing or has nothing but its intro and Downloads.
notes() {
  local version="$1" file="$2" rc=0 out
  # ENVIRON (not -v) keeps the version literal: -v would process backslash escapes.
  out="$(V="$version" awk '
    /^## / { insec = ($2 "" == ENVIRON["V"]); found = found || insec; keep = 0; next }
    insec && /^### / {
      h = $0; sub(/[ \t\r]+$/, "", h)
      keep = (h != "### Downloads")
      if (keep) sub(/^#/, "")
    }
    insec && keep
    END { if (!found) exit 3 }
  ' "$file")" || rc=$?
  if [ "$rc" -eq 3 ]; then
    echo "::error::$file has no '## $version' section" >&2
    return 1
  elif [ "$rc" -ne 0 ]; then
    return "$rc"
  fi
  if [ -z "$out" ]; then
    echo "::error::'## $version' in $file has no notes: add a '###' section other than Downloads" >&2
    return 1
  fi
  printf '%s\n' "$out"
}

page() {
  local version="$1" dir="$2" file="$3" notes_text f
  : "${WEBKIT:?WEBKIT is not set}" "${JSC:?JSC is not set}" "${GTK:?GTK is not set}"

  # The table below names these files; fail if the bundles came out under other names.
  for f in "MKV_Anime_Batch_${version}_amd64.AppImage" "MKV_Anime_Batch_${version}_amd64.deb" \
           "MKV_Anime_Batch-${version}-1.x86_64.rpm"; do
    if [ ! -e "$dir/$f" ]; then
      echo "::error::expected bundle $dir/$f is missing" >&2
      return 1
    fi
  done

  notes_text="$(notes "$version" "$file")"

  cat <<'EOF'
MKV Anime Batch is a desktop app and a command-line tool (`mkv-cli`) that remux or extract a whole folder of anime fansub MKV episodes in one run. See the [README](https://github.com/Si-Forces/mkvanimebatch#readme) for install and usage.

## Downloads

### Windows (x64)

| File | Description |
|------|-------------|
| `mkv-anime-batch.exe` | Desktop app, single executable |
| `MKV_Anime_Batch_Portable_win64.zip` | Desktop app, portable zip |
| `mkv-cli-x86_64-pc-windows-msvc.zip` | Command-line tool |

### Linux (x86_64)

| File | Description |
|------|-------------|
EOF
  printf '| `MKV_Anime_Batch_%s_amd64.AppImage` | Desktop app for any distribution |\n' "$version"
  printf '| `MKV_Anime_Batch_%s_amd64.deb` | Desktop app for Debian and Ubuntu |\n' "$version"
  printf '| `MKV_Anime_Batch-%s-1.x86_64.rpm` | Desktop app for Fedora and RHEL |\n' "$version"
  cat <<'EOF'
| `mkv-cli-x86_64-unknown-linux-gnu.tar.gz` | Command-line tool |
| `APPIMAGE-RUNTIME.txt` | GTK and WebKitGTK versions bundled in the AppImage |

Each CLI archive has a `.sha256` file. There is no macOS build.

## Requirements

- `mkvmerge` and `mkvextract` ([MKVToolNix](https://mkvtoolnix.download/)), and `ffmpeg` with `libopus` ([FFmpeg](https://ffmpeg.org/)), all on your `PATH`.
- The Windows MKVToolNix installer does not add itself to `PATH`. Add `C:\Program Files\MKVToolNix` yourself.
- The Windows desktop app needs the Microsoft Edge WebView2 Runtime, which current Windows includes.
- On Linux, run `chmod +x` on the AppImage before starting it.

## Quick start (CLI)

With `mkv-cli` on your `PATH` (or run it by its full path):

```bash
# Print the inspection manifest (JSON), writing no media
mkv-cli --input-dir "/path/to/series" --plan

# Show what a remux would do, without writing anything
mkv-cli --input-dir "/path/to/series" --dry-run

# Remux every episode into an Output/ folder beside each MKV
mkv-cli --input-dir "/path/to/series"
```

The full option list is in the [README](https://github.com/Si-Forces/mkvanimebatch#command-line-mkv-cli).

EOF
  printf '%s\n\n' "$notes_text"
  cat <<'EOF'
## AppImage bundled runtime

The AppImage ships its own GTK and WebKitGTK instead of the host's, and they decide how its window renders ([ADR-0015](https://github.com/Si-Forces/mkvanimebatch/blob/main/docs/adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md)). If it crashes at startup or shows a blank window, run it with `WEBKIT_DISABLE_DMABUF_RENDERER=1`. This build bundles:

| Library | Version |
|---------|---------|
EOF
  printf '| `libwebkit2gtk-4.1.so.0` | `%s` |\n' "$WEBKIT"
  printf '| `libjavascriptcoregtk-4.1.so.0` | `%s` |\n' "$JSC"
  printf '| `libgtk-3.so.0` | `%s` |\n' "$GTK"
}

case "${1:-}" in
  notes)
    [ $# -ge 2 ] && [ $# -le 3 ] || { echo "usage: $0 notes <version> [notes-file]" >&2; exit 2; }
    notes "$2" "${3:-$root/docs/release-notes.md}"
    ;;
  page)
    [ $# -ge 3 ] && [ $# -le 4 ] || { echo "usage: $0 page <version> <bundle-dir> [notes-file]" >&2; exit 2; }
    page "$2" "$3" "${4:-$root/docs/release-notes.md}"
    ;;
  *)
    echo "usage: $0 notes <version> [notes-file] | page <version> <bundle-dir> [notes-file]" >&2
    exit 2
    ;;
esac
