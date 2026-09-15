# 0009. Streaming Card Progress, Centralized Heartbeat, and Authoritative Item Duration Model

## Context
Following the implementation of the typed engine bridge (ADR-0008) and asynchronous event streaming (ADR-0006, ADR-0007), the desktop interface received real-time batch execution events. However, the queue card presentation suffered from visual and state inconsistencies:
1. **Persistent Indeterminate Shimmer**: Queue cards retained an indeterminate pulsing shimmer even after ffmpeg progress was actively reporting, failing to reflect granular track muxing completion.
2. **Missing Live Elapsed Presentation**: In-flight items lacked a real-time ticking clock showing current elapsed run duration, making long transcodes appear stalled.
3. **Fragile Duration Persistence**: Terminal item runtimes were inferred opportunistically in client JavaScript, causing completed item durations to disappear whenever the UI was reloaded or re-hydrated from `get_job_snapshot()`.

## Decision
We decided to adopt a unified, authoritative progress and duration architecture across the Media Engine and Desktop Application contexts:

1. **Hybrid Visual Progress Lifecycle**:
   - Items start in an indeterminate shimmering state during startup, EBML probe, and audio transcoding phases where exact stream percentages are unavailable.
   - Upon receiving the first `JobEvent::ItemProgress` event from the engine, the card transitions immediately from indeterminate shimmer to a determinate progress bar whose width percentage is driven by incoming stream progress.
   - Upon reaching terminal state (`JobEvent::ItemComplete`), the progress bar settles to 100% (or error/skipped/cancelled styling) and freezes.

2. **Centralized Presentation Heartbeat**:
   - Rather than attaching individual `setInterval` timers to every card DOM element, a single centralized heartbeat timer is maintained in `QueueTracker`.
   - The heartbeat ticks at fixed intervals (e.g. 1000ms), updating elapsed runtime displays for all currently active (`processing`) cards in a single coordinated render pass.
   - When no items are in flight, the heartbeat timer halts to avoid idle CPU cycles and battery drain.

3. **Authoritative Engine-Owned Item Duration Contract**:
   - `mkv-core` assumes exclusive ownership over measuring item execution duration.
   - In both extraction and remux worker tasks, the engine captures a monotonic start timestamp (`Instant::now()`) immediately upon task entry and calculates elapsed seconds (`start_instant.elapsed().as_secs_f64()`) upon task termination (completion, cancellation, or error).
   - `JobEvent::ItemComplete` exposes a strongly-typed `duration_sec: Option<f64>` field containing this measured duration (or `None` for planned/skipped items that never ran).
   - Desktop bridge snapshot reduction (`update_snapshot`) extracts `duration_sec` directly from `ItemComplete` into `ItemSnapshot`, ensuring reloaded snapshots preserve accurate terminal runtimes.

4. **Adaptive Humanized Duration Formatting**:
   - Durations are rendered using adaptive formatting thresholds:
     - Sub-decisecond runs: `<0.1s` (e.g., instant cache hits or skipped probe operations).
     - Short runs (<60s): `Xs` (e.g., `4.2s`, `42.0s` for terminal outcomes; `4s`, `42s` for in-flight ticker).
     - Extended runs (>=60s): `Xm Ys` (e.g., `1m 15.4s` for terminal outcomes; `2m 14s` for in-flight ticker).

## Considered Options
- **Decentralized Per-Card JavaScript Timers**: *Rejected*: Creating independent timer intervals per item causes DOM thrashing, uncoordinated redraws, and timer drift across concurrent tasks.
- **Engine-Emitted Tick Events Across IPC**: *Rejected*: Emitting synthetic 1Hz heartbeat events from the media engine would congest the IPC channel and tightly couple backend execution to frontend display frame rates.
- **Client-Side Duration Calculation**: *Rejected*: Measuring duration solely in the webview causes inaccuracies when the window is minimized or throttled by the OS webview scheduler, and completely loses durations when the page is reloaded.

## Consequences
- Guarantees exact, drift-free execution timing for all batch operations across engine restarts and UI reloads.
- Eliminates timer memory leaks and reduces webview CPU overhead during batch processing.
- Preserves full backward-compatible serialization on `JobEvent::ItemComplete` using `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Ensures queue item snapshots reflect authoritative wall-clock duration without fragile JSON metadata side-channels.
