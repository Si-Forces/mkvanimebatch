# Context Map

## Contexts

- [Media Engine](./crates/mkv-core/CONTEXT.md) — parses, indexes, scores, probes, and executes anime fansub batch processing
- [Desktop Application](./desktop-tauri/CONTEXT.md) — manages GUI state, queue progression, user configuration, and desktop presentation; its Desktop Core is the toolkit-agnostic `crates/mkv-desktop-core` (it owns each mode's queue, Settings Draft and Run Log, the Intents, the Job Coordinator, the Inspection Coordinator, the telemetry cadence and the Engine Bridge so far, and grows slice by slice), and `desktop-tauri` is one view over it ([ADR-0016](./docs/adr/0016-toolkit-agnostic-desktop-core.md))

## Relationships

- **Desktop Application → Media Engine**: Desktop Application imports `mkv-core` as an in-process library; sends `BatchRequest` (with optional `TrackAssignment` entities) and preset queries (`PresetProfile`), and consumes domain types (`InspectionManifest`, `BatchItemInfo`, `ItemOutcome`, `JobEvent`) and dual streaming channels (Job Event & Log Channel). Desktop Application owns and projects `JobSnapshot` from the event stream, and its Engine Bridge also authors an `ItemComplete` for each item the Batch Pipeline left open when a job ends (Item Settlement).
