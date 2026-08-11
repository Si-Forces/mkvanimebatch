# MKV Anime Batch Tool (`mkvanimebatch`)

An automated, high-performance batch processing suite for anime and fansub MKV collections. `mkvanimebatch` provides intelligent subtitle track selection, font extraction & deduplication, audio selection scoring, subtitle timestamp shifting, and multi-threaded remuxing.

---

## 🔥 Key Features

### 🎬 Intelligent Subtitle Selection & Extraction
* **Language Priority**: Automatically detects and scores subtitle tracks, prioritizing Indonesian (`ind`, `id`) and fallback English (`eng`, `en`).
* **Format & Track Type Scoring**: Prefers Advanced SubStation Alpha (`.ass` typesetting) over SubRip (`.srt`), prioritizing dialogue tracks over signs/titles or commentaries.
* **Format-Variant Coexistence**: Supports evaluating multiple subtitle formats (`.ass` vs `.srt`) for the same episode without prematurely discarding candidates.

### 🔊 10-Tier Audio Picker
* **Dialogue Priority**: Automatically scores audio tracks, favoring Japanese stereo and multi-channel audio tracks.
* **Audio Commentary Exclusion**: Isolates commentary tracks into a secondary fallback pool, ensuring voice-actor/director commentaries never override film dialogue.

### ⏱️ Subtitle Offset Shifting & Preservation
* **Offset-Encoded Filenames**: Encodes timestamp offsets into output filenames (e.g. `episode_01.ind.[+5s].ass`) to allow instant skip checks on automated runs.
* **BOM Preservation**: Preserves UTF-8 BOM byte-order marks (`utf-8-sig`) when shifting timestamps.

### 🛡️ Atomic Operations & Safe Recovery
* **Crash-Resistant Output**: Performs all remuxing and extraction into isolated temporary job directories, promoting completed files via atomic move operations only upon successful exit codes. Partial or interrupted outputs from power failures are automatically quarantined and discarded.

### 🚀 Standard & Fast Concurrency Engine
* Offers both standard single-file routines and parallel high-throughput implementations (`mkv_extract.py` and `mkv_remux.py`) backed by thread pools and bounded disk I/O jobs.
* **Persistent Inspect Cache**: Caches `mkvmerge` JSON probe results to `.mkv_inspect_cache.json` across runs, dramatically reducing repeated disk reads on large collections.

---

## 🛠️ System Requirements

Make sure the following software executables are installed and available in your system `PATH`:

1. **Python 3.10+**
2. **MKVToolNix** (`mkvmerge`, `mkvextract`)
3. **ffmpeg** (Required for Opus audio transcoding)

---

## 🚀 Quick Start & Usage

### 1. Extract Subtitles & Fonts

Extract the best matching subtitle, external cover, chapters, and deduplicated font attachments from all MKVs in a directory:

```bash
# High Performance Parallel Extractor (with custom Output Dir and Fonts Dir)
python mkv_extract.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Extracted" --fonts-dir "D:/Anime/Fonts" --workers 8 --io-jobs 2

# With Subtitle Timestamp Shift (+5 seconds)
python mkv_extract.py --input-dir "D:/Anime/Series" --sub-offset 5.0

# Standard Extractor
python mkv_extract.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Extracted"
```

### 2. Batch Remuxing

Re-mux anime episodes with extracted subtitles, external chapter XMLs, covers, and attached fonts:

```bash
# Parallel Remuxer with custom Opus bitrate, complexity, and font dir
python mkv_remux.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Remuxed" --fonts-dir "D:/Fonts" --bitrate 192k --complexity 10 --workers 4 --io-jobs 2

# Keep temporary font folder after remux
python mkv_remux.py --input-dir "D:/Anime/Series" --keep-fonts

# Dry-run mode (Inspect without modifying files)
python mkv_remux.py --input-dir "D:/Anime/Series" --dry-run
```

When `--out-dir` is used, nested source folders are preserved beneath the
output directory so episodes from different seasons cannot overwrite each other.

### 3. CLI Options Reference

| Option | Description | Default |
| :--- | :--- | :--- |
| `--input-dir` | Input directory containing MKVs / assets | Current directory |
| `--out-dir` | Output directory for remuxed or extracted files | Input directory (Remux: `<input>/Output`) |
| `--fonts-dir` | Explicit font directory override | Auto-detected |
| `--assets-dir` | Fallback directory to search for subtitles/chapters/covers/fonts when missing locally | `None` |
| `--workers` | Maximum parallel worker threads | CPU count |
| `--io-jobs` | Maximum concurrent disk I/O tasks | `2` |
| `--complexity` | Opus audio transcoding complexity (`0`–`10`) | `10` |
| `--bitrate` | Opus audio bitrate (e.g. `128k`, `192k`) | `192k` |
| `--sub-offset` | Subtitle timestamp shift offset in seconds | `0.0` |
| `--keep-fonts` | Keep temporary extracted fonts directory | `False` |
| `--dry-run` | Preview actions without writing files | `False` |
| `--capture-stdout` | Capture subprocess stdout for debugging | `False` |

### 4. Interactive Windows Launchers

Interactive quick-start scripts are included for Windows users:

* `run.bat` / `run.ps1` - High-performance parallel pipeline launcher with toggles for Dry Run, Keep Fonts, I/O Jobs, Opus Complexity, and Fonts Directory overrides.

All batch launchers include non-interactive EOF loop guards to safely handle redirected input without hanging.

### 5. Standalone Tauri Desktop App (`desktop-tauri/`)

A native desktop GUI built with **Tauri v2** featuring an Apple **Liquid Glass** UI aesthetic (frosted glass panels, translucent dark/light mode, traffic light window controls, and live stdout/stderr log streaming).

> 🎨 Alternative UI design explorations live in [`docs/prototypes/`](docs/prototypes/) (open `docs/prototypes/index.html` in a browser to browse them). They are dev artifacts and are not shipped in the built app.

#### Building locally:
- **Windows**:
  ```powershell
  powershell -ExecutionPolicy Bypass -File .\desktop-tauri\build-sidecars.ps1
  cd desktop-tauri\src-tauri
  cargo tauri build
  ```
- **Linux (Ubuntu / Debian / Mint)**:
  ```bash
  sudo apt update
  sudo apt install -y build-essential curl wget file libssl-dev libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf python3-pip
  pip3 install pyinstaller --break-system-packages
  cargo install tauri-cli --version "^2"
  export PATH="$HOME/.cargo/bin:$PATH"
  bash desktop-tauri/build-sidecars.sh
  cd desktop-tauri/src-tauri && cargo tauri build
  ```

---

## 🧪 Running Unit Tests

The test suite includes **300+ automated unit tests** covering filename parsing, subtitle shifting, directory traversal, font registry deduplication, multi-threaded cache safety, track selection scoring, and CLI validation.

Run the tests using `pytest`:

```bash
python -m pytest tests/ --tb=short
```

---

## 📄 License

This project is open-source and available under the MIT License.
