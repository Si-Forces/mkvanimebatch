# MKV Anime Batch Tool (`mkvanimebatch`)

[![CI / Release](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml/badge.svg)](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml)
[![Python Version](https://img.shields.io/badge/python-3.10%20%7C%203.11%20%7C%203.12%20%7C%203.13-blue)](https://www.python.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)](https://github.com/sifahaz/mkvanimebatch/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

An automated, high-performance batch processing suite and native desktop GUI for anime and fansub MKV collections. `mkvanimebatch` provides intelligent subtitle track selection, font extraction & deduplication, 10-tier audio track scoring, subtitle timestamp shifting, crash-resilient atomic processing, and multi-threaded remuxing.

Available both as a fast **Python CLI suite** and as a native **Tauri v2 Desktop Application** with real-time hardware telemetry and live log streaming.

---

## Table of Contents

- [Key Features](#key-features)
- [System Requirements](#system-requirements)
- [Quick Start & CLI Usage](#quick-start--cli-usage)
  - [1. Subtitle & Asset Extraction](#1-subtitle--asset-extraction-mkv_extractpy)
  - [2. Batch Remuxing](#2-batch-remuxing-mkv_remuxpy)
  - [3. CLI Options Reference](#3-cli-options-reference)
  - [4. Interactive Windows Launchers](#4-interactive-windows-launchers)
- [Tauri Desktop Application](#tauri-desktop-application)
  - [Pre-built Releases](#pre-built-releases)
  - [Building from Source](#building-from-source)
- [Architecture & Advanced Features](#architecture--advanced-features)
- [Running Automated Tests](#running-automated-tests)
- [Project Layout](#project-layout)
- [License](#license)

---

## Key Features

### Intelligent Subtitle Selection & Extraction
* **Language Priority**: Automatically detects and scores subtitle tracks, prioritizing Indonesian (`ind`, `id`) and fallback English (`eng`, `en`).
* **Format & Track Type Scoring**: Prefers Advanced SubStation Alpha (`.ass` typesetting) over SubRip (`.srt`), prioritizing dialogue tracks over signs/titles or commentaries.
* **Format-Variant Coexistence**: Evaluates multiple subtitle formats (`.ass` vs `.srt`) for the same episode without prematurely discarding candidates.

### 10-Tier Audio Picker & Opus Transcoding
* **Dialogue Priority**: Automatically scores audio tracks, favoring Japanese stereo and multi-channel audio tracks.
* **Commentary Track Isolation**: Isolates commentary tracks into a secondary fallback pool, ensuring voice-actor/director commentary never overrides main film dialogue.
* **High-Efficiency Opus Conversion**: Transcodes audio to high-quality Opus with customizable bitrates (e.g. `128k`, `192k`) and compression complexity (`0`–`10`, default: `9`).

### Font Registry & Asset Deduplication
* **Embedded Font Detection**: Identifies font attachments from supported filename extensions and mkvmerge content types before transactional extraction and deduplication.
* **Font Table Deduplication**: Deduplicates fonts across entire seasons by full font name and content hash to eliminate redundant font copies.
* **Chapter & Cover Art Preservation**: Automatically extracts and embeds XML chapter definitions and front cover art (`cover.jpg`, `cover.png`, `folder.jpg`).

### Subtitle Timestamp Shifting & Preservation
* **Offset-Encoded Filenames**: Encodes timestamp offsets into output filenames (e.g., `episode_01.ind.[+5s].ass`) to allow instant skip checks on subsequent runs.
* **BOM Preservation**: Preserves UTF-8 byte-order marks (`utf-8-sig`) when adjusting subtitle timing.
* **Stale Offset Cleanup**: Automatically purges stale offset variants when re-running with a different timing offset.

### Crash-Resistant Output & Safe Recovery
* **Atomic Job Isolation**: Performs all remuxing and extraction inside isolated temporary scratch directories, promoting completed files via atomic move operations only upon successful exit codes.
* **Automatic Quarantine**: Interrupted files from power failures or cancellation are automatically quarantined and discarded, preventing corrupt MKV outputs.

### Concurrency Engine & Persistent Cache
* **Bounded Concurrency**: Uses configurable thread pools (`--workers`) paired with disk I/O throttles (`--io-jobs`, default: `2`) to prevent disk thrashing on HDDs and SSDs.
* **Persistent Probe Cache**: Caches `mkvmerge` JSON probe results to `.mkv_inspect_cache.json` with thread-safe file locks, dramatically speeding up repeated runs on large collections.
* **Nested Directory Hierarchy**: Preserves source folder hierarchies under `--out-dir` so episodes across different seasons never collide or overwrite each other.

---

## System Requirements

Make sure the following dependencies are installed and available in your system `PATH`:

1. **Python 3.10+** (for running CLI scripts)
2. **MKVToolNix** (`mkvmerge`, `mkvextract`) — [Download MKVToolNix](https://mkvtoolnix.download/)
3. **ffmpeg** (Required for Opus audio transcoding) — [Download ffmpeg](https://ffmpeg.org/download.html)

### Package Manager Installation

#### Windows (Winget / Chocolatey / Scoop)
```powershell
# Using Winget
winget install Python.Python.3.13
winget install MKVToolNix.MKVToolNix
winget install Gyan.FFmpeg

# Using Scoop
scoop install python mkvtoolnix ffmpeg
```

#### Linux (Debian / Ubuntu / Mint)
```bash
sudo apt update
sudo apt install -y python3 python3-pip mkvtoolnix ffmpeg
```

#### Linux (Fedora / RHEL)
```bash
sudo dnf install -y python3 mkvtoolnix ffmpeg-free
```

#### Linux (Arch Linux)
```bash
sudo pacman -S python mkvtoolnix-cli ffmpeg
```

---

## Quick Start & CLI Usage

Clone the repository:
```bash
git clone https://github.com/sifahaz/mkvanimebatch.git
cd mkvanimebatch
```

### 1. Subtitle & Asset Extraction (`mkv_extract.py`)

Extract best-matching subtitles, covers, XML chapters, and deduplicated fonts from all MKVs in a directory:

```bash
# Parallel extraction with custom output and font directories
python mkv_extract.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Extracted" --fonts-dir "D:/Anime/Fonts" --workers 8 --io-jobs 2

# Extract with a +5.0 second subtitle timestamp shift
python mkv_extract.py --input-dir "D:/Anime/Series" --sub-offset 5.0

# Dry-run inspection (preview matches without extracting)
python mkv_extract.py --input-dir "D:/Anime/Series" --dry-run
```

### 2. Batch Remuxing (`mkv_remux.py`)

Re-mux anime episodes with extracted subtitles, external chapter XMLs, covers, and attached fonts:

```bash
# High-throughput remuxing with Opus audio encoding (192k, complexity 10)
python mkv_remux.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Remuxed" --fonts-dir "D:/Fonts" --bitrate 192k --complexity 10 --workers 4 --io-jobs 2

# Remux using external assets from a prior extraction directory
python mkv_remux.py --input-dir "D:/Anime/Raw" --assets-dir "D:/Anime/Extracted" --out-dir "D:/Anime/Output"

# Keep extracted fonts folder after remuxing completes
python mkv_remux.py --input-dir "D:/Anime/Series" --keep-fonts

# Dry-run mode (validate audio/subtitle track scoring without modifying files)
python mkv_remux.py --input-dir "D:/Anime/Series" --dry-run
```

### 3. CLI Options Reference

| Option | Applies To | Description | Default |
| :--- | :--- | :--- | :--- |
| `--input-dir` | `extract`, `remux` | Directory containing MKV video files | Current working directory |
| `--out-dir` | `extract`, `remux` | Output directory for finished files | Extract: `<input-dir>`<br>Remux: `<input-dir>/Output` |
| `--fonts-dir` | `extract`, `remux` | Explicit font directory override | Auto-detected (`fonts/`, `attachments/`) |
| `--assets-dir` | `remux` | Fallback directory (and immediate parent) to search for subtitles, chapters, covers, or fonts | `None` |
| `--workers` | `extract`, `remux` | Maximum concurrent files to process (`1`–`64`) | Auto (CPU count, max `64`) |
| `--io-jobs` | `extract`, `remux` | Maximum concurrent disk I/O operations (`0` to disable throttling) | `2` |
| `--sub-offset` | `extract`, `remux` | Shift subtitle timestamps in seconds (negative = earlier, positive = later) | `0.0` |
| `--bitrate` | `remux` | Target bitrate for Opus audio conversion (e.g. `128k`, `192k`) | `192k` |
| `--complexity` | `remux` | Opus audio compression complexity level (`0`–`10`) | `10` |
| `--keep-fonts` | `remux` | Keep temporary extracted fonts directory after remuxing | `False` |
| `--dry-run` | `extract`, `remux` | Preview discovered files and track plans without writing files | `False` |
| `--events` | `extract`, `remux` | Emit structured machine-readable JSON events (`@event: {...}`) on stdout | `False` |
| `--plan` | `remux` | Output inspection manifest JSON for directory without executing | `False` |
| `--no-cache` | `extract`, `remux` | Bypass the persistent MKV probe inspect cache | `False` |
| `--clear-cache` | `extract`, `remux` | Clear `.mkv_inspect_cache.json` and exit without processing files | `False` |

### 4. Interactive Windows Launchers

Quick-start interactive scripts are provided for Windows users:

* **`run.bat`** — Interactive command prompt menu with toggles for Dry Run, Keep Fonts, I/O Jobs, Opus Complexity, and directory overrides.
* **`run.ps1`** — Modern PowerShell interactive launcher.

Both scripts include non-interactive EOF loop guards to safely handle redirected standard input without hanging.

---

## Tauri Desktop Application

A modern desktop GUI built with **Tauri v2** and **Rust**, featuring:
* **Liquid Glass Aesthetic**: Translucent frosted panels, macOS-style traffic light window controls, and seamless Dark/Light theme switching.
* **Real-time Hardware Telemetry**: Live CPU load percentage, RAM usage graphs, and disk I/O throughput indicators.
* **Streaming Logs**: Real-time STDOUT / STDERR process logs with ANSI color code rendering.
* **Native File Choosers**: Seamless folder pickers via native OS dialogs.
* **Instant Cancellation**: Gracefully stops sidecar subprocesses without leaving partial or corrupted output files.

### Pre-built Releases

Download ready-to-run releases directly from the [GitHub Releases](https://github.com/sifahaz/mkvanimebatch/releases) page:

* **Windows**: Standalone `mkv-anime-batch.exe` or `MKV_Anime_Batch_Portable_win64.zip`
* **Linux**: Universal `MKV_Anime_Batch_*_amd64.AppImage`, Debian/Ubuntu `.deb`, or Fedora/RHEL `.rpm`

### Building from Source

#### Prerequisites
- [Rust & Cargo](https://rustup.rs/) (Stable)
- [Node.js 20+](https://nodejs.org/) (for Tauri CLI if using npm, or install via `cargo install tauri-cli --version "^2"`)
- Python 3.10+ with `pyinstaller`

#### 1. Windows Build
```powershell
# 1. Compile Python sidecars to binaries/
powershell -ExecutionPolicy Bypass -File .\desktop-tauri\build-sidecars.ps1

# 2. Build the Tauri desktop app
cd desktop-tauri\src-tauri
cargo tauri build
```

#### 2. Linux Build (Ubuntu / Debian / Mint)
```bash
# 1. Install system dependencies
sudo apt update
sudo apt install -y build-essential curl wget file libssl-dev libgtk-3-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf python3-pip

# 2. Install PyInstaller & Tauri CLI
pip3 install pyinstaller --break-system-packages
cargo install tauri-cli --version "^2"

# 3. Build sidecars & bundle application
bash desktop-tauri/build-sidecars.sh
cd desktop-tauri/src-tauri
cargo tauri build --config tauri.linux.conf.json --bundles appimage,deb,rpm
```

---

## Architecture & Advanced Features

```
┌─────────────────────────────────────────────────────────────┐
│                    mkvanimebatch Suite                      │
├──────────────────────────────┬──────────────────────────────┤
│      Desktop GUI Layer       │        CLI Core Engine       │
│  Tauri v2 + Rust Sidecars    │   mkv_remux.py / extract.py  │
│  Live Telemetry & UI State   │   Multi-threaded Task Pool   │
└──────────────┬───────────────┴──────────────┬───────────────┘
               │                              │
               ▼                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      mkv_common.py                          │
├─────────────────────────────────────────────────────────────┤
│  • FontManager (Transactional Deduplication & Promotion)    │
│  • PersistentInspectCache (Thread-safe JSON Probe Cache)    │
│  • 10-Tier Audio Picker & Commentary Exclusion              │
│  • Subtitle Scorer (Indonesian / English & ASS / SRT)       │
│  • Timestamp Shifter (utf-8-sig BOM & Offset Encoders)      │
│  • Crash-Resistant Scratch Directories & Atomic Promotion   │
└─────────────────────────────────────────────────────────────┘
```

### 1. Asset & Subtitle Selection Hierarchy
When evaluating subtitle candidates for an episode:
1. **Language Check**: Indonesian (`ind`, `id`) tracks are scored higher than English (`eng`, `en`).
2. **Format Score**: Advanced SubStation Alpha (`.ass`) is scored above SubRip (`.srt`).
3. **Keyword Filtering**: Dialogue tracks receive high priority, while signs, songs, titles, and commentaries are deprioritized.
4. **Fallback Asset Search**: If an MKV contains no internal subtitle or cover, `--assets-dir` (as well as its immediate parent directory for shared ancestor assets) is searched using fuzzy episode number matching (e.g. `[01]`, `E01`, `EP01`, `01v2`).

### 2. Audio Scoring & Commentary Isolation
1. Non-commentary Japanese stereo and surround tracks score highest.
2. Voice-actor, director, and staff commentary tracks are isolated into a secondary pool and never selected as default dialogue audio.
3. Transcoding to Opus retains original channel layout and high dynamic range while dramatically reducing file size.

### 3. Safe Atomic Pipeline
```
[Source MKV] ──> [Inspect Cache] ──> [Temp Scratch Directory] ──> [Atomic Move] ──> [Final Output MKV]
                                             │ (on error/abort)
                                             └──> [Quarantine & Delete]
```

---

## Running Automated Tests

The test suite includes **300+ automated unit tests** covering subtitle shifting, filename and episode parsing, audio track scoring, font deduplication, multi-threaded cache concurrency, and CLI error handling.

To run the full test suite locally:

```bash
# Run pytest with concise tracebacks
python -m pytest tests/ --tb=short -v

# Run Rust desktop tests
cd desktop-tauri/src-tauri
cargo test --verbose
```

---

## Project Layout

```
mkvanimebatch/
├── mkv_common.py            # Shared parsing, scoring, caching, and font registry logic
├── mkv_extract.py           # CLI tool for extracting subtitles, chapters, covers & fonts
├── mkv_remux.py             # CLI tool for batch remuxing and audio transcoding
├── run.bat                  # Interactive Windows batch launcher
├── run.ps1                  # Interactive Windows PowerShell launcher
├── desktop-tauri/           # Native Tauri v2 desktop application
│   ├── build-sidecars.sh    # Linux PyInstaller sidecar build script
│   ├── build-sidecars.ps1   # Windows PyInstaller sidecar build script
│   ├── ui/                  # Web frontend (Liquid Glass UI, telemetry, controls)
│   └── src-tauri/           # Rust Tauri v2 backend and sidecar manager
├── tests/                   # 300+ pytest unit and integration test suites
│   ├── test_audio_picker.py
│   ├── test_cache_concurrency.py
│   ├── test_episode_parser.py
│   ├── test_font_registry.py
│   ├── test_subtitle_shift.py
│   └── test_track_selection.py
└── .github/workflows/       # GitHub Actions CI/CD matrix (Python 3.10-3.13, Pyright, Tauri)
```

---

## License

This project is open-source software licensed under the [MIT License](LICENSE).
