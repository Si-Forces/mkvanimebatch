# Context Map

## Contexts

- [Media Engine](./crates/mkv-core/CONTEXT.md) — parses, indexes, scores, probes, and executes anime fansub batch processing
- [Desktop Application](./desktop-tauri/CONTEXT.md) — manages GUI state, queue progression, user configuration, and desktop presentation

## Relationships

- **Desktop Application → Media Engine**: Desktop Application imports `mkv-core` as an in-process library (and optionally invokes `mkv-cli`); consumes domain types (`InspectionManifest`, `BatchItem`, `ItemOutcome`) and progress channels.
