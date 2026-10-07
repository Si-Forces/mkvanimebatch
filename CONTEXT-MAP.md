# Context Map

## Contexts

- [Media Engine](./crates/mkv-core/CONTEXT.md) — parses, indexes, scores, probes, and executes anime fansub batch processing
- [Desktop Application](./desktop-tauri/CONTEXT.md) — manages GUI state, queue progression, user configuration, and desktop presentation

## Relationships

- **Desktop Application → Media Engine**: Desktop Application imports `mkv-core` as an in-process library; sends `BatchRequest` (with optional `TrackAssignment` entities) and preset queries (`PresetProfile`), and consumes domain types (`InspectionManifest`, `BatchItemInfo`, `ItemOutcome`, `JobEvent`) and dual streaming channels (Job Event & Log Channel). Desktop Application owns and projects `JobSnapshot` from the event stream, and its Engine Bridge also authors an `ItemComplete` for each item the Batch Pipeline left open when a job ends (Item Settlement).
