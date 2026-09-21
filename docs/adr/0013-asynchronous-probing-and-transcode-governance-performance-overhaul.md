# 0013. Asynchronous Probing and Transcode Governance Performance Overhaul

> **Status**: Accepted
> **Date**: 2026-09-21
> **Context**: Media Engine Context & In-Process Engine Bridge
> **Amends**: ADR-0006, ADR-0008, ADR-0011

## Context

Performance profiling and benchmark measurements of `crates/mkv-core` revealed four systemic bottlenecks:
1. Fallback container probing (`mkvmerge -J`) executed via synchronous `std::process::Command`, freezing the Tokio async runtime worker threads during fallback probing or cold disk I/O.
2. Default audio transcoding used maximum algorithmic complexity (`complexity: 10`) and static thread allocation (`ffmpeg_threads: 2`), consuming ~16.5s of CPU time per episode and causing CPU oversubscription when multi-worker batches ran.
3. Font registry seeding (`FontRegistry::seed_existing`) held a mutex lock continuously while performing sequential BLAKE2b cryptographic hashing across all directory fonts, blocking concurrent worker tasks.
4. Inspection cache persistence allocated an intermediate `HashMap` and serialized full JSON strings into heap memory before writing.

## Decision

We decided to execute a coordinated performance overhaul across the Media Engine:

1. **Full Asynchronous Container Probing**:
   Migrate `probe_video` and `get_or_probe_video` to `async fn`. Fallback probing uses `tokio::process::Command` instead of `std::process::Command`, preventing reactor worker thread stalls.

2. **Transcode Governor & Complexity Downshift**:
   Downshift standard transcoding algorithmic complexity from `10` to `5` across default pipeline configs (reducing CPU transcode time by ~40.6% with imperceptible acoustic difference). Dynamically calculate `ffmpeg_threads = (total_physical_cores / workers).clamp(1, 4)` during request resolution when unspecified by the caller.

3. **Fine-Grained Font Seeding**:
   Refactor `FontRegistry::seed_existing` to compute BLAKE2b hashes for uncached files outside the mutex lock, taking the lock only for final batch insertion to reduce lock hold duration from ~160ms to < 15µs.

4. **Zero-Copy Streaming Cache Persistence**:
   Serialize `PersistentInspectCache` disk snapshots directly into `std::io::BufWriter` with `serde_json::to_writer`, eliminating temporary heap string allocations and collection copies.

## Considered Options

- **Retain Synchronous Probing with `spawn_blocking` Wrappers**: *Rejected*: Retains overhead of thread pool hopping and obscures async lifecycle semantics when the pipeline is already fully asynchronous.
- **Retain Maximum Complexity 10**: *Rejected*: Yields negligible acoustic gain on anime dialogue/BGM at 192k while doubling transcoding CPU time.
- **Binary Cache Format (Bincode/Postcard)**: *Rejected*: JSON format preserves human inspectability and cross-tool debugging with negligible size overhead for typical release libraries.

## Consequences

- Completely eliminates Tokio async reactor thread stalls during cold inspection.
- Reduces total batch CPU transcode duration by ~40% and balances core utilization across parallel worker slots.
- Eliminates font registry lock contention during pipeline startup.
- Cuts cache serialization latency by 75% and eliminates intermediate heap memory allocations.
