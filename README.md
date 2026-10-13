# MKV Anime Batch (`mkvanimebatch`)

[![CI](https://github.com/Si-Forces/mkvanimebatch/actions/workflows/ci.yml/badge.svg)](https://github.com/Si-Forces/mkvanimebatch/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)](https://github.com/Si-Forces/mkvanimebatch/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

MKV Anime Batch processes a folder of anime fansub episodes (Matroska `.mkv` files) in one run. It has two modes:

- **Remux**: writes a new MKV per episode with the best matching subtitle, one audio track, chapters, a cover, and the release's fonts. A FLAC audio track is transcoded to Opus. Subtitle timing can be shifted.
- **Extract**: pulls the best subtitle, the chapters, the cover, and the fonts out of each episode, with fonts deduplicated across the whole release.

It is for people who keep or prepare fansub releases where subtitles, fonts, chapters, and covers are spread across season folders, sit next to or above the videos, or repeat in every episode. It runs on Windows and Linux, as a desktop app and as a command-line tool (`mkv-cli`). Both use the same engine and the same rules.

It does not re-encode video. All media work is done by [MKVToolNix](https://mkvtoolnix.download/) and [FFmpeg](https://ffmpeg.org/), which you install separately.

Found a bug or have a question? [Open an issue](https://github.com/Si-Forces/mkvanimebatch/issues).

---

## Contents

- [Install](#install)
- [Desktop app](#desktop-app)
- [Command line (`mkv-cli`)](#command-line-mkv-cli)
- [How files are chosen](#how-files-are-chosen)
- [Output and safety](#output-and-safety)
- [Troubleshooting](#troubleshooting)
- [Building from source](#building-from-source)
- [Development](#development)
- [License](#license)

---

## Install

### 1. Download a release

Get the files from [GitHub Releases](https://github.com/Si-Forces/mkvanimebatch/releases).

| Platform | Desktop app | CLI |
| :--- | :--- | :--- |
| Windows (x64) | `mkv-anime-batch.exe` (single executable), or `MKV_Anime_Batch_Portable_win64.zip` | `mkv-cli-x86_64-pc-windows-msvc.zip` |
| Linux (x86_64), any distribution | `MKV_Anime_Batch_*_amd64.AppImage` | `mkv-cli-x86_64-unknown-linux-gnu.tar.gz` |
| Debian, Ubuntu | `MKV_Anime_Batch_*_amd64.deb` | same as above |
| Fedora, RHEL | `MKV_Anime_Batch_*_x86_64.rpm` | same as above |

Each CLI archive has a `.sha256` checksum file next to it. Extract `mkv-cli` somewhere on your `PATH`, or run it by its full path. On Linux, make the AppImage executable first (`chmod +x MKV_Anime_Batch_*_amd64.AppImage`). There is no macOS build.

The Windows desktop app uses the Microsoft Edge WebView2 Runtime, which Windows 11 and current Windows 10 already include. If the window does not open, install the runtime from Microsoft.

### 2. Install the media tools

Both the desktop app and the CLI need these programs on your `PATH`:

| Tool | Used for |
| :--- | :--- |
| `mkvmerge`, `mkvextract` ([MKVToolNix](https://mkvtoolnix.download/)) | Writing remuxed files, extracting tracks and attachments, and reading files the built-in parser cannot |
| `ffmpeg` with the `libopus` encoder ([FFmpeg](https://ffmpeg.org/download.html)) | Transcoding FLAC audio to Opus. Only needed when a selected audio track is FLAC |

The packages below include `libopus`.

```powershell
# Windows: winget
winget install MKVToolNix.MKVToolNix
winget install Gyan.FFmpeg

# Windows: Scoop
scoop install mkvtoolnix ffmpeg
```

```bash
# Debian, Ubuntu, Mint
sudo apt install mkvtoolnix ffmpeg

# Fedora
sudo dnf install mkvtoolnix ffmpeg-free

# Arch Linux
sudo pacman -S mkvtoolnix-cli ffmpeg
```

Check that they are found with `mkvmerge --version` and `ffmpeg -hide_banner -encoders | grep libopus` (on Windows, `findstr libopus`).

---

## Desktop app

1. Pick **Remux** or **Extract** under **Pipeline** in the sidebar.
2. Click **Open folder…** and choose the release folder. The app scans it, including subfolders, and lists every episode, grouped by folder.
3. Tick the episodes to process. The arrows on a row change the order they run in.
4. Click an episode to open the inspector. It shows the output file, the audio and subtitle tracks, the subtitle ladder (which subtitle would be used, and why), and the fonts. In Remux mode you can assign a different subtitle there, with its own language, title, and offset.
5. Pick a preset in the sidebar, or open the settings sheet with the gear next to the mode. The sheet holds the input, output, fonts, and assets folders, the Opus bitrate and complexity, the subtitle offset, worker and disk I/O limits, **Dry run**, and **Bypass inspection cache**. Empty worker and disk I/O fields use the engine's defaults, shown as `auto (N)`.
6. Click **Run remux** or **Run extract**. Each row shows its progress and result. **Stop** cancels the batch and removes partial output. **Log** opens the log, which you can copy.

One batch runs at a time. If the window is reloaded during a run, it reconnects to the running batch. The theme follows the system light or dark setting until you toggle it.

The bottom bar shows CPU and RAM use and the disk write rate of the app and its `mkvmerge`, `mkvextract`, and `ffmpeg` processes.

---

## Command line (`mkv-cli`)

```bash
# Print the inspection manifest (JSON) for a release folder, writing no media
mkv-cli --input-dir "/path/to/series" --plan

# Show what a remux would do, without writing anything
mkv-cli --input-dir "/path/to/series" --dry-run

# Remux every episode into an Output/ folder beside each MKV
mkv-cli --input-dir "/path/to/series"

# Remux two episodes, in this order, with the hifi preset, into another folder
mkv-cli --input-dir "/path/to/series" --selected-items "01.mkv, 02.mkv" --preset hifi --out-dir "/path/to/output"

# Select a file whose name contains a comma
mkv-cli --input-dir "/path/to/series" --selected-item "[Group] Show - 01 [1080p, AAC].mkv"

# Extract subtitles, fonts, chapters, and covers with 4 workers
mkv-cli --input-dir "/path/to/series" --extract --workers 4

# Remux with manual subtitle assignments
mkv-cli --input-dir "/path/to/series" --assignments assignments.json
```

### Options

| Option | Description | Default |
| :--- | :--- | :--- |
| `--input-dir <DIR>` | Release folder to process; searched recursively | Required, except with `--help` or `--clear-cache` |
| `--out-dir <DIR>` | Output root. Each MKV's subfolder under `--input-dir` is mirrored beneath it | Remux: an `Output/` folder beside each MKV<br>Extract: the MKV's own folder |
| `--fonts-dir <DIR>` | Fonts folder to use instead of the one found automatically; must exist | Found automatically (see [Fonts](#fonts)) |
| `--assets-dir <DIR>` | Fallback folder for external subtitles, chapters, and covers; must exist | None |
| `--preset <PROFILE>` | Preset Profile, case-insensitive: `balanced` (192k, complexity 5), `fast` (128k, 5), `hifi` (320k, 10), `custom` (192k, 5). `default` means `balanced`; `high_quality` and `high-quality` mean `hifi` | `balanced` values (192k, complexity 5) |
| `--assignments <FILE>` | JSON file of subtitle Track Assignments (see [Track Assignments](#track-assignments)). If repeated, the last one wins | None |
| `--selected-item <PATH>` | Process this video. Repeatable. Taken literally, so commas in names are safe | All discovered videos |
| `--selected-items <LIST>` | Process these comma-separated videos. Repeatable. Each entry is trimmed; empty entries are dropped | All discovered videos |
| `--sub-offset <SEC>` | Subtitle timing offset in seconds, such as `1.5` or `-0.5`, rounded half away from zero to whole milliseconds. A Track Assignment's own offset replaces it | `0` |
| `--workers <N>` | Episodes processed at once, 1–64 | Physical CPU cores, capped at the CPUs the process may use, clamped to 1–16 |
| `--io-jobs <N>` | Disk-heavy steps run at once, 0–64. `0` means the worker count | `2` |
| `--ffmpeg-threads <N\|auto>` | FLAC decoder threads per transcode, 1–32, or `auto` | `auto`: cores ÷ workers, clamped to 1–4 |
| `--bitrate <RATE>` | Opus bitrate: a positive integer with an optional `k`, `K`, or `M` suffix, such as `128k`. Lowercase `m` is rejected, because FFmpeg reads it as "milli" | The preset's bitrate, else `192k` |
| `--complexity <0-10>` | Opus encoder complexity | The preset's complexity, else `5` |
| `--extract` | Extract instead of remux | Off |
| `--remux` | Remux | On, unless the executable is named `mkv-extract` |
| `--plan` | Print the Inspection Manifest (JSON, version 2) and exit. With a selection, keep only the selected items, in selection order | Off |
| `--dry-run` | Plan the batch and report each runnable item as `planned`, without writing, extracting, or transcoding | Off |
| `--events` | Print job events as NDJSON lines prefixed `@event: `, instead of the summary | Off |
| `--no-cache` | Do not read or write the inspection cache | Off |
| `--clear-cache` | Delete the inspection cache and exit | Off |
| `-h`, `--help` | Show help | |

Notes:

- `--extract` and `--remux` override each other (the last one wins) and the executable-name default. Renaming or linking the binary as `mkv-extract` makes extract the default.
- `--selected-item` and `--selected-items` need `--input-dir`. A selection may be relative to `--input-dir`, absolute, or relative to the current directory. It must be a discovered `.mkv` inside `--input-dir`, or the whole run fails. Selected videos run in the order given.
- Unknown arguments are an error. All values are checked before any work starts.

Exit codes: `0` on success; `1` on an invalid argument, when any episode ends in error, or on a fatal error; `130` when a batch is cancelled with Ctrl-C (or SIGTERM on Linux). Cancelling `--plan` exits `1`.

### Tuning

- **`--workers`** sets how many episodes are in flight. It is also the main control over CPU use, because each Opus encode uses about one core.
- **`--io-jobs`** limits how many episodes run `mkvmerge` or `mkvextract` and move their output at the same time. Transcoding is outside this limit, so it overlaps with other episodes' muxing. Keep `2` on a spinning hard drive; an SSD can take `4`–`8`.
- **`--ffmpeg-threads`** only limits FLAC decoding threads. Encoding is single-threaded per episode whatever you set here. The `auto` value keeps a batch from starting many idle decoder threads.

### Track Assignments

A Track Assignment tells the tool which subtitle file to use for one episode. `--assignments` accepts any of these JSON shapes:

```json
[
  {
    "target_video": "Season 1/Show - 01.mkv",
    "asset_path": "subs/Show - 01 [v2].ass",
    "language": "ind",
    "title": "Indonesian",
    "offset_ms": 1500
  }
]
```

```json
{ "assignments": [ { "target_video": "Show - 01.mkv", "asset_path": "Show - 01.ass" } ] }
```

```json
{ "Show - 01.mkv": "Show - 01.ass" }
```

`target_video` is the video's path relative to `--input-dir`. `asset_path` is absolute, or relative to `--input-dir` (then `--assets-dir`). `language`, `title`, and `offset_ms` are optional; `offset_ms` replaces `--sub-offset` for that episode, and `0` means no shift. A target that does not exist, a missing subtitle, or two assignments for one video fail the run before it starts.

---

## How files are chosen

### Subtitles

For each episode, the first source that has a match wins:

1. A manual Track Assignment.
2. An external subtitle in the video's own folder.
3. A subtitle track inside the video.
4. An external subtitle in the nearest parent folder (up to `--input-dir`).
5. An external subtitle in `--assets-dir`.

The source order beats language: an English `.ass` next to the video wins over an embedded Indonesian track. External files match by episode identity parsed from the file name (`S01E02`, `- 02`, `[02]`, `EP02`, `第02話`, OVA and specials, fractional episodes such as `12.5`). Within one folder, a file of the video's known season beats one with no season; a nearer folder still wins first. An episode with no matching subtitle is skipped.

Within one source, any ASS subtitle beats any other format (SSA included). Within one format, subtitles rank by language: Indonesian, then undetermined (`und`), then English, then others. Ties go to names that mark dialogue (`Full`, `Dialogue`, `Main`, and similar). Extract mode only extracts ASS and SSA tracks.

### Audio

One audio track is kept. Japanese is preferred, then undetermined language (`und`), then any stereo track. Within Japanese and `und`, a stereo track named as dialogue comes first, then any stereo track, then any layout. Remaining ties go to the earliest track. Tracks named like commentary or audio description are used only when every track looks like one. A FLAC track is transcoded to Opus; other codecs are copied.

### Chapters and covers

- **Chapters**: an `.xml` file containing a `<Chapters>` element, whose name parses to the episode. The nearest folder wins. A chapter file in the release folders replaces the video's own chapters. A release's single chapter file with no episode number, and `--assets-dir` chapters, are used only when the video has none.
- **Covers**: `cover.<ext>` and `folder.<ext>` apply to a folder and its subfolders. `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, and `<video-stem>.<ext>` apply to one episode. The nearest folder wins, then `_cover`, `.cover`, the bare stem, `cover`, and `folder`, in that order. The bare-stem form only matches a video in the same folder. Covers in the release folders replace embedded artwork; `--assets-dir` covers are used only when the video has none.

### Fonts

- **Remux** attaches every font in the release's fonts folder: `--fonts-dir`, else the nearest folder from the video up to `--input-dir` that has a `fonts`, `font`, or `attachments` folder with fonts in it, else the one in `--assets-dir`, else `<video folder>/fonts`. When that folder has no fonts, the video's own font attachments are kept.
- **Extract** saves each new font once, into `--fonts-dir` or a `fonts` folder under the output folder. A font with the same name and size, or the same content (BLAKE2b hash), as one already there is not written again. A different font whose name is taken is saved as `<name>_<hash prefix>.<ext>`, so existing files are never overwritten.

---

## Output and safety

- Remuxed files are named after the source, with an `OVA<ep>_` or `SP<ep>_` prefix for OVAs and specials. Extracted files are named `<stem>_EP<ep>_<lang>_track<id>.ass`, and `<stem>_EP<ep>_chapters.xml` (`Part`, `OVA`, or `SP` replace `EP` where they apply, and the episode part is left out when the name has none), and `<stem>_cover<ext>`.
- Work is written to a hidden temporary folder inside the destination and moved into place only when complete. Stopping a batch or a failure removes the temporary folder, so a partial file never appears under its final name.
- An existing output file is replaced with a backup-and-restore step, so a failed replace keeps the old file.
- Media file metadata is cached, keyed on path, size, and modification time, so a second scan of the same folder is fast. The cache is `inspect_cache.json` in `%LOCALAPPDATA%\mkvanimebatch` on Windows and `~/.cache/mkvanimebatch` on Linux. Use **Clear inspection cache…** in the settings sheet or `mkv-cli --clear-cache` to delete it.

---

## Troubleshooting

**The AppImage crashes at startup or shows a blank window.** Run it with `WEBKIT_DISABLE_DMABUF_RENDERER=1` to use software rendering, and [open an issue](https://github.com/Si-Forces/mkvanimebatch/issues) with your GPU and driver. With the NVIDIA driver loaded, the AppImage already sets this. Do not set it for the `.deb` or `.rpm` packages: there it breaks the window's rendering ([ADR-0015](docs/adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md)).

**An episode was skipped or failed.** The log line for that episode says why, for example no matching subtitle, or the `mkvmerge` or `ffmpeg` error.

**More detail.** The engine writes its own warnings (an unreadable cache, `mkvmerge -J` warnings) as diagnostics. The default level is `warn`. Set `RUST_LOG` to see more. `RUST_LOG` replaces the default, so start it with `warn,` to keep other warnings when raising one module.

- **CLI**: diagnostics go to stderr, so stdout holds only the `--plan` JSON, the `--events` lines, or the summary.
- **Desktop**: diagnostics go to a log file and to stderr. The file is emptied at startup when it is over 1 MB. The Windows app has no console, so the file is the only output there.

  | OS | Log file |
  | :--- | :--- |
  | Linux | `$XDG_DATA_HOME/com.mkvanimebatch.desktop/logs/mkv-anime-batch.log` (`~/.local/share` when unset) |
  | Windows | `%LOCALAPPDATA%\com.mkvanimebatch.desktop\logs\mkv-anime-batch.log` |

```bash
# Everything at debug level
RUST_LOG=debug mkv-cli --input-dir /path/to/series --plan

# Only the font code at debug level, everything else at warn
RUST_LOG=warn,mkv_core::fonts=debug mkv-cli --input-dir /path/to/series

# Desktop on Linux: start it from a terminal to see stderr too
RUST_LOG=debug ./MKV_Anime_Batch_*_amd64.AppImage
```

---

## Building from source

Requirements:

- [Rust](https://rustup.rs/), the current stable toolchain (CI builds with the latest stable).
- On Linux, the Tauri system libraries (Debian and Ubuntu names):

  ```bash
  sudo apt install build-essential curl wget file libssl-dev libgtk-3-dev \
    libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
  ```

```bash
# CLI
cargo build --release -p mkv-cli
./target/release/mkv-cli --help

# Desktop app
cargo install tauri-cli --version "^2" --locked
cd desktop-tauri
cargo tauri build
```

---

## Development

The repository is a Cargo workspace:

```
mkvanimebatch/
├── crates/
│   ├── mkv-core/        # Media engine library: probing, matching, fonts, remux, extract, batch pipeline
│   └── mkv-cli/         # Command-line front end
├── desktop-tauri/
│   ├── src-tauri/       # Tauri v2 host; runs mkv-core in-process
│   └── ui/              # Frontend: plain ES modules and CSS, no framework or bundler
├── docs/
│   ├── adr/             # Architecture Decision Records
│   ├── agents/          # Conventions for coding agents (issue tracker, labels, domain docs)
│   └── release-notes.md
├── scripts/             # Git hooks and the review-ledger script
└── .github/             # CI and release workflows
```

Run the same checks as CI (Node.js 20 or newer for the UI tests):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
```

`crates/mkv-core/tests/test_real_remux.rs` runs the real `ffmpeg`, `mkvmerge`, and `mkvextract` and is skipped when they are missing. Linux CI sets `MKV_REQUIRE_MEDIA_TOOLS=1`, which makes a missing tool a failure:

```bash
MKV_REQUIRE_MEDIA_TOOLS=1 cargo test -p mkv-core --test test_real_remux
```

Design documents:

- [architecture.md](architecture.md): the detailed system description.
- [CONTEXT-MAP.md](CONTEXT-MAP.md), [crates/mkv-core/CONTEXT.md](crates/mkv-core/CONTEXT.md), and [desktop-tauri/CONTEXT.md](desktop-tauri/CONTEXT.md): the domain vocabulary.
- [docs/adr/](docs/adr/): why the system is built the way it is (ADR-0001 to ADR-0015).

---

## License

[MIT](LICENSE).
