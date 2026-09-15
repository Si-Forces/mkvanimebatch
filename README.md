# MKV Anime Batch Tool (`mkvanimebatch`)

[![CI](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml/badge.svg)](https://github.com/sifahaz/mkvanimebatch/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2021%20%7C%201.80%2B-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)](https://github.com/sifahaz/mkvanimebatch/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

An automated, high-performance batch processing engine and native desktop GUI for anime fansub collections. `mkvanimebatch` provides native EBML probing, intelligent subtitle track selection, transactional font extraction & deduplication, 10-tier audio track scoring, Opus audio transcoding, subtitle sync offsets, crash-resilient atomic file promotion, and bounded multi-threaded processing.

Available both as a standalone native **CLI binary (`mkv-cli`)** and as a self-contained **Tauri v2 Desktop Application** with an in-process engine bridge, hardware telemetry, and real-time streaming progress.

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
* **Native In-Memory Parsing**: Scans Matroska/EBML containers in `<1ms` without process spawning overhead (ADR-0006).
* **Transparent Fallback**: Seamlessly falls back to `mkvmerge -J` when container quirks or unsupported extensions are encountered.

### Intelligent Subtitle Selection & Extraction
* **Language Priority**: Automatically detects and scores subtitle tracks, prioritizing Indonesian (`ind`, `id`) and fallback English (`eng`, `en`).
* **Format & Track Type Scoring**: Prefers Advanced SubStation Alpha (`.ass` typesetting) over SubRip (`.srt`), prioritizing dialogue tracks over signs/titles or commentaries.
* **Manual Track Assignments**: Supports precise per-episode subtitle overrides via `--assignments` while retaining automatic fallback.

### 10-Tier Audio Scoring & Opus Transcoding
* **Dialogue Priority**: Automatically scores audio tracks, favoring Japanese stereo and multi-channel audio tracks.
* **Commentary Track Isolation**: Isolates commentary tracks into a secondary fallback pool, ensuring commentary never overrides dialogue.
* **Preset Profiles**: Configurable Preset Profiles (`balanced`, `fast`, `hifi`, `custom`) targeting high-quality Opus encoding via FFmpeg.

### Transactional Font Registry & Asset Deduplication
* **Embedded Font Detection**: Identifies font attachments by MIME type and filename extensions before transactional extraction.
* **BLAKE2b Hash Deduplication**: Deduplicates fonts across entire releases by content hash and font identity, avoiding redundant font attachments.
* **RAII Claim Safety**: Uncommitted font claims in staging directories are safely defused or released on cancellation.
* **Chapter & Cover Art Preservation**: Extracts and embeds XML chapter definitions and front cover art (`cover.jpg`, `cover.png`, `folder.jpg`).

### Crash-Resistant Output & Safe Recovery
* **Atomic File Promotion**: Performs remuxing and extraction inside isolated temporary scratch directories, promoting files via atomic replacement with rollback protection across filesystem boundaries (`EXDEV`).
* **Automatic Quarantine**: Interrupted files from cancellation or failure are quarantined and discarded, preventing corrupt MKV outputs.

---

## System Requirements

Ensure the following external media tools are installed and available in your system `PATH`:

1. **MKVToolNix** (`mkvmerge`, `mkvextract`) — [Download MKVToolNix](https://mkvtoolnix.download/)
2. **ffmpeg** (Required for Opus audio transcoding) — [Download ffmpeg](https://ffmpeg.org/download.html)

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

# Dry-run batch remuxing simulation (validates tracks without modifying files)
./mkv-cli --input-dir "/path/to/anime/series" --dry-run

# Batch remux episodes with Opus audio conversion and custom output directory
./mkv-cli --input-dir "/path/to/anime/series" --out-dir "/path/to/output" --preset balanced

# Batch extract subtitles, fonts, chapters, and covers
./mkv-cli --input-dir "/path/to/anime/series" --extract --workers 4

# Remux with manual track assignments JSON
./mkv-cli --input-dir "/path/to/anime/series" --assignments "/path/to/assignments.json"
```

### 2. CLI Options Reference

| Option | Description | Default |
| :--- | :--- | :--- |
| `--input-dir <DIR>` | Source directory containing anime video releases | *Required* |
| `--out-dir <DIR>` | Destination output directory for processed files | Remux: `<input-dir>/Output`<br>Extract: MKV directory |
| `--preset <PROFILE>` | Named preset profile (`balanced`, `fast`, `hifi`, `custom`) | `balanced` |
| `--assignments <FILE>` | Path to JSON file mapping episode IDs to track paths | None |
| `--sub-offset <SEC>` | Subtitle timestamp sync offset in seconds (e.g. `1.5`, `-0.5`) | `0.0` |
| `--workers <N>` | Number of concurrent worker tasks | `4` |
| `--io-jobs <N>` | Bounded disk I/O jobs concurrency limit (`0` for unbounded) | `2` |
| `--ffmpeg-threads <N>` | CPU threads allocated per audio transcode job | `2` |
| `--bitrate <RATE>` | Opus audio transcoding bitrate (e.g. `128k`, `192k`) | Profile default |
| `--complexity <0-10>` | Opus compression complexity level | Profile default |
| `--keep-fonts` | Retain extracted fonts directory | `false` |
| `--extract` | Run batch extraction instead of remuxing | `false` |
| `--plan` | Output Manifest v2 JSON for directory and exit | `false` |
| `--dry-run` | Simulate batch processing without executing remux commands | `false` |
| `--events` | Emit machine-readable NDJSON events (`@event: {...}`) | `false` |
| `--no-cache` | Bypass persistent inspect cache | `false` |
| `--clear-cache` | Clear persistent inspection cache and exit | `false` |
| `-h, --help` | Show complete usage help | |

### 3. Concurrency & Performance Tuning

* **`--workers`**: Governs overall concurrency (number of episodes in flight).
* **`--io-jobs`**: Enforces a strict disk I/O semaphore. Keeping `--io-jobs 2` prevents head thrashing on spinning hard drives (HDDs), while SSD/NVMe drives can scale to `4`–`8`.
* **`--ffmpeg-threads`**: Allocates CPU cores per transcode task, avoiding CPU exhaustion when running concurrent audio conversions.

---

## Tauri Desktop Application

A modern desktop GUI built with **Tauri v2**, featuring:
* **Liquid Glass Aesthetic**: Translucent frosted panels, custom window frame, and dark mode interface.
* **In-Process Engine Bridge**: Consumes `mkv-core` directly in-process with panic isolation and zero subprocess overhead (ADR-0007).
* **Queue Tracker & Streaming Progress**: Indeterminate preflight shimmer transitioning to live determinate fill and wall-clock execution duration.
* **Live Telemetry & Logs**: Hardware telemetry (CPU, RAM, Disk) and real-time streaming console drawer.

### Pre-built Releases

Download ready-to-run desktop releases directly from [GitHub Releases](https://github.com/sifahaz/mkvanimebatch/releases):

* **Windows**: Standalone `mkv-anime-batch.exe` or portable `MKV_Anime_Batch_Portable_win64.zip`
* **Linux**: Universal `MKV_Anime_Batch_*_amd64.AppImage`, Debian/Ubuntu `.deb`, or Fedora/RHEL `.rpm`

### Building from Source

#### Prerequisites
- [Rust & Cargo](https://rustup.rs/) (Stable 1.80+)
- [Node.js 20+](https://nodejs.org/)
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
│  • Domain Heuristics (10-Tier Audio Scoring, Subtitle Ranks)│
│  • Hybrid EBML Probing (<1ms Native + Fallback Inspector)   │
│  • Transactional Font Registry (BLAKE2b Hash Dedup)         │
│  • Unified Asset Resolver (Deepest Local Precedence)        │
│  • Batch Pipeline & Bounded Concurrency Engine              │
│  • Dual-Channel Streaming (Job Events & Log Channel)       │
└─────────────────────────────────────────────────────────────┘
```

See the architecture decision records in `docs/adr/` for complete architectural rationale:
- [ADR-0005: Rust Engine Migration & Workspace Topology](docs/adr/0005-rust-engine-migration-and-workspace-topology.md)
- [ADR-0006: Hybrid EBML Probing & Channel Streaming](docs/adr/0006-hybrid-ebml-probing-and-channel-streaming.md)
- [ADR-0007: In-Process Engine Bridge & Sidecar Retirement](docs/adr/0007-in-process-engine-bridge.md)
- [ADR-0008: Typed Engine Bridge & Track Assignment Model](docs/adr/0008-typed-engine-bridge-and-track-assignment-model.md)
- [ADR-0009: Streaming Card Progress & Item Duration Model](docs/adr/0009-streaming-card-progress-and-item-duration-model.md)
- [ADR-0010: Mainline Rust Engine Cutover & Python Decommissioning](docs/adr/0010-mainline-rust-cutover-and-python-decommissioning.md)

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

# Run frontend UI contract and state machine tests (Node.js 20+)
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
```

---

## Project Layout

```
mkvanimebatch/
├── Cargo.toml               # Unified Cargo workspace root
├── CONTEXT-MAP.md           # Bounded context map
├── AGENTS.md                # Agent guidelines & CI commit gate rules
├── crates/
│   ├── mkv-core/            # Standalone media engine library crate
│   │   ├── src/             # EBML parsing, domain heuristics, fonts, pipeline
│   │   └── tests/           # 140+ unit and integration tests
│   └── mkv-cli/             # Standalone CLI binary adapter
│       └── src/             # CLI argument parsing, smoke validation
├── desktop-tauri/           # Desktop GUI application
│   ├── ui/                  # Web frontend (Liquid Glass UI, Queue Tracker, tests)
│   └── src-tauri/           # In-process Tauri Engine Bridge & telemetry
├── docs/
│   └── adr/                 # System Architecture Decision Records (0001 - 0010)
└── .github/
    └── workflows/           # Consolidated CI matrix & Release workflows
```

---

## License

This project is open-source software licensed under the [MIT License](LICENSE).
