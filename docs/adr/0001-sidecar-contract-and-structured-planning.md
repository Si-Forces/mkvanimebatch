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
5. **Queued cancellation has no start event**: a worker that observes cancellation before beginning processing emits `item_complete(status="cancelled")` without a preceding `item_start`. This preserves `startTime = null` and prevents consumers from displaying a fabricated processing duration. A worker interrupted after it begins processing retains the normal `item_start` then `item_complete(status="cancelled")` sequence.
6. **Terminal outcomes are unique**: the engine emits at most one `item_complete` for each item ID. Batch-level cancellation drains unresolved items as `cancelled` without contradicting outcomes already emitted as completed, skipped, planned, or error.

## Addendum: Desktop Operation Ownership
The desktop adapter owns the lifecycle of both Inspection Manifest generation and batch execution:
1. **Managed inspection**: `inspect_directory` starts `mkv-remux --plan` in an owned process group or process tree. A superseding inspection, Stop, window Close, or application Exit terminates that planner and its descendants. Tracking inspection is required lifecycle behavior, not a second implementation of probing or asset resolution in Rust.
2. **Single operation**: inspection startup is serialized, and the frontend coordinates remux and extraction through one global operation state. Conflicting run, cache, path, and assignment controls remain disabled while an operation is starting, running, or stopping.
3. **ID-bound execution**: `run_job` returns an adapter-generated job ID. Structured events and completion notifications carry that ID, and Stop supplies it back to the adapter. A stale Stop request or same-mode event must not target a newer job.
4. **Stopping state**: once stopping begins, the backend slot remains reserved until the owned process is reaped and temporary assignment data is removed. A new job cannot replace a running or stopping slot.
5. **Close and exit**: closing the custom window or exiting the application first performs bounded cleanup of any owned inspection and execution process trees. Cleanup failure prevents normal exit rather than knowingly orphaning media tools.
6. **Forced-stop presentation**: Windows hard termination and Unix escalation may prevent the engine from emitting its final structured events. When the frontend initiated Stop, unresolved items are finalized as cancelled and the batch is presented as Cancelled rather than Failed.

## Addendum: Inspection Reuse and Telemetry Boundary
1. **Inspection deduplication**: concurrent requests for the same path share one in-flight inspection. Reusing a completed manifest for an unchanged path must rebuild Queue Tracker state from that manifest so terminal state from a prior run is not retained. Starting a job performs a fresh inspection to account for files added, removed, or renamed since the previous browse scan.
2. **Snapshot consistency**: execution arguments and manual subtitle assignments are snapshotted before asynchronous preflight work. Form edits and assignment controls are locked while that snapshot is pending.
3. **Telemetry is outside the Sidecar Contract**: system CPU, memory, and disk-rate telemetry is desktop-only presentation data. It does not change Inspection Manifest fields, structured event vocabulary, asset resolution, or engine behavior, and should not be treated as a Sidecar Contract extension during review.
