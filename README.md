# MKV Anime Batch Tool (`mkvanimebatch`)

[![CI](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml/badge.svg)](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2021%20%7C%201.88%2B-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)](https://github.com/sifahaz/mkvanimebatch/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A batch remux and extraction engine with a native desktop GUI for anime fansub releases. `mkvanimebatch` probes Matroska files with a native EBML parser, ranks subtitle and audio tracks, extracts and deduplicates fonts, transcodes FLAC audio to Opus, applies subtitle sync offsets, promotes outputs atomically, and processes files with bounded concurrency.

Available both as a standalone **CLI binary (`mkv-cli`)** and as a **Tauri v2 desktop application** that runs the engine in-process and shows system telemetry and streamed per-item progress.

---

## Table of Contents

- [Key Features](#key-features)
- [System Requirements](#system-requirements)
- [Quick Start & CLI Usage](#quick-start--cli-usage)
  - [1. Standalone CLI (`mkv-cli`)](#1-standalone-cli-mkv-cli)
  - [2. CLI Options Reference](#2-cli-options-reference)
  - [3. Concurrency & Performance Tuning](#3-concurrency--performance-tuning)
- [Tauri Desktop Application](#tauri-desktop-application)
  - [Pre-built Releases](#pre-built-releases)
  - [Building from Source](#building-from-source)
- [Architecture & Design](#architecture--design)
- [Running Automated Tests](#running-automated-tests)
- [Project Layout](#project-layout)
- [License](#license)

---

## Key Features

### Hybrid EBML Probing & Fast Preflight
* **Native Parsing**: Reads Matroska/EBML headers, tracks, attachments, and chapters in-process, without spawning a tool per file (ADR-0006).
* **Fallback Inspector**: Falls back to `mkvmerge -J` whenever native parsing fails.

### Subtitle Selection & Extraction
* **Language Priority**: Ranks subtitles by language: Indonesian (`ind`, then `id`), undetermined (`und`), English (`eng`, then `en`), then any other language.
* **Format Scoring**: Any ASS subtitle (`S_TEXT/ASS` track or `.ass` file) outranks every non-ASS subtitle. Among equal scores, a track or file whose name marks it as dialogue (e.g. `Full`, `Dialogue`, `Main`) wins. Extract mode extracts only ASS/SSA tracks.
* **Manual Track Assignments**: Per-episode subtitle overrides via `--assignments`, validated fail-fast before the batch starts.
* **Resolution Precedence Ladder**: Evaluates candidate subtitles in strict order: manual `TrackAssignment` > exact same-directory external subtitle > embedded subtitle track > nearest matching ancestor external subtitle > explicit `--assets-dir` fallback. Source precedence beats language score across sources.

### Audio Scoring & Opus Transcoding
* **Dialogue Priority**: Scores audio tracks, preferring Japanese (stereo dialogue, then stereo, then any channel layout), then undetermined-language tracks, then other stereo tracks.
* **Commentary Track Isolation**: Tracks whose names look like commentary or audio description are considered only when every audio track looks like one.
* **FLAC to Opus**: A selected FLAC audio track is transcoded to Opus with FFmpeg; other codecs are kept as-is. Preset Profiles (`balanced`, `fast`, `hifi`, `custom`) set the Opus bitrate and complexity.
* **Automatic Thread Governance**: Per-job FFmpeg threads default to `auto`: `(cores / workers)` clamped to 1–4, where cores are physical cores from `/proc/cpuinfo` on Linux, capped at the available parallelism (which accounts for cgroup CPU quotas and affinity where it can read them); the available parallelism elsewhere (ADR-0013).

### Transactional Font Registry & Asset Deduplication
* **Embedded Font Detection**: Identifies font attachments by filename extension or MIME type.
* **BLAKE2b Hash Deduplication**: Claims each font by name and size, then deduplicates by BLAKE2b-512 content hash against fonts already in the target fonts directory.
* **RAII Claim Safety**: A font claim that is never promoted, for example on cancellation or failure, is released and its staging file deleted when the claim guard drops.
* **Mode-Specific Font Handling**: Extraction mode retains extracted fonts as user output, while remux mode discovers and embeds fonts into output Matroska files.
* **Chapter & Cover Art Preservation**: Extracts and embeds XML chapter definitions and front cover art. Directory covers (`cover.<ext>`, `folder.<ext>`) and episode covers (`<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, `<video-stem>.<ext>`) resolve nearest applicable directory first (`_cover` > `.cover` > bare stem > `cover` > `folder`). The bare-stem form only matches a video in the same directory, so an `--assets-dir` cover meant for a video elsewhere must use one of the other four forms. Local and ancestor external covers supersede embedded artwork; explicit `--assets-dir` covers apply only when embedded art is absent.

### Crash-Resistant Output & Safe Recovery
* **Atomic File Promotion**: Remuxing and extraction write into a temporary scratch directory inside the destination, then promote each file by rename. An existing destination is replaced with backup-and-rollback; across filesystem boundaries (`EXDEV`) the file is copied to a temporary sibling and renamed into place.
* **Scratch Cleanup**: On cancellation or failure the scratch directory is removed, so no partial output reaches its final path.

---

## System Requirements

Ensure the following external media tools are installed and available in your system `PATH`:

1. **MKVToolNix** (`mkvmerge`, `mkvextract`) — [Download MKVToolNix](https://mkvtoolnix.download/)
2. **ffmpeg** (used to transcode FLAC audio tracks to Opus) — [Download ffmpeg](https://ffmpeg.org/download.html)

### Package Manager Installation

#### Windows (Winget / Scoop)
```powershell
# Using Winget
winget install MKVToolNix.MKVToolNix
winget install Gyan.FFmpeg

# Using Scoop
scoop install mkvtoolnix ffmpeg
```

#### Linux (Debian / Ubuntu / Mint)
```bash
sudo apt update
sudo apt install -y mkvtoolnix ffmpeg
```

#### Linux (Fedora / RHEL)
```bash
sudo dnf install -y mkvtoolnix ffmpeg-free
```

#### Linux (Arch Linux)
```bash
sudo pacman -S mkvtoolnix-cli ffmpeg
```

---

## Quick Start & CLI Usage

### 1. Standalone CLI (`mkv-cli`)

Download `mkv-cli` from [GitHub Releases](https://github.com/sifahaz/mkvanimebatch/releases) or compile from source:

```bash
# Inspect a series directory and generate structured Manifest v2 JSON
./mkv-cli --input-dir "/path/to/anime/series" --plan

# Plan inspection manifest filtered to specific episodes
./mkv-cli --input-dir "/path/to/anime/series" --selected-item "[Group] Show - 01 [1080p, AAC].mkv" --plan

# Dry-run batch remuxing (plans each file without remuxing, transcoding, or writing outputs)
./mkv-cli --input-dir "/path/to/anime/series" --dry-run

# Batch remux selected episodes with the balanced preset
./mkv-cli --input-dir "/path/to/anime/series" --selected-items "01.mkv, 02.mkv" --preset balanced

# Batch remux episodes into a custom output directory
./mkv-cli --input-dir "/path/to/anime/series" --out-dir "/path/to/output" --preset balanced

# Batch extract subtitles, fonts, chapters, and covers
./mkv-cli --input-dir "/path/to/anime/series" --extract --workers 4

# Remux with manual track assignments JSON
./mkv-cli --input-dir "/path/to/anime/series" --assignments "/path/to/assignments.json"
```

### 2. CLI Options Reference

| Option | Description | Default |
| :--- | :--- | :--- |
| `--input-dir <DIR>` | Source directory containing anime video releases; searched recursively | *Required* (except with `--help` or `--clear-cache`) |
| `--out-dir <DIR>` | Output root; each MKV's subdirectory under `--input-dir` is mirrored beneath it | Remux: an `Output/` directory beside each MKV<br>Extract: the MKV's own directory |
| `--fonts-dir <DIR>` | Explicit fonts directory override; must be an existing directory | None (auto-discovered) |
| `--assets-dir <DIR>` | Fallback directory for external subtitle/chapter/cover assets; must be an existing directory | None |
| `--preset <PROFILE>` | Preset profile, case-insensitive: `balanced` (192k, complexity 5), `fast` (128k, 5), `hifi` (320k, 10), `custom` (192k, 5). `default` is accepted for `balanced` and `high_quality` / `high-quality` for `hifi` | None (192k, complexity 5) |
| `--assignments <FILE>` | JSON file of subtitle Track Assignments: an array of `TrackAssignment` objects, an object whose `assignments` key holds such an array or a map, or a legacy `{"video.mkv": "sub.ass"}` map. If repeated, the last one wins | None |
| `--selected-item <PATH>` | Select one video file to process (repeatable, literal path; safe for filenames with commas) | None (all discovered files) |
| `--selected-items <LIST>` | Comma-separated list of video files to process (repeatable; trims whitespace and drops empty segments) | None (all discovered files) |
| `--sub-offset <SEC>` | Subtitle timestamp sync offset in seconds (e.g. `1.5`, `-0.5`) | `0.0` |
| `--workers <N>` | Number of concurrent worker tasks (1–64) | Physical cores, capped at the available parallelism, clamped to 1–16 |
| `--io-jobs <N>` | Concurrency limit for disk-heavy work (0–64; `0` uses the worker count) | `2` |
| `--ffmpeg-threads <N\|auto>` | CPU threads per audio transcode job (1–32); `auto` is case-insensitive | `auto`: `(cores / workers)` clamped to 1–4 |
| `--bitrate <RATE>` | Opus bitrate: a positive integer with an optional `k`/`K` or `M` suffix (e.g. `128k`, `192K`); lowercase `m` is rejected | Preset bitrate, else `192k` |
| `--complexity <0-10>` | Opus encoder complexity | Preset complexity, else `5` |
| `--extract` | Run batch extraction instead of remuxing | Off |
| `--remux` | Run batch remuxing | On, unless the executable is named `mkv-extract` |
| `--plan` | Print the Inspection Manifest (version 2) JSON and exit. With a selection, fails if any selected item is not a discovered MKV, then keeps only the selected items in selection order | Off |
| `--dry-run` | Plan the batch and report each runnable item as `planned`, without remuxing, extracting, or transcoding | Off |
| `--events` | Emit NDJSON job events, one per line, prefixed `@event: `; the summary log is not printed | Off |
| `--no-cache` | Bypass the persistent inspection cache | Off |
| `--clear-cache` | Clear the persistent inspection cache and exit | Off |
| `-h, --help` | Show usage help | |

`--extract` and `--remux` override each other (the last one wins) and the executable-name default. `--selected-item` and `--selected-items` require `--input-dir`; each selection is resolved relative to `--input-dir` (absolute paths and paths relative to the working directory are also accepted) and is rejected if it resolves outside it. Unknown arguments are an error.

Exit codes: `0` on success; `1` on an argument or validation error, when any file ends in error, or on a fatal pipeline error; `130` when cancelled by Ctrl-C/SIGINT (or SIGTERM on Unix).

### 3. Concurrency & Performance Tuning

* **`--workers`**: Governs overall concurrency (number of episodes in flight). Defaults to the core count clamped to 1–16: physical cores from `/proc/cpuinfo` on Linux, capped at the available parallelism (which accounts for cgroup CPU quotas and affinity where it can read them); the available parallelism elsewhere (see `crates/mkv-core/src/cpu.rs`).
* **`--io-jobs`**: Caps how many workers run disk-heavy work at once. Keeping `--io-jobs 2` prevents head thrashing on spinning hard drives (HDDs), while SSD/NVMe drives can scale to `4`–`8`. `0` sets the cap to the worker count.
* **`--ffmpeg-threads`**: Defaults to `auto`, deriving per-job threads as `(cores / workers)` clamped to 1–4 (ADR-0013). Set an explicit value to override the governor.
* **Queue Scoping & Ordering**: Specifying `--selected-item` or `--selected-items` restricts execution to the selected episodes in the exact specified order, running up to `--workers` concurrently across the active selection (ADR-0011, ADR-0014). Every selected item must be a discovered MKV file, otherwise the whole run fails.

---

## Tauri Desktop Application

A desktop GUI built with **Tauri v2**, featuring:
* **Liquid Glass Aesthetic**: Translucent frosted panels, a custom window frame, and light and dark themes. The theme follows the system color scheme until you toggle it; the choice is then remembered.
* **In-Process Engine Bridge**: Runs `mkv-core` in-process, with no sidecar process, and isolates pipeline panics so a failed job reports an error instead of taking the app down (ADR-0007).
* **Queue Tracker & Streaming Progress**: Each running card shows an indeterminate bar until the engine streams progress for it, then a determinate fill; finished cards show the engine-measured duration.
* **Live Telemetry & Logs**: System CPU and RAM usage, the disk write rate of the app and its mkvmerge/mkvextract/ffmpeg child processes, and a streaming log console.

### Pre-built Releases

Download ready-to-run desktop releases directly from [GitHub Releases](https://github.com/sifahaz/mkvanimebatch/releases):

* **Windows**: Standalone `mkv-anime-batch.exe` or portable `MKV_Anime_Batch_Portable_win64.zip`; CLI archive `mkv-cli-x86_64-pc-windows-msvc.zip`
* **Linux**: `MKV_Anime_Batch_*_amd64.AppImage`, Debian/Ubuntu `MKV_Anime_Batch_*_amd64.deb`, or Fedora/RHEL `MKV_Anime_Batch_*_x86_64.rpm`; CLI archive `mkv-cli-x86_64-unknown-linux-gnu.tar.gz`

Each CLI archive ships with a `.sha256` checksum file.

### Building from Source

#### Prerequisites
- [Rust & Cargo](https://rustup.rs/) (stable; the locked dependencies require 1.88 or newer)
- Linux dependencies (Debian/Ubuntu):
  ```bash
  sudo apt install -y build-essential curl wget file libssl-dev libgtk-3-dev \
    libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
  ```

#### 1. Compile `mkv-cli`
```bash
cargo build --release -p mkv-cli
./target/release/mkv-cli --help
```

#### 2. Compile Desktop Application
```bash
# Install Tauri CLI
cargo install tauri-cli --version "^2" --locked

# Build desktop application
cd desktop-tauri
cargo tauri build
```

---

## Architecture & Design

```
┌─────────────────────────────────────────────────────────────┐
│                    mkvanimebatch Suite                      │
├──────────────────────────────┬──────────────────────────────┤
│      Desktop GUI Host        │       Standalone CLI         │
│        (Tauri v2)            │         (mkv-cli)            │
│  Queue Tracker & Telemetry   │   Terminal & Scripting       │
└──────────────┬───────────────┴──────────────┬───────────────┘
               │                              │
               ▼                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   mkv-core (Media Engine)                   │
├─────────────────────────────────────────────────────────────┤
│  • Domain Heuristics (Audio Scoring, Subtitle Ranking)      │
│  • Hybrid EBML Probing (Native Parser + mkvmerge Fallback)  │
│  • Transactional Font Registry (BLAKE2b Hash Dedup)         │
│  • Unified Asset Resolver (Nearest-Directory Precedence)    │
│  • Batch Pipeline & Bounded Concurrency Engine              │
│  • Dual-Channel Streaming (Job Events & Log Channel)        │
└─────────────────────────────────────────────────────────────┘
```

See the architecture decision records in `docs/adr/` (0001–0015) for complete architectural rationale:
- [ADR-0005: Rust Engine Migration & Workspace Topology](docs/adr/0005-rust-engine-migration-and-workspace-topology.md)
- [ADR-0006: Hybrid EBML Probing & Channel Streaming](docs/adr/0006-hybrid-ebml-probing-and-channel-streaming.md)
- [ADR-0007: In-Process Engine Bridge & Sidecar Retirement](docs/adr/0007-in-process-engine-bridge.md)
- [ADR-0008: Typed Engine Bridge & Track Assignment Model](docs/adr/0008-typed-engine-bridge-and-track-assignment-model.md)
- [ADR-0009: Streaming Card Progress & Item Duration Model](docs/adr/0009-streaming-card-progress-and-item-duration-model.md)
- [ADR-0010: Mainline Rust Engine Cutover & Python Decommissioning](docs/adr/0010-mainline-rust-cutover-and-python-decommissioning.md)
- [ADR-0011: Selected Items Queue & Concurrency Pipelining](docs/adr/0011-selected-items-queue-and-concurrency-pipelining.md)
- [ADR-0012: Track Assignment Override Execution & Seam Unification](docs/adr/0012-track-assignment-override-execution-and-seam-unification.md)
- [ADR-0013: Asynchronous Probing and Transcode Governance Performance Overhaul](docs/adr/0013-asynchronous-probing-and-transcode-governance-performance-overhaul.md)
- [ADR-0014: CLI Selected Items Execution and Manifest Filtering](docs/adr/0014-cli-selected-items-execution-and-manifest-filtering.md)
- [ADR-0015: Per-Package WebKitGTK DMABUF Renderer Scoping](docs/adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md)

---

## Running Automated Tests

To run the complete automated test suite:

```bash
# Check formatting
cargo fmt --all -- --check

# Run Clippy across all crates
cargo clippy --workspace --all-targets -- -D warnings

# Run all Rust unit and integration tests (mkv-core, mkv-cli, desktop-tauri)
cargo test --workspace

# Run frontend UI contract and state machine tests (Node.js 20+; CI uses Node 20)
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
```

`crates/mkv-core/tests/test_real_remux.rs` drives real `ffmpeg`, `mkvmerge`, and `mkvextract` binaries and is skipped when they are unavailable. On Linux, CI and the release verification gate set `MKV_REQUIRE_MEDIA_TOOLS=1`, which turns a missing tool into a test failure:

```bash
MKV_REQUIRE_MEDIA_TOOLS=1 cargo test -p mkv-core --test test_real_remux
```

---

## Project Layout

```
mkvanimebatch/
├── Cargo.toml               # Unified Cargo workspace root
├── CONTEXT-MAP.md           # Bounded context map
├── AGENTS.md                # Agent workflow rules (review and CI gate before commits) and agent skill docs
├── crates/
│   ├── mkv-core/            # Standalone media engine library crate
│   │   ├── src/             # EBML parsing, CPU topology, domain heuristics, fonts, pipeline
│   │   └── tests/           # 11 integration suites (assets, domain, scan, remux, extract, fonts, pipeline, pipeline-assignments, pipeline-assignment-args, cross-regex, real-remux)
│   └── mkv-cli/             # Standalone CLI binary adapter
│       └── src/             # CLI argument parsing and entry point
├── desktop-tauri/           # Desktop GUI application
│   ├── ui/                  # Web frontend (Liquid Glass UI, Queue Tracker, 11 Node test suites)
│   └── src-tauri/           # In-process Tauri Engine Bridge & telemetry
├── docs/
│   ├── adr/                 # Architecture Decision Records (0001 - 0015)
│   ├── agents/              # Agent skill docs (issue tracker, triage labels, domain docs)
│   └── release-notes.md     # Release notes
└── .github/
    └── workflows/           # ci.yml (CI checks) and release.yml (release build & publish)
```

---

## License

This project is open-source software licensed under the [MIT License](LICENSE).
