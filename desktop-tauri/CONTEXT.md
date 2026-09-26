# Desktop Application Context

Desktop GUI host built with Tauri v2 providing queue presentation, interactive preflight inspection, and orchestration for anime fansub collections.

## Boundaries & Responsibilities

- **GUI Presentation**: Liquid Glass webview user interface presenting scanning controls, preset configuration, track override modals, terminal log drawers, and real-time episode cards.
- **Queue State Projection**: Client-side state machine (`queue-state.js` & `QueueTracker`) projecting incoming engine `JobEvent` streams into reactive visual queue card states keyed strictly by canonical item IDs.
- **User Settings**: Managing and persisting user preferences (default paths, last-used preset, UI density) and manual track overrides.
- **Hardware Telemetry**: Probing host system resources (CPU core topology, available memory) to compute intelligent default concurrency levels for workers and I/O tasks.
- **Bridge Communication**: In-process integration layer connecting Tauri IPC commands to `mkv-core` via strongly-typed `BatchRequest` contracts, task-isolated asynchronous execution, and dual-channel streaming.

## Domain Invariants

- **Canonical Directory Identity**: Every directory identity crossing the Engine Bridge into the Queue Tracker is the display form of the canonical path — a Windows verbatim (`\\?\`) prefix is stripped, and `\\?\UNC\` is rewritten to `\\`. The Inspection Manifest's `canonical_dir`, the Job Snapshot's `input_dir`, and `JobEvent::BatchStart.input_dir` must all carry that same form, because Queue Tracker directory identity is a string comparison and a mismatch silently drops Queue Selection and Active Batch ordering on the next same-directory rescan. The Media Engine still receives the real `PathBuf` for filesystem work; only the projected form is normalized, in the Engine Bridge.
- **Single Active Job Coordination**: Exactly one batch pipeline execution can be active at any given moment. Desktop execution is guarded by an exclusive `JobSlot` mutex; concurrent execution attempts are immediately rejected before mutating state.
- **Two-Phase Teardown (2.0s cooperative cancellation before abort)**: Stop first signals the `CancellationToken` so workers can clean up. If the inner pipeline task remains active after 2.0s, its abort handle stops that task; the Engine Bridge supervisor still settles events and the Job Snapshot, emits `job-done`, and releases the job slot (ADR-0007).
- **In-Memory Snapshot Projection Across Reloads**: `JobState` in the Engine Bridge reduces incoming `JobEvent` streams into an in-memory `JobSnapshot`. When the frontend webview reloads or navigates, `get_job_snapshot()` immediately re-hydrates the active or last-completed queue state, progress bars, and authoritative item durations without disk re-probing or engine restarts (ADR-0008).
- **Best-Effort Incremental Card Rendering (~8ms)**: Initial Inspection Manifest cards are created in `requestAnimationFrame` chunks with a best-effort ~8ms deadline. Live progress events render synchronously on mounted cards; cards not yet mounted render from current reducer state when created. Log batches flush in one animation frame; neither has a hard per-update frame budget or a guaranteed frame rate.
- **Mode-Specific Font Handling**: Extract mode retains extracted fonts as user output, while remux mode embeds discovered fonts into output Matroska files. The inert `keep_fonts` configuration flag has been completely removed across desktop UI controls, presets, and typed IPC `BatchRequest` structures.

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
| **Job Snapshot** | The in-memory projection of one active or last-completed Active Batch, including its participating items and outcomes across UI reloads; it does not include unselected Inspection Manifest items. | Job state, queue cache, execution status |
| **Card Progress** | The visual representation of an active episode's execution within the Queue Tracker, presenting an indeterminate preflight shimmer sweep during startup and audio transcoding, transitioning to a determinate percentage fill upon receiving streamed item progress. | Loading bar, queue spinner, progress gauge |
| **Item Duration** | The authoritative wall-clock execution duration in seconds measured from item start to completion and rendered on terminal episode cards (`Done in Xs`). | Run time, execution clock, stop watch |
| **Queue Selection** | The interactive subset and custom ordering of scanned manifest items marked by the user for execution. | Checked items, run list |
| **Active Batch** | The ordered selected items submitted for one execution, excluding unselected items from the Inspection Manifest. | Entire scanned queue |
