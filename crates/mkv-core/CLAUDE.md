# crates/mkv-core/CLAUDE.md

## Engine flow

`BatchRequest` (`request.rs`, resolved into a `PipelineConfig` by `BatchRequest::resolve`) → `BatchPipeline` (`pipeline.rs`):

1. Discovery (`scan.rs`).
2. Selected-items filtering.
3. Probing. A native EBML parser (`ebml.rs`) runs first and falls back to `mkvmerge -J`. Results are cached in `PersistentInspectCache`, keyed by path plus size and mtime. Bulk probing, for both Inspect and remux runs, goes through `inspect.rs` (`inspect_videos`: one cancellable, cache-backed fan-out).
4. Per-directory asset resolution (`assets.rs`).
5. Planning (remux mode: `RemuxPlanner` in `remux_plan.rs` decides one Remux Plan per item; `pipeline.rs` reports it).
6. Execution through a tokio `JoinSet` of planned jobs drawn from one global queue. Remux execution is in `remux.rs` and extraction in `extract.rs`.

The concurrency limits and the invariants are in the root `AGENTS.md`.
