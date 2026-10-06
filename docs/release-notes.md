# Release Notes

## Unreleased

### Breaking Changes

- **`FontRegistry::claimed_hashes()` returns only the hashes computed so far**: a font already in the fonts directory is hashed only when a staged font of its size is promoted, so until then its hash is absent from the returned set.
- **`JobEvent` and `BatchItemInfo` drop `Deserialize`**: `JobEvent` and `BatchItemInfo` in `mkv-core::pipeline` no longer derive `Deserialize`, and unused `#[serde(default...)]` annotations were removed from `JobEvent` variants.
- **`EpisodeType::label()` parameter dropped**: `EpisodeType::label(&self)` no longer accepts `long: bool`; it always returns the standard short label (`EP`, `Part`, `OVA`, `SP`).
- **Asset font fields and `use_source_fonts()` removed**: Removed write-only `fonts: FontInfo` from `ResolvedAssets` and `AssetResolver`, removed unused `AssetResolver::use_source_fonts()`, and eliminated redundant font fallback scanning in `AssetResolver::discover`, which drops its `fonts_dir` and `font_cache` parameters. Directory-level font resolution is owned by `prepare_release_directory`.
- **`execute_remux_job` requires the I/O governor**: its `io_semaphore` parameter is now `&Semaphore` instead of `Option<&Semaphore>`. A closed semaphore fails the item instead of running it without the ADR-0011 disk-write limit, in both remux and extract.
- **`sub_offset` renamed to `default_sub_offset_ms` in whole milliseconds**: the global subtitle offset on `BatchRequest` (and so the `run_job` IPC payload), `PipelineConfig`, and `RemuxOptions` is now `default_sub_offset_ms`, an `i64` in milliseconds instead of `sub_offset`, an `f64` in seconds; `build_mkvmerge_args` takes it as `i64`. No alias is kept: a request that still sends `sub_offset` has the key ignored and runs with no global offset, and a fractional `default_sub_offset_ms` fails to deserialize. `--sub-offset` and the desktop Remux view still take seconds and convert them to whole milliseconds, rounding half away from zero. The desktop now sends an entered `0` as `0` rather than `null`; both mean no global offset.
- **Non-finite subtitle offset rejected**: `mkv-cli` rejects a `--sub-offset` value that is not finite, such as `inf`, `-inf`, or `NaN`, or whose milliseconds do not fit `i64`. With the field now an integer, `BatchRequest::resolve` no longer checks it.
- **Lowercase `m` bitrate suffix rejected**: CLI and desktop requests now reject rates such as `2m`, which ffmpeg interprets as milli rather than mega. Use uppercase `M` for mega; `k` and `K` remain accepted. Surrounding whitespace is trimmed before encoding.
- **Removal of `--keep-fonts` CLI Flag**: The `--keep-fonts` command-line option has been removed from `mkv-cli`. Invoking `mkv-cli` with `--keep-fonts` will now result in an unknown flag error.
- **Removal of `keep_fonts` from Typed Contracts & IPC**: The `keep_fonts` field has been removed from `BatchRequest`, `PresetProfile`, `PipelineConfig`, and desktop Tauri IPC command schemas (`run_job`, `get_preset_profiles`). Desktop presets and UI controls no longer expose a font-retention toggle. No compatibility alias is retained.
- **Public `mkv-core` API removals**: Removed `SubIndex::{lookup, indexed_count, primary_count, fallback_count}`, `ChapterIndex::{indexed_count, rejected_count}`, `CoverIndex::{indexed_count, has_global}`, and `AssetResolver::{has_local_subtitle, count_unmatched_chapter_videos}`. Downstream code calling these helpers must migrate; the methods are no longer available.
- **`mkv-core` public API narrowed to what the workspace uses**: The `cpu` module is now crate-private. Removed or no longer public, by module:
  - `assets`: `FontsOrigin`; `PreparedRelease::{mkv_dir, videos, fonts_origin}`; `AssetResolver::{new, discover_with_caches, resolve_chap}` (`discover` takes the former `discover_with_caches` arguments); `SubIndex::{all_subtitles, lookup_same_dir, lookup_ancestors, lookup_fallback}`; `ChapterIndex::has_match`; `CoverIndex::lookup_fallback`. `prepare_release_directory` drops its `videos` parameter.
  - `domain`: `SUBTITLE_LANGS_PRIMARY_ORDER`, `SUBTITLE_LANGS_FALLBACK_ORDER`, `SUBTITLE_EXTENSIONS`, `CHAPTER_EXT`, `DIALOGUE_KEYWORDS`, `KNOWN_LANG_TOKENS`, `ep_type_label`, `format_ep`, `parse_progress_percentage`, `is_windows_reserved_device`, `EpisodeType::is_special_or_ova`. `is_dialogue_track` takes `&str`; `select_best_subtitle_track` returns `Option<(u32, String)>`; `select_audio_track` returns `Option<AudioTrack>`; `read_progress_stream` takes `&mut Vec<u8>`.
  - `extract`: `build_mkvextract_args`, `ExtractSummary::mkv_path`.
  - `fonts`: `FONT_DIR_NAMES`, `FontMemoEntry`, `FontHashMemo::memo_file`, `FontRegistry::fonts_dir`, `FontClaim::{name, size, identity}` fields (use `FontClaim::name()`), `FontClaimsGuard::{defuse, len, drain_claims}`.
  - `manifest`: `parse_resolution`, `parse_video_codec`, `parse_audio_track_info`, `extract_mkv_metadata`.
  - `pipeline`: `PipelineConfig::cache_file`, the `BatchPipeline` fields; `with_log_sender` takes a `LogSender`.
  - `remux`: `RemuxSummary` (`execute_remux_job` returns the log lines), `PlannedJob::{base, release_dir, item_id}`, `TrackSelection::{audio_channels, audio_reason, has_source_chapters}`.
  - `request`: `assignments_to_track_map`, `canonical_assignment_target_id` (use `clean_relative_path`), `TrackAssignment::{clean_target_video, is_relative_asset}`.
  - `scan`: `COVER_LIKE_STEMS`, `parse_attachments`, `InspectCacheEntry`, `get_default_cache_file`, `InspectedVideo::{metadata, into_metadata, attachments, fonts_count}`, `InspectionResult::is_success`, `PersistentInspectCache::{get, put}` (use `get_path`/`put_path`) and its fields. `cover_attachment`/`font_attachments` return references.
- **`mkv-core` type surface narrowed**: beyond the items listed above, these public types lost trait implementations, and one conversion was removed: `InspectedVideo` no longer derives `Serialize`, `Deserialize` or `Default`, and `impl From<MkvmergeMetadata> for InspectedVideo` is gone; `InspectionResult` no longer derives `Serialize` or `Deserialize`; `FileSignature` no longer derives `Hash`, `Serialize` or `Deserialize`; `FontClaim` no longer derives `Clone`. `AssetResolver` now derives `Default`. Downstream code relying on these traits must migrate.
- **App version no longer pinned in `tauri.conf.json`**: the `version` key was removed so the workspace package version (`[workspace.package]` in the root `Cargo.toml`, inherited by `desktop-tauri/src-tauri/Cargo.toml`) is the single source of truth; Tauri falls back to it. The packaged version is unchanged.
- **Release profile no longer aborts on panic**: `[profile.release]` no longer sets `panic = "abort"`. This is what lets the Engine Bridge intercept a task panic, emit a terminal failure event, and release the job slot instead of the whole desktop process dying. Expect a larger packaged binary; crash containment takes priority over size.
- **`BatchStart` / `JobSnapshot` `input_dir` is now the display path form**: On Windows these fields no longer carry a verbatim (`\\?\`) prefix, and `\\?\UNC\` is rewritten to `\\`. Consumers that compare the path against a raw `std::fs::canonicalize` result must strip the prefix themselves. This matches the form the Inspection Manifest's `canonical_dir` already used; see *Canonical Directory Identity* in `architecture.md` §5.1.
- **Removal of `--sub-assignments` CLI Flag**: The deprecated `--sub-assignments` alias has been removed from `mkv-cli`; passing it now fails with `Unknown argument: --sub-assignments`. Use `--assignments`, which still accepts the legacy flat `{"video.mkv": "sub.ass"}` map as well as `TrackAssignment` arrays.
- **`TrackAssignment.track_type` is a typed `AssignmentKind`**: the field is now `AssignmentKind` (only `Subtitle`) instead of `Option<String>`, and is always serialized as `"track_type":"subtitle"`. Assignment JSON accepts only lowercase `"subtitle"` or an omitted key; `"Subtitle"`, padded values, and `null` are now rejected. Malformed `TrackAssignment` arrays, bare or under an `assignments` key, now report the underlying reason (e.g. ``Invalid track assignment: unknown variant `audio` ``) instead of a generic schema error.
- **`PresetProfile.description` removed**: The `description` field is gone from `PresetProfile` and from the `get_preset_profiles` payload.
- **Inspection cache entries drop `mtime`**: `inspect_cache.json` entries no longer store the unused `mtime` field; freshness is still checked by size and `mtime_ns`. Existing cache files keep loading.
- **`generate_inspection_manifest` prunes the inspect cache**: after a completed inspection it now drops the cache entries under the input directory whose file the scan did not discover, then saves the cache, including when the directory holds no MKV files. It previously only added entries. A cancelled inspection prunes nothing.
- **`InspectionManifest::filter_selected` replaced by `select_items`**: `select_items(&[String])` returns `Result<InspectionManifest, String>`, failing with `Selected items not found among discovered MKV files: ...` when a selection names no discovered item. It keeps selection order and drops repeated selections.
- **Asset scan cache is a `ScanCache`; `ScanResults.videos` removed**: `scan_assets` takes `Option<&mut ScanCache>` instead of a `HashMap<PathBuf, ScanResults>` and returns `Arc<ScanResults>`; `ReleasePreparationOptions.scan_cache` and `AssetResolver::discover` take `ScanCache` too. `ScanResults` no longer has a `videos` field. `ChapterIndex::add` takes `&ParsedFile` and no longer checks that the file is chapter XML: `scan_assets` is the only place a file is sniffed for chapter content.
- **`AssetResolver::resolve_for` no longer checks that the assigned subtitle exists, and cannot fail**: it takes `assigned_subtitle` as given and no longer fails with `Assigned subtitle '...' no longer exists`, and it returns `ResolvedAssets` instead of `Result<ResolvedAssets, String>`, since it can no longer fail. The Remux Plan makes that check once, before it resolves, so a batch still aborts on a missing assigned subtitle exactly as before (ADR-0008). A caller outside the pipeline must check `Path::is_file` itself.
- **`generate_inspection_manifest` takes an `Arc` cache**: its `persistent_cache` parameter is now `Option<Arc<PersistentInspectCache>>` instead of `Option<&PersistentInspectCache>`, matching `PipelineConfig.cache`, because each spawned probe task holds the cache. Callers wrap their cache in an `Arc`.
- **`BatchPipeline::from_request` removed**: it had no callers left once the CLI and the desktop both resolve the request themselves. Call `BatchRequest::resolve` and pass the config to `BatchPipeline::new`.
- **`BatchRequest::resolve` replaces `validate` and `into_pipeline_config`**: `BatchRequest::validate` is now private and `into_pipeline_config` is removed; `BatchRequest::resolve(self) -> Result<PipelineConfig, String>` validates the request and converts it in one step, so a caller validates and converts with one call. The ranges and defaults are public constants (`WORKERS_RANGE`, `IO_JOBS_RANGE`, `FFMPEG_THREADS_RANGE`, `COMPLEXITY_RANGE`, `DEFAULT_IO_JOBS`, `DEFAULT_PRESET_ID`). An unset bitrate and complexity now come from the `balanced` Preset Profile rather than `PipelineConfig::default`, which is a fixed fixture for tests; the values are unchanged. `PipelineConfig.io_jobs` now holds the effective limit: a requested `0` resolves to the worker count, so it is never `0`, and a hand-built config with `io_jobs: 0` now runs with 1 I/O job instead of the worker count. The invalid-bitrate error now reads ``Invalid bitrate format: '<value>'. Must be a positive integer with an optional k/K/M suffix, like 128k or 192k.`` in the desktop too, and `mkv-cli` no longer rejects `--preset` or `--bitrate` while parsing: `resolve` reports them. New `EffectiveSettings` type, from `PipelineConfig::effective_settings()` and `EffectiveSettings::defaults()`, holds the workers, I/O jobs, FFmpeg threads, bitrate and complexity a run uses.
- **Media Tool runner parameter on `execute_remux_job` and `extract_mkv_file`; `read_progress_stream` removed**: both take a trailing `runner: &dyn MediaToolRunner` (`mkv-core::media_tool`); pass `&SubprocessRunner` to spawn the real tools. `BatchPipeline::with_media_tool_runner` replaces the runner a pipeline uses, defaulting to `SubprocessRunner`. `domain::read_progress_stream` is gone: `media_tool::run` parses the "Progress: N%" lines.

### Core Scanning

- Recursive MKV scanning skips canonical-key path allocations for non-MKV files and directories excluded by the output/font-directory name filters. Canonical keys are still derived for candidates that need ignore-path or deduplication checks.
- Asset discovery stops repeating work for directories shared by many Release Directories (ancestors and the `--assets-dir` fallback): chapter XMLs are sniffed once per directory per run instead of on every index build, a repeated spelling of a scanned directory skips the `realpath`, a cache hit no longer deep-copies the scan results, and directory entries are classified from `read_dir` file types instead of a resolving `stat` each.
- A stop during the per-directory asset pass takes effect after the directory being scanned, instead of only once planning starts. Remux checks before each Release Directory and settles every item as `cancelled`; preflight inspection checks before each directory whose subtitles it lists and fails with `Inspection cancelled`. A directory scan already underway still runs to completion.

### Core Performance & Probing

- The native EBML probe runs on Tokio's blocking pool. Concurrent probes reading cold or slow media (cold-read latency measured on a USB HDD and an SSD) no longer stall the async runtime, which froze progress events and NDJSON output for the CLI and desktop app. Once started, a native probe is not cancellable and reads to completion after a stop.
- `probe_ebml_native` preserves `BufReader` buffers across intra-buffer element skips using relative seeking and tracks stream positions in memory, eliminating redundant kernel `lseek` loop guards and reducing probe I/O from hundreds of syscalls to roughly one buffer read per 8 KiB of header scanned.
- Preflight inspection default probe concurrency is now calculated from physical core count via `cpu::probe_concurrency(cpu::default_workers())`, aligning with pipeline execution's default. Previously it used logical cores (`available_parallelism()`), which allowed up to 16 concurrent probes on machines with 4 physical and 8 logical cores.
- The physical core count is capped at `available_parallelism()`, which accounts for cgroup CPU quotas and `taskset` affinity where it can read them. In a CPU-limited container or under `taskset`, the defaults derived from it (CLI `--workers`, `auto` FFmpeg threads, preflight probe concurrency) follow the CPUs the process may use instead of the host's `/proc/cpuinfo` topology. On an unconstrained host they are unchanged. Pinning to two hyperthread siblings of one core counts as 2.
- Extraction no longer hashes every font already in the fonts directory when it starts. Existing fonts are recorded by name and size, and one is hashed only when a staged font of its size is promoted, since only then can it be a content duplicate. This helps most with `--no-cache` or the desktop "no cache" box, which have no font hash memo, so each run hashed every existing font once per fonts directory. Duplicates are still caught.
- Preflight inspection looks up and stores each file's inspect-cache entry inside its probe task, as the pipeline does, instead of checking every file in one serial pass before the first probe starts. Lookups now run alongside probing, and pending ones are aborted with the probes on cancel. On high-latency storage (network shares, USB HDDs) the first probe starts sooner, and an all-hit re-Inspect is no longer fully serial.
- `--ffmpeg-threads` and the desktop "FFmpeg Thds" field are now described as the FLAC decoder thread cap per transcode. `-threads` precedes `-i`, so it never reached the encoder, and libopus is single-threaded: each transcode encodes on about one core whatever the value, and `--workers` is the only lever on encode CPU use. Behavior, the `auto` default `(cores / workers)` clamped to 1–4, and validation are unchanged; the `--help` text, the field's tooltip, the README, and ADR-0013 (a dated note under Decision 2) say so now.

### Inspect Cache Lifetime

- A completed preflight inspection prunes the inspect cache: `generate_inspection_manifest` drops the entries under the input directory whose file the scan did not discover, so the cache follows the current library instead of every file ever probed. Entries outside the input directory are kept, and a cancelled inspection prunes nothing. Output and font directories are never discovered, so inspecting their parent drops their entries; they are re-probed if inspected later.
- The cache file is read with one `fs::read` and parsed with `serde_json::from_slice` instead of streamed through `serde_json::from_reader`.
- `PersistentInspectCache::clear` now discards a save already in flight, which could otherwise write the cleared entries back.
- Desktop: one inspect cache per app session. The first Inspect or Run that wants it loads the file, and later Inspects and Runs share it, so an Inspect followed by a Run reads the file once instead of twice. Entries written meanwhile by the CLI or a second app instance are not picked up, and the session's next save overwrites them.
- Desktop: "Clear cache" clears the session's cache, so a later save no longer restores the cleared entries, and it no longer parses the file just to delete it. It works during a running job.
- CLI: `--no-cache` no longer parses the default cache file, and neither does a batch run (its pipeline loads its own). `--clear-cache` deletes the file without parsing it.

### Audio Track Selection

- Tracks named `Comments`, `Commentaries`, and lowercase variants are excluded from primary audio selection so they are not transcoded over feature audio.

### Worker Panic Reporting

- A worker task that panics now completes its item with outcome `error` and the message `worker panicked: ...`. Previously that item never received an `ItemComplete` event.

### Media Tool Invocation

- Extraction now logs mkvextract's exit-code-1 warnings in the item's log, as remux does for mkvmerge (`  Warning: <text>`). It discarded them before.
- The `mkvmerge -J` fallback probe accepts exit code 1 (warnings) when the output is valid JSON. It failed with `mkvmerge exit code Some(1)` before.
- `mkvmerge`, `mkvextract`, and `ffmpeg` failures share one wording: `Failed to run <tool>: <error>`, `<tool> timed out after <N> seconds`, and `<tool> failed (exit code N | terminated by signal): <stderr and stdout, else the code>`. A failed transcode reads `Audio transcode: ffmpeg failed (exit code N): ...` instead of `ffmpeg audio transcode failed: ...`.
- Every Media Tool starts with stdin closed; `mkvmerge` and `mkvextract` inherited it before.

### Extracted Subtitle Naming

- When a file name does not parse as an episode, its extracted subtitle is named `{stem}_{lang}_track{id}.ass` instead of repeating the stem (`{stem}_{stem}_{lang}_track{id}.ass`). Parsed episodes are unchanged: `{stem}_{EpLabel}{ep}_{lang}_track{id}.ass`.

### Desktop Application

- The theme follows the system light/dark preference, live, until the user toggles it; only an explicit toggle is stored and applied to the native window. Unrecognized stored themes are ignored, consistently with the early theme bootstrap.
- The window's corner resize handles now have a usable size and a diagonal resize cursor.
- Stepper limits match the engine's validation: workers 1–64, Disk I/O jobs 0–64, FFmpeg threads 1–32. Fractional and out-of-range values in those three fields are rejected before inspection instead of being truncated to integers. The engine does the rejecting (the new `resolve_batch_request` command; the webview has no check of its own): an out-of-range whole number logs the engine's message, such as `Worker thread count must be between 1 and 64`, while a fractional or negative one is refused earlier, as the command's arguments are read, and logs that error text instead. A rejected request leaves the previous run's log in place and adds the error line to it.
- The UI uses the system font stacks; bundled web fonts are no longer loaded.
- The per-item subtitle offset reads exponent notation correctly (`1e3` is 1000 ms; it was 1 ms). A value beyond the safe-integer range now leaves the global offset in effect instead of failing the run.
- Stop and Close no longer freeze the window while a job shuts down. Their teardown runs off the main thread, so the log and queue keep updating, including when the cooperative cancel misses its 2 s deadline and the job is aborted. The "Stop requested; waiting for shutdown." log line now appears when Stop is pressed instead of after the job has stopped.
- Exited media tools are now removed from the telemetry process list. They used to stay in it for the rest of the session, and each one's last write rate was counted again on every poll, so the disk write rate could stay up after a batch. Each telemetry poll refreshes only disk usage per process and runs off the main thread, and startup no longer takes a full process snapshot.
- The `set_window_theme` IPC command no longer coerces an unrecognized theme name to Dark. An unrecognized name is now ignored and leaves the native window exactly as it was, including any pin set by an earlier explicit choice. The webview only ever sends `light` or `dark`, so this affects a caller that bypasses the UI.
- Starting a Run on the directory already shown no longer empties the queue list while it is re-inspected. Cards whose items are unchanged are kept, and only a changed item's card is rebuilt. A progress update now rewrites only a card's progress bar and status text, and the log lines waiting while the window is hidden are capped at the console's 2000-line limit.
- The Workers and Disk I/O inputs in the Remux and Extract views start empty and show `auto (N)`, where N is the engine's default worker count or I/O job limit, read through the new `get_default_settings` command. Before, both Workers inputs were pre-filled with 8, so the desktop always overrode the default: a 4-core machine or a 2-CPU container ran 8 workers and `auto` FFmpeg threads dropped to 1. The Disk I/O inputs were pre-filled with 2, a copy of the engine's default. An empty field now sends `workers: null` or `io_jobs: null` and the engine's default applies; a typed number is used as is, and clearing the field returns to auto. The − and + buttons on an empty field start from N. If N cannot be read, the placeholder stays `auto` and the worker and I/O job chips show `Standby` and `Idle` with no count; they used to show 8 workers and 2 I/O jobs before anything was known.
- New desktop commands: `get_default_settings` returns the Effective Settings of a request that sets nothing (its `workers` is the N above, and its `io_jobs` the idle chip's count); `resolve_batch_request` resolves a request exactly as `run_job` does, without starting anything, and returns its Effective Settings or the engine's validation error. The webview calls it before a Run's preflight inspection and shows the resolved counts on the chips, so an empty Workers field or an I/O jobs value of 0 shows the number the run uses.
- The `JobSnapshot` carries the Effective Settings its job runs with (`effective_settings`), so after a webview reload the chips of a running job show its own values instead of the defaults. A job's snapshot keeps its `input_dir` and `effective_settings` only from the snapshot `run_job` began for that same job, never from an earlier job's.
- The preset menu holds no built-in copy of the catalog: it applies the profiles `get_preset_profiles` returned, and a click before they arrive does nothing. A Remux card for a FLAC track reads `→ Opus` without a bitrate when the bitrate control has no value, instead of assuming 192k.

### Queue Selection Freeze

- Every Queue Selection mutation is refused for the whole preflight and run window — bulk Select All / Deselect All, per-card checkboxes, and reorder controls all run off one predicate, enforced at the mutation site rather than by the `disabled` attribute. Controls re-enable when the coordinator releases the job.
- If a previously missing selected episode reappears during the forced inspection, the job is not started and the queue logs `Queue selection changed during inspection; previously missing selected episodes reappeared`. Previously a vanished selection aborted with a single message; the reappearance case is now distinguished so the user knows the queue needs review rather than repair.

### Cover Art Matching

- A bare-stem cover (`<video-stem>.<ext>`) must be an exact sibling of the video, so a root cover no longer attaches to a nested episode with the same stem. A bare-stem cover inside `--assets-dir` only matches a video in that same directory.

### Selected Item Validation

- If any selected item is unmatched or is not an MKV file, the entire request now fails instead of processing only the valid subset.
- If two discovered MKV files resolve to the same canonical item ID, the batch now fails instead of running. This applies with or without an explicit selection, because a shared ID makes item identity ambiguous.

### CLI Mode Selection

- Explicit `--remux` and `--extract` flags override mode defaults inferred from the executable name. `mkv-cli` continues to default to remux mode.

### Exact Track Assignment Behavior

- A `TrackAssignment` applies only to the video whose root-relative ID matches `target_video`. The bare-filename fallback is removed, so an assignment for root `ep.mkv` no longer also applies to `Season 01/ep.mkv`.

### Font Handling Behavior

- **Extract Mode**: Always retains extracted font attachments as user output in a `fonts/` directory under the destination directory (or in `--fonts-dir` when given).
- **Remux Mode**: Discovers and embeds fonts into output Matroska files as container attachments.
- **Transactional Staging**: Fonts are staged in `<fonts_dir>/.font_staging/`, which `FontRegistry::cleanup_staging()` removes once it is empty. BLAKE2b content-hash deduplication is preserved without modifying or deleting pre-existing or user-supplied font files.

### Subtitle Resolution Precedence Ladder

Subtitle candidate selection follows a strict five-tier precedence hierarchy where source precedence strictly supersedes cross-source language scoring:

1. **Manual `TrackAssignment`**: Explicit per-episode track assignment. Fails fast during preflight validation if the asset is missing or invalid.
2. **Same-Directory External Subtitle**: Exact external subtitle residing in the same directory as the MKV file matching the episode identity.
3. **Embedded Subtitle Track**: Best embedded subtitle track inside the source MKV container.
4. **Nearest Ancestor External Subtitle**: External subtitle matching the episode identity found in the nearest parent directory.
5. **Explicit `--assets-dir` Fallback**: External subtitle resolved from the explicit fallback assets directory.

*Rule*: A local same-directory external subtitle takes precedence over an embedded track regardless of language score. An embedded subtitle track strictly takes precedence over ancestor external subtitles (e.g., a parent English subtitle will not displace an embedded Indonesian track).

### Cover Art Precedence & Naming Conventions

Cover art resolution evaluates directory hierarchy before candidate kind, following strict deterministic ranking:

- **Directory-Wide Covers**: Strictly `cover.<ext>` and `folder.<ext>`.
- **Episode-Specific Covers**: `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, and exact adjacent `<video-stem>.<ext>`. An inapplicable `*_cover` image is never classified as a directory-wide cover.
- **Directory Hierarchy**: The nearest applicable directory wins before cover kind specificity.
- **Intra-Directory Ranking**:
  1. Episode `_cover` (`<video-stem>_cover.<ext>`)
  2. Episode `.cover` (`<video-stem>.cover.<ext>`)
  3. Exact adjacent bare stem (`<video-stem>.<ext>`)
  4. Directory `cover` (`cover.<ext>`)
  5. Directory `folder` (`folder.<ext>`)
- **Tie-Breaking**: Equal-tier candidates break ties by natural filename order followed by full filename.
- **Source Hierarchy**: Local or ancestor external covers supersede embedded container artwork. Explicit `--assets-dir` fallback covers apply only when no embedded artwork is present in the source file.
