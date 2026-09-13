# Desktop Application

Desktop GUI host built with Tauri v2 providing queue presentation, interactive preflight inspection, and orchestration for anime fansub collections.

## Language

**Queue Tracker**:
The frontend state machine managing batch item lifecycles (`queued` → `processing` → `completed` / `skipped` / `planned` / `error`) strictly keyed by item ID.
_Avoid_: Task list, item manager

**Engine Bridge**:
The in-process Rust integration layer connecting Tauri commands and channels to the Media Engine's Batch Pipeline with task-level panic isolation, dual-channel event/log streaming, and cooperative cancellation.
_Avoid_: Wrapper, helper, controller

**Sidecar Contract**:
_Deprecated_: The legacy CLI subprocess invocation and event streaming interface, superseded by the in-process Engine Bridge.
_Avoid_: IPC, subprocess pipe

**Inspection Manifest**:
The structured document consumed by the UI to hydrate the queue and display candidate track selections before batch execution.
_Avoid_: Execution plan, dry run report

**Preset Profile**:
A named audio and pipeline configuration queried from the Media Engine and applied in the desktop GUI.
_Avoid_: Config preset, setting template

**Job Snapshot**:
The in-memory projection maintained in real-time by the Engine Bridge from Job Events, summarizing active or last-completed batch execution, manifest items, and terminal outcomes across UI reloads.
_Avoid_: Job state, queue cache, execution status

**Card Progress**:
The visual representation of an active episode's execution within the Queue Tracker, presenting an indeterminate preflight shimmer sweep during startup and audio transcoding, transitioning to a determinate percentage fill upon receiving streamed item progress.
_Avoid_: Loading bar, queue spinner, progress gauge

**Item Duration**:
The authoritative wall-clock execution duration in seconds measured from item start to completion and rendered on terminal episode cards (`Done in Xs`).
_Avoid_: Run time, execution clock, stop watch
