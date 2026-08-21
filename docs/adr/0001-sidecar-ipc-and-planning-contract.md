# 0001. Sidecar Contract and Structured Planning

## Context
The Tauri desktop application previously duplicated directory scanning, `mkvmerge -J` probing, and metadata parsing in Rust (`commands.rs`), while relying on freeform regex matching of stdout logs for progress tracking. This caused severe logic duplication, disk I/O double-passes, parameter range drift, and fragile progress state machines.

## Decision
We establish a versioned Sidecar Contract between the desktop UI and Python engine:
1. Python provides an inspection manifest interface via `mkv-remux --plan --input-dir <dir>` returning a versioned JSON document (`{"version": 1, "items": [{"id": <relative_posix_path>, "name": <basename>, "metadata": {...}}]}`). Failed/unknown metadata fields default to `null`. Rust validates `version == 1` and acts as a pass-through adapter without probing files directly.
2. When executed with the explicit `--events` flag, Python emits versioned, machine-readable `@event: {"version": 1, "type": ...}` JSON-lines on stdout for the entire execution lifecycle (`batch_start`, `item_start`, `item_complete`, `batch_complete`). Each item event carries its canonical root-relative POSIX item `id`.
3. The desktop adapter (`commands.rs`) strips the `@event:` prefix and emits `job-event` payloads to the frontend, while passing human-readable lines as `job-log`.
4. The frontend `QueueTracker` consumes `job-event` via a pure reducer in `queue-state.js` indexed strictly by item ID, eliminating fuzzy matching, card invention, regex log parsing, and optimistic completion heuristics.

