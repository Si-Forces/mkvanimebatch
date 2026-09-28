# 0013. Asynchronous Probing and Transcode Governance Performance Overhaul

> **Status**: Accepted (amended 2026-09-28)
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

## Amendment — 2026-09-28

- Adds to ADR-0013 "Decision": the copy fallback in `promote_file` is restricted to genuine
  device boundaries, and the blanket `spawn_blocking` proposal below is rejected. The
  "Considered Options" entry is left unedited — it is scoped to probing, which this
  amendment does not change.

### Scope of `spawn_blocking` on file promotion

A later review proposed nine performance changes to `crates/mkv-core`. Benchmarking against
the current code rejected seven, superseded one, and acted on a single sub-case of another.
Findings are numbered to match the original report so a reader holding it can cross-reference:

| # | Proposal | Measured | Outcome |
| --- | --- | --- | --- |
| 1 | `spawn_blocking` around every `std::fs` call in `promote_file` / `remux` / `extract` / `fonts` / `scan` | `create_dir_all` 0.011ms inline vs 0.035ms wrapped | 3.2x slower; dispatch dominates sub-millisecond ops. Also mis-scoped: the `std::fs` calls in `remux.rs`, `extract.rs` and `scan.rs` sit inside `async fn`, not in sync helpers. `scan.rs::probe_video` is the one that is this ADR's own subject — see #20; it measures 0.091ms warm, which is why it stays for now |
| 2 | Buffered EBML VINT reads in `probe_ebml_native` | totals 0.091ms, already inside its <1ms budget | No headroom; the probe `break`s at the first `Cluster` and never walks clusters, and runs on a `BufReader` so `read_exact` is a memcpy |
| 3 | `Arc`-share `MkvmergeMetadata` across jobs | Each job probes its own file; `InspectedVideo::new` moves, never clones | Nothing is shared, so there is nothing to share |
| 4 | Move CPU-bound work off the async runtime | all flagged CPU work totals 0.13ms vs a 60ms `mkvextract` | No meaningful load; ADR-0011's dual governor already covers subprocess scheduling |
| 5 | Optimise the `parse_episode` regex pipeline | 38.9µs/file (~39ms per 1000-file season) | Irrelevant next to a ~60ms `mkvextract` invocation |
| 6 | Parallelize `FontRegistry::seed_existing` | Cold 364ms -> warm 2.1ms via the existing `FontHashMemo` | Decision 3 above already removed 99% of it |
| 7 | Add micro-benchmarks for `parse_episode` / `probe_ebml_native` / `hash_file` / `promote_file` | measures 0.1–40ms operations against a 60ms subprocess | Not taken: the measurement that mattered here was the copy's wall-clock cost, taken directly rather than as a micro-benchmark |
| 8 | Replace `fancy-regex` with `regex` | `fancy-regex` already delegates non-backtracking patterns to `regex` internally | ~0 gain; only the 3 lookbehind patterns genuinely need `fancy-regex` |
| 9 | Enable tokio's `parking_lot` feature | `tokio::sync::Mutex` is used only in tests; production uses `Semaphore`/`mpsc` | The feature does not touch those primitives |

The blanket `spawn_blocking` proposal in #1 also contradicts this ADR's own rejected option
above — an option scoped to probing, which the proposal extends to the filesystem — and the
numbers vindicate that rejection as it applies to sub-millisecond calls. Separately,
`tokio::fs::copy` measured *slower* than plain `std::fs::copy` (0.42ms vs 0.32ms), so the
proposal's "or use `tokio::fs` async equivalents" is backwards for this operation.

One genuinely blocking operation survived review: within finding #1, the table's
`promote_file` row listed `copy` alongside cheap metadata calls, and that one entry is real —
`promote_file`'s cross-device fallback is a full-payload `std::fs::copy`.

**The defect.** The copy was the unconditional last resort in `promote_file`: it was reached
whenever `rename` failed and the preceding backup-and-rollback branch did not return success.
That branch returns early only when it manages to rename `staged` onto `final_path`; it has
no `else`, so it fell through to the copy on every other outcome — a cross-device error, an
absent destination, a failed backup rename, or a failed `staged -> final_path` rename whose
rollback then succeeded. A missing source, a locked destination, or a permissions problem all
reached a whole-payload copy that then failed the same way.

Both `extract` and `remux` place their scratch directory *inside* the final destination
directory — `tempdir_in(dest_dir)` and `tempdir_in(out_dir)` respectively — so the staged file
and the final path are always under the same filesystem, and a `rename` between them never
returns `EXDEV`. The cross-device branch is therefore **unreachable in production**, and the
realistic production trigger for the copy was a non-cross-device fall-through: most plausibly
a Windows `EPERM` from an AV or indexer handle holding the destination open. On Linux and
macOS the fall-throughs a caller can actually reach are ones a copy cannot rescue.

**Cost — measured synthetically, not observed in production.** The 3.18s figure is a
`std::fs::copy` of a 1.5GB file on disk, timed by forcing the fallback directly. It was never
observed stalling a real batch: on the primary platform the copy is unreachable, as above. It
sizes the risk rather than documenting an incident. The unmeasured production exposure was a
Windows `EPERM` fall-through copying a real episode, which would be seconds by the same
reasoning but has never been timed. The decision below removes that exposure.

**Decision — gate the fallback, do not offload the promotion.** `promote_file` now returns
the rename error for anything that is not a device boundary, so the multi-gigabyte copy that
prompted this review no longer happens at all. Offloading the promotion to the blocking pool
was considered and rejected: `spawn_blocking` work is detachable, and every caller wraps the
item in a `biased select!` on a `CancellationToken`, so cancelling mid-promotion would report
the item `Cancelled` while the copy kept writing and the io permit had already been released.
That regression is worse than the stall it removes, and gating the fallback removes the stall
at its source instead.

The device test also moves into a named `is_cross_device_rename`, which closes a pre-existing
platform leak: raw code 18 is `EXDEV` on Linux/BSD but `ERROR_NO_MORE_FILES` on Windows, and
was matched unconditionally, so a Windows error 18 sent the whole payload to a copy.

**What this costs.** `rename(2)` requires write permission on *both* parents, so a staged file
in a read-only or foreign-owned directory fails the rename while remaining readable — and the
old copy path succeeded there, because it reads the source, writes a temporary sibling into
the writable destination, and renames within that one directory. Such a promotion now returns
`PermissionDenied`. No caller in this crate is affected: `extract` and `remux` stage into a
temp directory this process created, and font promotion stages into `<fonts_dir>/.font_staging`.
`promote_file` is public, so a future caller staging elsewhere would meet it, and
`test_promote_file_readonly_source_parent_returns_error` pins the new behaviour so the trade
stays visible.

Font promotion is unaffected either way: it is synchronous because `FontRegistry::promote`
holds a `parking_lot::MutexGuard` across the call, which would make an awaiting future
non-`Send` and break `tokio::spawn`.

**Coverage.** `test_promote_file_cross_device_copy` exercises the copy end to end, including
that `rename` consumes the temporary sibling, and skips where the host offers no second
filesystem. Two gaps remain, stated rather than hidden. The `remove_file(&temp_sibling)`
cleanup on the copy's *failure* paths is untested, because forcing a partial-write failure
portably is not practical. And a source-side rename failure is indistinguishable from a gated
one, because `std::fs::copy` opens the source before the destination and fails it the same
way — only the source-parent case above changes the outcome, and that is what the gate test
covers. The classification itself is unit-tested in `domain.rs`.
