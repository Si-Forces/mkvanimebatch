# 0001. Sidecar Contract and Structured Planning

## Context
The Tauri desktop application previously duplicated directory scanning, `mkvmerge -J` probing, and metadata parsing in Rust (`commands.rs`), while relying on freeform regex matching of stdout logs for progress tracking. This caused severe logic duplication, disk I/O double-passes, parameter range drift, and fragile progress state machines.

## Decision
We establish a versioned Sidecar Contract between the desktop UI and Python engine:
1. Python provides an inspection manifest interface via `mkv-remux --plan --input-dir <dir>` returning a versioned JSON document (`{"version": 1, "items": [{"id": <relative_posix_path>, "name": <basename>, "metadata": {...}}]}`). Failed/unknown metadata fields default to `null`. Rust validates `version == 1` and acts as a pass-through adapter without probing files directly.
2. When executed with the explicit `--events` flag, Python emits versioned, machine-readable `@event: {"version": 1, "type": ...}` JSON-lines on stdout for the entire execution lifecycle (`batch_start`, `item_start`, `item_complete`, `batch_complete`). Each item event carries its canonical root-relative POSIX item `id`.
3. The desktop adapter (`commands.rs`) strips the `@event:` prefix and emits `job-event` payloads to the frontend, while passing human-readable lines as `job-log`.
4. The frontend `QueueTracker` consumes `job-event` via a pure reducer in `queue-state.js` indexed strictly by item ID, eliminating fuzzy matching, card invention, regex log parsing, and optimistic completion heuristics.

## Addendum: GUI Manual Subtitle Selection (additive, still v1)
Manual subtitle assignment extends the contract without a version bump:
1. **Candidates out**: manifest items may carry an additive optional field `"sub_candidates": [<relative_posix_path>, ...]` listing every external subtitle discovered for that item across its ancestor chain. The field is omitted when empty; consumers must treat it as optional.
2. **Assignments in**: the frontend sends manual selections as a JSON document (`{"version": 1, "assignments": {<item_id>: <subtitle_path>}}`). Rust persists this payload verbatim to a temp file and appends `--sub-assignments <path>` to the sidecar invocation — the adapter never interprets the payload.
3. **Precedence**: an assigned subtitle overrides every automatic source (local index match, embedded track, fallback assets). A stale path at run time falls back to automatic matching rather than failing the job.
4. Scope: candidates cover the input-root ancestor chain only; `--assets-dir` fallback assets remain automatic-only and are not offered as GUI picks.

## Addendum: Cooperative Cancellation (additive, still v1)
Graceful shutdown extends the event contract without a version bump:
1. **New item outcome**: `item_complete` may carry additive optional `status: "cancelled"` for jobs terminated by a cancellation request — either never started (queued drain) or interrupted mid-run (child process terminated). Consumers must treat it as a terminal status distinct from both `skipped` (planning-time decision) and `error` (job failure).
2. **Batch flag**: `batch_complete` may carry an additive optional boolean field `cancelled`. When `true`, the run ended because cancellation was requested; `success` is `false` in that case and the sidecar exits with code 130.
3. **Mechanics**: the engine routes SIGINT/SIGTERM into a shared cancel token; worker threads observe it between stages and long-running children (`mkvmerge`, `mkvextract`, `ffmpeg`) are terminated with a kill escalation after a short grace period. Temp scratch directories are removed by existing `finally` paths before exit.
4. **Adapter duty**: the desktop stop path must signal before killing — on Unix it sends SIGTERM to the sidecar process group and escalates to SIGKILL only after a grace period; on Windows `taskkill /T /F` remains a hard stop (no catchable signal), so cancelled-status events are best-effort there.

