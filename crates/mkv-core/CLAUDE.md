# crates/mkv-core/CLAUDE.md

## Engine flow

`BatchRequest` (`request.rs`, resolved into a `PipelineConfig` by `BatchRequest::resolve`) → `BatchPipeline` (`pipeline.rs`):

1. Discovery (`scan.rs`).
2. Selected-items filtering, then (remux only, before `BatchStart` and any probe) assignment-target validation and the destination check (`validate_remux_destinations`: no output inside the input outside an `Output/` folder, none onto a discovered source after following symlinks, no two items on one output).
3. Probing. A native EBML parser (`ebml.rs`) runs first and falls back to `mkvmerge -J`. Results are cached in `PersistentInspectCache`, keyed by path plus size and mtime. Bulk probing, for both Inspect and remux runs, goes through `inspect.rs` (`inspect_videos`: one cancellable, cache-backed fan-out).
4. Per-directory asset resolution (`assets.rs`). A remux run prepares it before probing, so the probe can add the unselected videos a leftover chapter claim needs, then claims (`claim_release_chapters`).
5. Planning (remux mode: `RemuxPlanner` in `remux_plan.rs` decides one Remux Plan per item; `pipeline.rs` reports it).
6. Execution through a tokio `JoinSet` of planned jobs drawn from one global queue. Remux execution is in `remux.rs` and extraction in `extract.rs`.

The concurrency limits and the invariants are in the root `AGENTS.md`.
