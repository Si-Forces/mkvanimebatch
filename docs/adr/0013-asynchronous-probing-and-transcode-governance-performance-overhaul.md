# 0013. Asynchronous Probing and Transcode Governance Performance Overhaul

> **Status**: Accepted (amended 2026-10-02)
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

## Amendment 1 — 2026-09-28 (memo persistence)

### Decision 4 extended to the font hash memo

Decision 4 removed the "serialize to a heap `String` before writing" pattern from
`PersistentInspectCache`. `FontHashMemo::save` was missed: it still called
`serde_json::to_string` over the whole map and then wrote those bytes, so every save
materialized the entire memo in the heap in addition to the file being written. It now
uses the same `BufWriter` + `serde_json::to_writer` + `persist` shape.

**Why this needed recording rather than a quiet fix.** Decision 4 names only
`PersistentInspectCache`, so a reader taking the ADR at its word would conclude the
zero-copy property held for every persistent cache in the system. It did not. The two are
not otherwise identical — `save_internal` also sets `0o644` on the persisted file and
re-checks a revision counter before clearing `dirty`, neither of which the memo does —
so this amendment widens Decision 4 to the memo's *serialization* path specifically.

**How large the memo gets, stated honestly.** The memo is keyed by absolute font path and
is never pruned: entries are only ever inserted, and `data` is cleared solely when the
memo file fails to parse. So it grows monotonically with every distinct font path the
process has ever hashed. In the common layout — one `fonts/` directory shared by a whole
release, which is what `find_fonts_dir` resolves to — that is one entry per font file and
stays small. In a layout giving each episode directory its own `fonts/`, `fonts_dir_for`
resolves a different directory per episode, each seeds its own registry, and the entry
count multiplies by the episode count. The payload is therefore unbounded in the size of
the library's history rather than fixed, which is the part worth acting on: the saving is
one full-payload heap copy per write, not a constant.

**What it does not change.** The `dirty` flag still short-circuits the write when nothing
changed, so the copy was already paid only when a font was genuinely added. No other
decision and no measured figure in this ADR is affected; Decision 3's `seed_existing`
lock-hold numbers are untouched. Two latent defects in the old `save` were fixed in passing
because the rewrite made them visible: `temp_file.flush()` was a `std::fs::File::flush`,
which is a no-op that always returns `Ok`, making that error check vacuous; and
`NamedTempFile::new_in` failure was silently ignored, leaving no diagnostic at all.

**Coverage.** `test_font_hash_memo_large_save_round_trips_through_buffered_writer` writes
500 entries producing a ~78 KB payload, so the mid-stream flush path is genuinely taken
rather than fitting the 8 KiB buffer, and every entry is checked for truncation.
`test_font_hash_memo_save_is_noop_when_not_dirty` pins the early return by inode, which a
byte-comparison cannot distinguish from a rewrite that happened to produce identical
output.

## Amendment 2 — 2026-09-28 (spawn_blocking scope)

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

## Amendment 3 — 2026-09-28 (traversal path contract)

The scan traversal and font-seeding work in one commit changed three externally visible
contracts without recording any of them, and introduced a fourth rule it got wrong. This
amendment records all four.

> **Partially withdrawn.** The first contract below — "reported paths change spelling" — was reversed
> by Amendment 5. The other three stand.

**The walk root is resolved once, so reported paths change spelling.**
`scan_mkv_files` now canonicalizes its root and builds every entry path beneath that
resolved prefix. A caller that passed a relative root, a root containing a `..` segment,
or a root reached through a symlink therefore receives absolute, resolved
`ParsedFile::path` values where it previously received paths spelled the way it spelled
them. Item identity is unaffected: `manifest::get_item_id` already falls back to
canonicalizing both operands when `strip_prefix` fails, so relative IDs are unchanged.
The cost is that fallback firing — for a relative `--input-dir`, which `mkv-cli` stores
verbatim, each `get_item_id` call costs two `realpath` calls, and the pipeline calls it
repeatedly per file. That partly offsets the `realpath` the traversal itself now skips, so
the −25% scan-time figure should be read as applying to an absolute root.

**`ignore_paths` matches prefixes, not just exact directories.** An entry is ignored when
its resolved path starts with an ignore entry, and the match now also applies to individual
files rather than only directories. An ignore entry naming a directory therefore excludes
its whole subtree. No production caller passes this parameter today —
`generate_inspection_manifest` and the pipeline both pass `None` — so the change is
contract-tightening on an unused parameter, not a behavior change in the running system.

**A root that fails to resolve is not treated as canonical.** `scan_mkv_files` records
whether `canonicalize` succeeded and hands that flag down the traversal stack. It used to
unconditionally claim the root was canonical, which would have suppressed the `realpath`
that the dedup sets and the `ignore_paths` comparison both assume.

**`FileSignature::of_dir_entry` signs only regular files.** This rule the commit did not
have. It replaced a symlink-resolving `path.is_file()` with an `lstat`-equivalent
`entry.metadata()` plus a `meta.is_dir()` test, which cannot see through a link. A directory
named `Something.otf` — or a symlink to one — was signed with the directory's own byte
length, entered `claimed_name_size`, and silently denied any later `claim()` of that name
and size. A FIFO, socket, or device node named `*.otf` was worse: it passed the same test,
entered `claimed_name_size` with `len() == 0`, and then blocked forever in `hash_file`'s
`File::open`, inside the `FontRegistry::new` constructor, on input taken from a directory
listing. The rule now lives in the one function that owns symlink policy and tests
`is_file` on the resolved target.

**What it does not change.** No other decision and no other measured figure in this ADR is
affected. The memo-persistence accounting in Amendment 1 and the `promote_file`
device-boundary rule in Amendment 2 are untouched. The −25% figure itself is qualified
above rather than retracted, and no benchmark was re-run.

**Coverage.** `test_file_signature_of_dir_entry_rejects_directories` and
`test_seed_existing_directory_named_like_a_font_does_not_poison_claim` pin the directory
half of the seeding rule. The FIFO half is the same `is_file` predicate on the same branch
and is not tested separately, because creating a FIFO from a Rust test would mean shelling
out to `mkfifo`. `test_scan_mkv_files_honors_ignore_paths_for_individual_files` pins the
`ignore_paths` contract above. The path-spelling contract is reversed by Amendment 5.

## Amendment 4 — 2026-09-28 (current-state corrections)

The decisions stand; these statements above overstate or misname what the code does.

**Probing still blocks on the native path.** Consequence 1 ("completely eliminates Tokio
async reactor thread stalls") holds only for the `mkvmerge -J` fallback, which runs through
`tokio::process::Command`. `probe_video` first calls `probe_ebml_native`, which reads the file
through a synchronous `std::fs::File` + `BufReader` on the calling worker thread. The rationale
for leaving it in place was Amendment 2's 0.091 ms warm figure. Amendment 6 removed the
buffer-discarding seeks and loop-guard `lseek` calls behind that figure, but the read itself is
still synchronous, so a cold read can still block a worker. Tracked as issue #20; resolved by
Amendment 7.

**The governor is crate-private.** `cpu` is a private module of `mkv-core`;
`transcode_governor_threads` is `pub(crate)` and reachable only through
`BatchRequest::into_pipeline_config`.
`physical_core_count`, `default_workers`, and `probe_concurrency` are `pub(crate)`.
`physical_core_count` is called by `default_workers` and by
`BatchRequest::into_pipeline_config`; `default_workers` by `into_pipeline_config` and by
`manifest::generate_inspection_manifest`; `probe_concurrency` by
`manifest::generate_inspection_manifest` and by `pipeline.rs` (Amendment 6). "Physical cores" means
unique `(physical id, core id)` pairs from `/proc/cpuinfo` on Linux; elsewhere, or
when that file exposes no topology, it is `std::thread::available_parallelism()`.
Note that `/proc/cpuinfo` reflects host topology without detecting cgroup quota limits;
containers with fractional or restricted CPU quotas may run with higher concurrency
defaults unless `--workers` is explicitly specified.

**Memo keys and growth (Amendment 1).** The memo is keyed by the font path as the caller
spelled it (`to_string_lossy`), which is absolute only when the fonts directory is; a relative
`--fonts-dir` produces relative keys. `data` starts empty when the memo file is missing,
unreadable, or fails to parse, not only on a parse failure. `put` marks the memo dirty whenever
an entry changes, so a save also happens when an already-memoized font's size, mtime, or hash
changes, not only when a font is added.

## Amendment 5 — 2026-09-28 (traversal path spelling reversed)

Amendment 3's first contract — "reported paths change spelling" — is withdrawn.
`scan_mkv_files` again reports every `ParsedFile::path` in the spelling of the root the caller
passed. Resolved paths broke subtitle pairing: the Asset Resolver derives its search
directories from the caller's `input_dir`, so a video reported under a resolved spelling never
equals the directory of the subtitle beside it, and same-directory subtitles were skipped as
"subtitle files found but none matched". Any input whose spelling differs from its canonical
path triggered it — a symlinked or relative root on every platform, and on Windows every root,
because `canonicalize` adds the `\\?\` verbatim prefix and expands 8.3 short names.

The traversal keeps its `realpath` savings. Each stack entry carries its directory's canonical
path when known, and entries beneath it derive their dedup and `ignore_paths` keys by joining
their name onto it; only symlinks and entries beneath a root that failed to resolve pay a
`realpath`. The "root that fails to resolve" rule of Amendment 3 is unchanged in effect.

**Coverage.** `test_scan_mkv_files_keeps_caller_spelling_under_non_canonical_root` pins the
spelling, and `test_remux_dry_run_matches_subtitles_through_symlinked_input_dir` pins the
subtitle pairing it protects.

## Amendment 6 — 2026-09-30 (native EBML buffer retention and probe concurrency alignment)

### Native EBML parser buffer retention

Amendment 2 Finding 2 assumed that because `probe_ebml_native` wraps the file in a
`BufReader`, `read_exact` operations were essentially in-memory `memcpy` calls with
no headroom for optimization. That assumption was invalid:
1. `std::io::Seek::seek` on a `BufReader` unconditionally discards the internal buffer
   (`self.discard_buffer()`), even when seeking forward across small offsets that sit
   within the current 8 KiB buffer window. Calls to `seek_forward` and post-element
   transitions to `entry_end` repeatedly invalidated the buffer.
2. Every loop guard (`while reader.stream_position()? < end`) and `element_end()`
   invoked `stream_position()`, which delegates to `inner.stream_position()` and executes
   an `lseek(fd, 0, SEEK_CUR)` kernel syscall.
3. Because the buffer was discarded between elements, subsequent `read_exact` calls
   repeatedly refilled the buffer, generating dozens of `read` and hundreds of `lseek`
   operations per probe on a multi-track container. On cold caches or high-latency media,
   this serialized syscall overhead stalled async worker threads.

**Decision.** This supersedes Amendment 2 Finding 2 ("read_exact is a memcpy / No headroom").
The native parser now uses an internal `EbmlReader` wrapper that maintains the stream
position in memory (`pos: u64`), converts loop guards and `element_end` to in-memory
comparisons, uses `seek_relative` for intra-buffer skips without flushing, and batch-reads
element IDs, VINT lengths, and uints into small stack arrays.

Finding 2's premise ("read_exact is a memcpy") is refuted by inspection of `Seek for
BufReader`, which discards the buffer on every `seek`, combined with a counted walk:
`test_synthetic_multitrack_mkv_native_probe_and_buffer_retention` drives the top-level element
walk of a 6-track container through `TrackingReader` and observes 1 buffer fill and 0 seeks,
against the dozens of `read` and hundreds of `lseek` calls the old `seek_forward` /
`stream_position()` path issued. That test re-implements the walk rather than instrumenting
`probe_ebml_native`, which opens its own `File` from a path, so it pins `EbmlReader`'s syscall
behaviour as a proxy for the production parser — the parser's own count is not measured.
What is claimed here is the syscall reduction, not a wall-clock speedup: Amendment 2's 0.091 ms
warm measurement was taken on the pre-amendment parser and has not been retaken, and the probe
was already inside its <1 ms budget.

### Probe concurrency alignment

Preflight inspection (`manifest.rs`) derived probe concurrency from
`std::thread::available_parallelism()` (logical hardware threads), yielding 16 permits
on a 4-physical/8-logical machine. Pipeline execution (`pipeline.rs`) derives probe
concurrency from `(self.max_workers() * 2).clamp(2, 16)`, which defaults to physical
cores (`physical_core_count()`) via `request.rs` when unspecified (yielding 8 permits
on that machine). Preflight and pipeline probe limits now derive from the same pair,
`cpu::default_workers()` and `cpu::probe_concurrency(workers)`.

The two paths agree only when `--workers` is unset. `generate_inspection_manifest` does not
receive the Batch Request, so it passes `cpu::default_workers()` where the pipeline passes
`max_workers()`; an explicit `--workers 2` therefore yields 4 execution permits against 8
preflight permits. `generate_plan_manifest` (`mkv-cli/src/main.rs`) has the request in hand and
could thread the configured worker count through, but that widens this amendment's scope and is
not done here.
`--workers` remains the lever for callers who need the two to match exactly.

**Coverage.** `test_ebml_reader_retains_buffer_on_intra_buffer_skips` and
`test_ebml_reader_multi_buffer_reads` verify that intra-buffer forward skips via `skip()`
and `seek_to()` advance the tracked position without issuing underlying `read` or `seek`
calls, and multi-buffer reads refill predictably.
`test_synthetic_multitrack_mkv_native_probe_and_buffer_retention` pins that `probe_ebml_native`
successfully parses a 6-track synthetic MKV from disk, and its counted walk — a proxy for the
parser, which `probe_ebml_native` does not instrument — completes in 1 buffer fill and 0 seeks.
Wall-clock is not re-measured here; Amendment 7 measures cold-cache behaviour on live media
(issue #20).

## Amendment 7 — 2026-10-02 (native probe on the blocking pool)

**Decision.** `probe_video` runs `probe_ebml_native` through `tokio::task::spawn_blocking`.
This reverses, for this one call, the rejected option "Retain Synchronous Probing with
`spawn_blocking` Wrappers", Consequence 1's stall claim for the native path, and Amendment 2
Finding 1's reason for keeping it inline. That finding's 3.2x figure was dispatch cost on
sub-millisecond metadata calls that never wait on the device. A cold header read does wait on
the device, and probes fan out up to 16 at a time (`cpu::probe_concurrency`) against a runtime
whose default worker count is the logical CPU count, so a batch of cold probes can pin every
worker. The `mkvmerge -J` fallback stays on `tokio::process::Command`. The change sits inside
`probe_video`, so pipeline probing, preflight inspection (`manifest.rs`) and
`get_or_probe_video` all take it.

**Measurement.** `probe_ebml_native`, 20 probes per run (20 files on the HDD), real 1080p BD
releases. The page cache was evicted per file with `dd if=FILE iflag=nocache count=0` (no root
needed); each cold probe was followed by a warm one.

| Media | Cold p50 | Cold p99 / max | Warm p50 |
|---|---|---|---|
| USB HDD, first run (drive idle) | 139 ms | 2619 ms | 0.50 ms |
| USB HDD, second run (drive cache warm) | 1.49 ms | 1.69 ms | 0.19 ms |
| SATA SSD, btrfs (5 files x 4) | 15.8 ms | 17.4 ms | 0.18 ms |

Every cold figure exceeds the <1 ms budget. This resolves Amendment 4's "probing still blocks
on the native path", Amendment 2 Finding 1's "it stays for now", and Amendment 6's #20 pointer.
`dd iflag=nocache` evicts only the page cache, not the drive's own cache, which is why the
second HDD run is fast. No network mount was measured.

**Failure.** If the blocking task panics, the `JoinError` is logged with `tracing::warn!` and
the probe falls back to `mkvmerge -J`, as for any other native failure (ADR-0006).

**Cancellation limit.** A blocking task cannot be aborted. After a cancel, `abort_all`, or
`stop_job`, an in-flight native probe reads until it finishes. In the CLI, dropping the runtime
waits for it, so a read on a hung mount delays exit; before, the same read pinned a reactor
worker. No `shutdown_timeout` is added.

One thing is worse than before. The probe permit belongs to the async task, so aborting it
releases the permit while the detached read keeps running, where an inline read held its
permit until it returned. Reads orphaned by a cancel, at most `probe_concurrency` per cancel,
are counted by no permit, so a job started right after a stop can briefly run more reads than
its probe limit; repeated stop-and-start cycles on a hung mount can accumulate them. In
exchange, cancellation is no longer delayed when cold reads pin the workers. The permit is not
moved into the blocking task: each job builds its own semaphore, so holding the old one would
not bound the next job.

**Out of scope.** Per-directory synchronous I/O inside `async fn` (`prepare_release_directory`,
`scan_fonts`, `claim_leftover_chapters`, `is_chapter_xml`, the `scan_mkv_files` walk) runs once
per directory rather than fanning out. It is tracked as #23.

**Coverage.** `test_probe_video_stalled_native_read_does_not_block_runtime` points
`probe_video` at a FIFO, whose `open()` blocks until a writer appears. It asserts that a timer
on a `current_thread` runtime still fires. It fails without the offload and passes with it.
Because this test shells out to `mkfifo`, the reason Amendment 3's **Coverage.** gives for not
testing the FIFO half of the seeding rule no longer holds; that test is still not written.
