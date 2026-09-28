# 0005. Rust Media Engine Migration and Workspace Topology

> **Status**: Accepted (Completed in ADR-0010; amended 2026-09-28)
> **Date**: 2026-04-01
> **Context**: System Workspace Topology

## Context
The core fansub batch processing engine (`mkv_*.py`) was implemented as a pure Python standard-library suite executed as PyInstaller sidecars from the Tauri desktop app and run via standalone CLI scripts. PyInstaller sidecar compilation added build friction across platforms, process spawn and NDJSON stream overhead in the GUI, and GIL-constrained concurrency.

## Decision
We decided to migrate the Python engine to Rust within a unified root Cargo workspace:
1. **Context Topology**: Split into two distinct Bounded Contexts documented in `CONTEXT-MAP.md`: `Media Engine Context` (core domain, media probing, asset resolution, track scoring, font deduplication, remux pipeline) and `Desktop Application Context` (Tauri GUI, queue state machine, window management).
2. **Crate Layout**:
   - `crates/mkv-core`: A standalone domain and pipeline library crate.
   - `crates/mkv-cli`: A CLI adapter wrapping `mkv-core` for standalone terminal users and headless automation.
   - `desktop-tauri/src-tauri`: Consumes `mkv-core` directly in-process via worker tasks and MPSC channels, eliminating sidecar compilation and NDJSON overhead while retaining CLI compatibility.
3. **Hybrid Media Processing**: Use native Rust EBML/Matroska parsing for fast, read-only preflight inspection and manifest generation, while continuing to orchestrate external `mkvmerge`, `mkvextract`, and `ffmpeg` CLI tools for writing/remuxing to guarantee fansub subtitle, font, and container integrity.
4. **Phased Migration**: Port components in dependency order (`mkv_domain` + `mkv_fonts` → `mkv_scan` + `mkv_assets` → pipeline and CLI) while keeping Python test suites active until parity is proven.

## Consequences
- PyInstaller sidecar build steps (`build-sidecars.sh` / `build-sidecars.ps1`) will be eliminated once migration completes.
- In-process execution couples desktop GUI stability to engine stability; memory leaks or unexpected panics in `mkv-core` could crash the desktop process. `mkv-core` must be panic-free and use strict `Result` propagation.
- External MKVToolNix and FFmpeg binaries remain hard runtime requirements for remuxing and transcoding.

## Amendment — 2026-09-28

- **Sidecar build steps are gone.** The migration completed (ADR-0010); `build-sidecars.*` no longer exist and the desktop host consumes `mkv-core` in-process.
- **Panic containment, not panic freedom.** `mkv-core` still propagates errors as `Result`, but the runtime no longer relies on it never panicking: a panicking worker task completes its item as `error` while the batch continues, and a panic escaping the pipeline is caught at the desktop bridge's join boundary (ADR-0007). Both require the release profile to unwind; `[profile.release]` must never set `panic = "abort"`.
