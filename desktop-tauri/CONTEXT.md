# Desktop Application Context

Desktop GUI host built with Tauri v2 providing queue presentation, interactive preflight inspection, and orchestration for anime fansub collections.

## Boundaries & Responsibilities

- **GUI Presentation**: Liquid Glass webview user interface presenting scanning controls, preset configuration, track override modals, terminal log drawers, and real-time episode cards.
- **Queue State Projection**: Client-side state machine (`queue-state.js` & `QueueTracker`) projecting incoming engine `JobEvent` streams into reactive visual queue card states keyed strictly by canonical item IDs.
- **User Settings**: Managing and persisting user preferences (default paths, last-used preset, UI density) and manual track overrides.
- **Hardware Telemetry**: Probing host system resources (CPU core topology, available memory) to compute intelligent default concurrency levels for workers and I/O tasks.
- **Bridge Communication**: In-process integration layer connecting Tauri IPC commands to `mkv-core` via strongly-typed `BatchRequest` contracts, task-isolated asynchronous execution, and dual-channel streaming.

## Domain Invariants

- **Single Active Job Coordination**: Exactly one batch pipeline execution can be active at any given moment. Desktop execution is guarded by an exclusive `JobSlot` mutex; concurrent execution attempts are immediately rejected before mutating state.
- **Two-Phase Teardown (2.0s cooperative cancellation before abort)**: When a batch job is cancelled or the desktop window closes during active execution, cooperative cancellation is signaled first via `CancellationToken`. Worker tasks have a grace period of up to 2.0 seconds to defuse font claims, clean temporary scratch directories, and flush caches. If workers remain active after 2.0s, hard task abortion (`JoinHandle::abort`) is executed as a safety valve (ADR-0007).
- **In-Memory Snapshot Projection Across Reloads**: `JobState` in the Engine Bridge reduces incoming `JobEvent` streams into an in-memory `JobSnapshot`. When the frontend webview reloads or navigates, `get_job_snapshot()` immediately re-hydrates the active or last-completed queue state, progress bars, and authoritative item durations without disk re-probing or engine restarts (ADR-0008).
- **Non-Blocking Frame Budgeted DOM Rendering (8ms)**: All visual updates to the Queue Tracker, progress bars, and terminal drawer logs are constrained to an 8ms frame execution budget using `requestAnimationFrame`. High-frequency log bursts or rapid event sequences are batched and scheduled, preventing UI thread blockage and preserving 60fps animations.

## Core Desktop Components

- **`Job Coordinator`**: Desktop application coordinator managing execution lifecycle, acquiring exclusive `JobSlot` locks, transforming GUI input into typed `BatchRequest` structures, spawning in-process tasks, and orchestrating two-phase teardown.
- **`Inspection Coordinator`**: Desktop component managing folder selection, triggering preflight directory scans via `mkv-core` inspection APIs, and hydrating the `QueueTracker` with the resulting `InspectionManifest`.
- **`Presentation Heartbeat`**: Centralized 1000ms timer in `QueueTracker` coordinating real-time elapsed duration updates across all currently active processing cards in a single frame, pausing when no items are in flight.
- **`System Telemetry Provider`**: Host resource probe assessing physical CPU cores and system architecture to generate recommended default worker and I/O concurrency limits, decoupled from quality presets.

## Ubiquitous Language & Explicit Prohibitions

| Canonical Domain Term | Definition | Prohibited Synonyms (Explicitly Avoid) |
| :--- | :--- | :--- |
| **Queue Tracker** | The frontend state machine managing batch item lifecycles (`queued` → `processing` → `completed` / `skipped` / `planned` / `error` / `cancelled`) strictly keyed by item ID. | Task list, item manager |
| **Engine Bridge** | The in-process Rust integration layer connecting Tauri commands and channels to the Media Engine's Batch Pipeline with task-level panic isolation, dual-channel event/log streaming, and cooperative cancellation. | Wrapper, helper, controller |
| **Job Coordinator** | Desktop application coordinator managing execution lifecycle, acquiring exclusive `JobSlot` locks, transforming GUI input into typed `BatchRequest` structures, spawning in-process tasks, and orchestrating two-phase teardown. | Job runner, execution manager |
| **Inspection Coordinator** | Desktop component managing folder selection, triggering preflight directory scans via `mkv-core` inspection APIs, and hydrating the `QueueTracker` with the resulting `InspectionManifest`. | Scan manager, folder inspector |
| **Presentation Heartbeat** | Centralized 1000ms timer in `QueueTracker` coordinating real-time elapsed duration updates across all currently active processing cards in a single frame, pausing when no items are in flight. | UI ticker, card timer, interval loop |
| **System Telemetry Provider** | Host resource probe assessing physical CPU cores and system architecture to generate recommended default worker and I/O concurrency limits, decoupled from quality presets. | Hardware probe, system monitor |
| **Sidecar Contract** | _Deprecated_: The legacy CLI subprocess invocation and event streaming interface, superseded by the in-process Engine Bridge. | IPC, subprocess pipe |
| **Inspection Manifest** | The structured document consumed by the UI to hydrate the queue and display candidate track selections before batch execution. | Execution plan, dry run report |
| **Preset Profile** | A named audio and pipeline configuration queried from the Media Engine and applied in the desktop GUI. | Config preset, setting template |
| **Job Snapshot** | The in-memory projection maintained in real-time by the Engine Bridge from Job Events, summarizing active or last-completed batch execution, manifest items, and terminal outcomes (`completed`, `skipped`, `planned`, `error`, `cancelled`) across UI reloads. | Job state, queue cache, execution status |
| **Card Progress** | The visual representation of an active episode's execution within the Queue Tracker, presenting an indeterminate preflight shimmer sweep during startup and audio transcoding, transitioning to a determinate percentage fill upon receiving streamed item progress. | Loading bar, queue spinner, progress gauge |
| **Item Duration** | The authoritative wall-clock execution duration in seconds measured from item start to completion and rendered on terminal episode cards (`Done in Xs`). | Run time, execution clock, stop watch |
| **Queue Selection** | The interactive subset and custom ordering of scanned manifest items marked by the user for active execution within the Queue Tracker. | Checked items, run list, active batch |
