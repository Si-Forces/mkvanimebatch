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
A saved configuration bundle of audio transcoding bitrates, complexity settings, and naming options selected by the user.
_Avoid_: Config preset, setting template
