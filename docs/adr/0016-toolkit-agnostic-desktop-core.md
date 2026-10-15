# 0016. Toolkit-Agnostic Desktop Core Owns UI State

> **Status**: Accepted (not yet implemented)
> **Date**: 2026-10-15
> **Context**: Desktop Application Context
> **Amends**: ADR-0001, ADR-0007, ADR-0008, ADR-0009, ADR-0011, ADR-0013

## Context

Issue #102 aims to replace the WebKitGTK webview with a native Rust toolkit (Slint or iced, still to be spiked). Much of the code in `desktop-tauri/ui` is logic rather than rendering: the queue reducer, the job phases, preflight, request building and display formatting. A native rewrite would have to port all of it. Part of it already exists twice: `reduceQueueEvent` in `queue-state.js` and `SnapshotStore::update` in `snapshot.rs` reduce the same `JobEvent`s with different rules, for example on late events, progress for queued items and the final batch status.

## Decision

Before choosing a toolkit, move all desktop UI state and logic into a new workspace crate, `crates/mkv-desktop-core` (the **Desktop Core**). It depends on `mkv-core` and **not on Tauri**.

1. **The Desktop Core owns the state.** Per mode it holds the Inspection Manifest, the Queue Selection and the Track Assignments. It also holds one Job Snapshot (participants only) and the Job Coordinator's phase. Queue rows are derived by laying the Job Snapshot over the manifest order. The core also runs preflight and builds the `BatchRequest`; a view never builds a request. A view sends user intents and renders what the core publishes.
2. **One reducer.** Where the JS and Rust reducers disagree, Rust's stricter rules win: drop events that arrive after the batch or belong to another job, apply progress only to processing items with finite values, let `ItemStart` rename the item's file, keep an item's status typed (`ItemStatus`, whose terminal values are the Item Outcomes) rather than a string, and settle the batch status as error when items are left open. There are two exceptions. Default outcome reasons become presentation labels, not stored values. A per-item fallback duration, measured on a monotonic clock like Item Duration, is kept for when `duration_sec` is missing. Outcome counts are derived, not counted.
3. **The core derives every displayed value**: status labels and tone tokens, the batch clock, elapsed times, filter counts and the subtitle ladder text. It also runs the Presentation Heartbeat, a 1 Hz tick that runs only while an item is in flight. A view keeps only toolkit geometry, such as sparkline points and ring arcs.
4. **Publication is whole-state and coalesced.** The core marks itself dirty on each change. The Tauri adapter emits the full view state at most about 15 times a second. A native toolkit reads it in-process.
5. **No reload restore.** The core lives as long as the process, so a webview reload fetches the current state. The Job Coordinator's `restoring` phase and `createQueueStateFromSnapshot` are removed.
6. **The engine tags each log line with its kind.** The Log Channel carries typed lines instead of `String`. Each line has a `LogKind` (`error`, `warning`, `skipped`, `succeeded`, `step`, `cache` or `note`, the plain default) and the text. The kind is chosen where the line is written, and the seven kinds map 1:1 to today's console looks (err, warn, skip, ok, info, done, unstyled). The `[ERR]` prefix, which the desktop supervisor adds today (`job.rs`), becomes CLI and copy-log rendering, and `classifyLine`'s substring guessing goes away. Three severity levels were rejected because they would turn the skipped, succeeded, step and note lines plain.
7. **One seconds-to-ms parser.** The CLI's private `parse_seconds_as_ms` (`crates/mkv-cli/src/args.rs`) moves into `mkv-core` and is shared by the CLI and the Desktop Core. The JS copy, `subOffsetSecondsToMs` (`job-controller.js`, which also rounds half away from zero), is deleted.
8. **Tests move with the code.** Each JS logic test is ported to Rust in the PR that moves its logic, and the JS test is deleted there. The Item Settlement golden fixture, `desktop-tauri/ui/fixtures/settlement-events.json`, is kept by the Rust wording test in `snapshot.rs` once its JS consumers are deleted. Node tests cover rendering only.
9. **The core is one actor task.** It owns the state and has a single inbox for Intents, Job Events, heartbeat ticks and publish ticks, so every state change happens in one place. The Engine Bridge moves into the core: `job.rs` (Job Slot, supervisor, two-phase stop), `snapshot.rs` (Job Snapshot and Item Settlement, `settlement_events`), `telemetry.rs` (its sampler takes a Tauri `AppHandle` today), and the `run_job`/`stop_job` logic in `commands.rs` and `process.rs`. `tauri::async_runtime` is replaced by a plain tokio handle, and the supervisor's `JobSink` feeds the actor's inbox.
10. **Two bridge calls.** A view sends a typed `Intent` enum through `dispatch` and reads the state through `get_state`. Native dialogs (`rfd`) and window chrome stay adapter commands; a picked path comes back in as an Intent.
11. **The log is not part of the whole state.** The core keeps the capped log ring, for copy-log and for a reload, but publishes new lines as appended batches of typed lines.
12. **Also in the core**: each mode's settings draft (which is still not persisted across launches), and the telemetry polling cadence. A `ViewVisible` Intent pauses polling while the window is hidden. **Not in the core**: the theme, which is the toolkit's own appearance.

`desktop-tauri` becomes one thin adapter over the core: `#[tauri::command]` wrappers, the `AppHandle` emit, dialogs and window chrome.

## Delivery

The move ships in five slices, sub-issues of #102. Each slice PR moves its JS tests to Rust.

- **S0**: the Log Channel `LogKind` and the shared seconds-to-ms parser, in `mkv-core` and the CLI.
- **S1**: the `mkv-desktop-core` crate, the Engine Bridge move and the reducer rules.
- **S2**: the actor, Intents, publication, derived queue rows and the Inspection Coordinator.
- **S3**: the Job Coordinator, preflight, `BatchRequest` building and the settings drafts.
- **S4**: presentation values, the Presentation Heartbeat, the telemetry cadence and the log ring.

The toolkit spike starts after S1, so it can render real queue state.

## Considered Options

- **Dedupe the reducers only**: *Rejected*: most of the logic would still be ported by hand during the native rewrite, and avoiding that port is the reason to do this first.
- **Keep state in JS and call Rust for pure functions**: *Rejected*: the two reducers would remain, and a native toolkit renders from Rust state anyway.
- **`Arc<Mutex<State>>` instead of an actor**: *Rejected*: a single inbox keeps every state transition in one place.
- **Per-row deltas to the view**: *Rejected*: rows are small, progress is already throttled upstream, and a native view would not need a diff protocol.
- **Rust→WASM in the webview (Leptos/Dioxus-web)**: *Rejected in #102*: it keeps every WebKitGTK problem and adds a bundler.

## Consequences

- The engine stays in-process. It amends these ADRs:
  - **ADR-0001**: decision 4 (the reducer in the frontend `QueueTracker`), the Inspection Reuse addendum (rebuilding Queue Tracker state from a reused manifest) and the 2026-09-28 amendment (the Queue Tracker rejecting a manifest whose `version` is not 2): all now live in the Desktop Core.
  - **ADR-0007**: its command surface (`run_job` in decision 1 and the 2026-10-04 amendment, `stop_job` in the 2026-10-04 amendment): running and stopping become Intents sent through `dispatch`.
  - **ADR-0008**: decision 4 (`get_job_snapshot()` restoring queue presentation across UI reloads): a reload calls `get_state` instead.
  - **ADR-0009**: decisions 2 and 4 and the 2026-09-28 and 2026-10-11 amendments: the Presentation Heartbeat (formerly in the JS `QueueTracker`), its batch clock redraw, the adaptive duration formatting and the reducer's `item_start` duration fallback move into the Desktop Core, and a webview reload calls `get_state` instead of restoring the Job Snapshot from `get_job_snapshot()`.
  - **ADR-0011**: decision 4: queue selection and reordering in the JS `QueueTracker` become Desktop Core state and Intents.
  - **ADR-0013**: Amendment 11 (2026-10-06): after a webview reload the chips show the Effective Settings from `get_state`, not from a restored Job Snapshot's `effective_settings`, and the frontend tests that cover them move to Rust.
- The toolkit ADR (the spike winner, and packaging for deb/rpm/AppImage/msi) follows the spike, per #102, and supersedes ADR-0007 and ADR-0015.
- The glossary's Queue Tracker, Job Coordinator, Presentation Heartbeat and Job Snapshot now describe Desktop Core components, not frontend ones.
- Several known JS-only bugs go away because the behaviour they live in goes away: `release`/`metadata` lost on restore, and unguarded non-finite progress.
- The Log Channel type change touches `mkv-core`, the CLI and the desktop.
