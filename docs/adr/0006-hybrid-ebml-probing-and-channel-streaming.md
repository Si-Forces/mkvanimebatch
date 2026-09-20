# 0006. Hybrid EBML Probing and Channel-First Event Streaming

> **Status**: Accepted
> **Date**: 2026-04-15
> **Context**: Media Engine Context

## Context
During preflight inspection, probing anime fansub MKVs using `mkvmerge -J` subprocesses can be slow across large release directories (50+ episodes). However, anime fansub files frequently contain non-standard container quirks, custom attachments, and variable font MIME types that fragile native parsers might mishandle. In addition, Tauri v2 needs low-overhead progress updates without string serializing and deserializing over stdout.

## Decision
1. **Hybrid EBML Probing**: `mkv-core` implements native EBML/Matroska parsing for fast preflight inspection of Segment Headers, Tracks, and Attachments. If the native parser encounters unsupported container features, malformed elements, or unhandled errors, it automatically falls back to invoking `mkvmerge -J`.
2. **Channel-First Event Streaming**: The batch pipeline communicates progress via strongly-typed Tokio MPSC channels (`tokio::sync::mpsc::Receiver<JobEvent>`).
   - The Desktop Application (`desktop-tauri`) receives typed `JobEvent` structs directly and emits them to the frontend webview via Tauri events.
   - The CLI adapter (`mkv-cli`) consumes the same receiver and formats NDJSON strings with the `@event:` prefix to stdout.
3. **Cancellation via CancellationToken**: Batch and item cancellation are coordinated through `tokio_util::sync::CancellationToken`. External `mkvmerge` and `ffmpeg` processes are launched with `kill_on_drop(true)` to ensure immediate cleanup when a task is cancelled or dropped.

## Consequences
- Preflight inspection achieves high performance for standard MKVs while retaining 100% compatibility for edge-case releases.
- In-process event handling avoids JSON string formatting overhead in the GUI.
- MKVToolNix remains required at runtime as both a fallback inspector and the primary remuxer.
