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
* Offers both standard single-file routines and parallel high-throughput implementations (`mkv_extract_fast.py` and `mkv_remux_fast.py`) backed by thread pools.

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
# High Performance Parallel Extractor
python mkv_extract_fast.py --input-dir "D:/Anime/Series" --workers 8

# With Subtitle Timestamp Shift (+5 seconds)
python mkv_extract_fast.py --input-dir "D:/Anime/Series" --sub-offset 5.0

# Standard Extractor
python mkv_extract.py --input-dir "D:/Anime/Series"
```

### 2. Batch Remuxing

Re-mux anime episodes with extracted subtitles, external chapter XMLs, covers, and attached fonts:

```bash
# Parallel Remuxer
python mkv_remux_fast.py --input-dir "D:/Anime/Series" --out-dir "D:/Anime/Remuxed" --workers 4

# Dry-run mode (Inspect without modifying files)
python mkv_remux_fast.py --input-dir "D:/Anime/Series" --dry-run
```

When `--out-dir` is used, nested source folders are preserved beneath the
output directory so episodes from different seasons cannot overwrite each other.

### 3. Windows Helper Scripts

Quick-start scripts are included in the repository for convenience:

* `run_fast.bat` / `run_fast.ps1` - Runs the fast extraction and remuxing pipeline interactive menu.
* `run.bat` / `run.ps1` - Runs the standard extraction and remuxing pipeline interactive menu.

---

## 🧪 Running Unit Tests

The test suite includes 220+ automated unit tests covering filename parsing, subtitle shifting, directory traversal, font registry deduplication, and track selection scoring.

Run the tests using `pytest`:

```bash
python -m pytest tests/ -v
```

---

## 📄 License

This project is open-source and available under the MIT License.
