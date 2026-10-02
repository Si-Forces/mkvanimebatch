# Release Notes

## Unreleased

### Breaking Changes

- **`JobEvent` and `BatchItemInfo` drop `Deserialize`**: `JobEvent` and `BatchItemInfo` in `mkv-core::pipeline` no longer derive `Deserialize`, and unused `#[serde(default...)]` annotations were removed from `JobEvent` variants.
- **`EpisodeType::label()` parameter dropped**: `EpisodeType::label(&self)` no longer accepts `long: bool`; it always returns the standard short label (`EP`, `Part`, `OVA`, `SP`).
- **Asset font fields and `use_source_fonts()` removed**: Removed write-only `fonts: FontInfo` from `ResolvedAssets` and `AssetResolver`, removed unused `AssetResolver::use_source_fonts()`, and eliminated redundant font fallback scanning in `AssetResolver::discover`, which drops its `fonts_dir` and `font_cache` parameters. Directory-level font resolution is owned by `prepare_release_directory`.
- **`execute_remux_job` requires the I/O governor**: its `io_semaphore` parameter is now `&Semaphore` instead of `Option<&Semaphore>`. A closed semaphore fails the item instead of running it without the ADR-0011 disk-write limit, in both remux and extract.
- **Non-finite subtitle offset rejected**: CLI and batch requests now validate that `--sub-offset` is a finite number, rejecting non-finite values such as `inf`, `-inf`, or `NaN`.
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
  - `remux`: `RemuxSummary` (`execute_remux_job` returns the log lines), `PlannedJob::{base, release_dir}`, `TrackSelection::{audio_channels, audio_reason, has_source_chapters}`.
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
- **`InspectionManifest::filter_selected` replaced by `select_items`**: `select_items(&[String])` returns `Result<InspectionManifest, String>`, failing with `Selected items not found among discovered MKV files: ...` when a selection names no discovered item. It keeps selection order and drops repeated selections.

### Core Scanning

- Recursive MKV scanning skips canonical-key path allocations for non-MKV files and directories excluded by the output/font-directory name filters. Canonical keys are still derived for candidates that need ignore-path or deduplication checks.

### Core Performance & Probing

- The native EBML probe runs on Tokio's blocking pool. Concurrent probes reading cold or slow media (cold-read latency measured on a USB HDD and an SSD) no longer stall the async runtime, which froze progress events and NDJSON output for the CLI and desktop app. Once started, a native probe is not cancellable and reads to completion after a stop.
- `probe_ebml_native` preserves `BufReader` buffers across intra-buffer element skips using relative seeking and tracks stream positions in memory, eliminating redundant kernel `lseek` loop guards and reducing probe I/O from hundreds of syscalls to roughly one buffer read per 8 KiB of header scanned.
- Preflight inspection default probe concurrency is now calculated from physical core count via `cpu::probe_concurrency(cpu::default_workers())`, aligning with pipeline execution's default. Previously it used logical cores (`available_parallelism()`), which allowed up to 16 concurrent probes on machines with 4 physical and 8 logical cores.

### Audio Track Selection

- Tracks named `Comments`, `Commentaries`, and lowercase variants are excluded from primary audio selection so they are not transcoded over feature audio.

### Worker Panic Reporting

- A worker task that panics now completes its item with outcome `error` and the message `worker panicked: ...`. Previously that item never received an `ItemComplete` event.

### Extracted Subtitle Naming

- When a file name does not parse as an episode, its extracted subtitle is named `{stem}_{lang}_track{id}.ass` instead of repeating the stem (`{stem}_{stem}_{lang}_track{id}.ass`). Parsed episodes are unchanged: `{stem}_{EpLabel}{ep}_{lang}_track{id}.ass`.

### Desktop Application

- The theme follows the system light/dark preference, live, until the user toggles it; only an explicit toggle is stored and applied to the native window. Unrecognized stored themes are ignored, consistently with the early theme bootstrap.
- The window's corner resize handles now have a usable size and a diagonal resize cursor.
- Stepper limits match the engine's validation: workers 1–64, Disk I/O jobs 0–64, FFmpeg threads 1–32. Fractional, out-of-range, and non-numeric values in those three fields are rejected before inspection instead of being truncated to integers.
- The UI uses the system font stacks; bundled web fonts are no longer loaded.
- The per-item subtitle offset reads exponent notation correctly (`1e3` is 1000 ms; it was 1 ms). A value beyond the safe-integer range now leaves the global offset in effect instead of failing the run.
- The `set_window_theme` IPC command no longer coerces an unrecognized theme name to Dark. An unrecognized name is now ignored and leaves the native window exactly as it was, including any pin set by an earlier explicit choice. The webview only ever sends `light` or `dark`, so this affects a caller that bypasses the UI.

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
