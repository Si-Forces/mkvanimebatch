# Release Notes

## Unreleased

### Breaking Changes

- **Removal of `--keep-fonts` CLI Flag**: The `--keep-fonts` command-line option has been removed from `mkv-cli`. Invoking `mkv-cli` with `--keep-fonts` will now result in an unknown flag error.
- **Removal of `keep_fonts` from Typed Contracts & IPC**: The `keep_fonts` field has been removed from `BatchRequest`, `PresetProfile`, `PipelineConfig`, and desktop Tauri IPC command schemas (`run_job`, `get_preset_profiles`). Desktop presets and UI controls no longer expose a font-retention toggle. No compatibility alias is retained.
- **Public `mkv-core` API removals**: Removed `SubIndex::{lookup, indexed_count, primary_count, fallback_count}`, `ChapterIndex::{indexed_count, rejected_count}`, `CoverIndex::{indexed_count, has_global}`, and `AssetResolver::{has_local_subtitle, count_unmatched_chapter_videos}`. Downstream code calling these helpers must migrate; the methods are no longer available.
- **Release profile no longer aborts on panic**: `[profile.release]` no longer sets `panic = "abort"`. This is what lets the Engine Bridge intercept a task panic, emit a terminal failure event, and release the job slot instead of the whole desktop process dying. Expect a larger packaged binary; crash containment takes priority over size.
- **`BatchStart` / `JobSnapshot` `input_dir` is now the display path form**: On Windows these fields no longer carry a verbatim (`\\?\`) prefix, and `\\?\UNC\` is rewritten to `\\`. Consumers that compare the path against a raw `std::fs::canonicalize` result must strip the prefix themselves. This matches the form the Inspection Manifest's `canonical_dir` already used; see *Canonical Directory Identity* in `architecture.md` §5.1.

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

- **Extract Mode**: Always retains extracted font attachments in user output within the destination directory.
- **Remux Mode**: Discovers and embeds fonts into output Matroska files as container attachments.
- **Transactional Staging**: Scratch directories (`.font_staging/`) are transactionally cleaned up via `FontRegistry::cleanup_staging()`, and BLAKE2b content-hash deduplication is preserved without modifying or deleting pre-existing or user-supplied font directories.

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
