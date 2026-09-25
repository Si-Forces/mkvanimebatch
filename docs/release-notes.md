# Release Notes

## Phase C: Media Precedence & Font Configuration Cutover (S06–S08)

### Breaking Changes

- **Removal of `--keep-fonts` CLI Flag**: The `--keep-fonts` command-line option has been removed from `mkv-cli`. Invoking `mkv-cli` with `--keep-fonts` will now result in an unknown flag error.
- **Removal of `keep_fonts` from Typed Contracts & IPC**: The `keep_fonts` field has been removed from `BatchRequest`, `PresetProfile`, `PipelineConfig`, and desktop Tauri IPC command schemas (`run_job`, `get_preset_profiles`). Desktop presets and UI controls no longer expose a font-retention toggle. No compatibility alias is retained.
- **Exact Track Assignment Targets**: A `TrackAssignment` applies only to the video whose root-relative ID matches `target_video`. The bare-filename fallback is removed, so an assignment for root `ep.mkv` no longer also applies to `Season 01/ep.mkv`.

### Font Handling Behavior (S08)

- **Extract Mode**: Always retains extracted font attachments in user output within the destination directory.
- **Remux Mode**: Discovers and embeds fonts into output Matroska files as container attachments.
- **Transactional Staging**: Scratch directories (`.font_staging/`) are transactionally cleaned up via `FontRegistry::cleanup_staging()`, and BLAKE2b content-hash deduplication is preserved without modifying or deleting pre-existing or user-supplied font directories.

### Subtitle Resolution Precedence Ladder (S06)

Subtitle candidate selection follows a strict five-tier precedence hierarchy where source precedence strictly supersedes cross-source language scoring:

1. **Manual `TrackAssignment`**: Explicit per-episode track assignment. Fails fast during preflight validation if the asset is missing or invalid.
2. **Same-Directory External Subtitle**: Exact external subtitle residing in the same directory as the MKV file matching the episode identity.
3. **Embedded Subtitle Track**: Best embedded subtitle track inside the source MKV container.
4. **Nearest Ancestor External Subtitle**: External subtitle matching the episode identity found in the nearest parent directory.
5. **Explicit `--assets-dir` Fallback**: External subtitle resolved from the explicit fallback assets directory.

*Rule*: A local same-directory external subtitle takes precedence over an embedded track regardless of language score. An embedded subtitle track strictly takes precedence over ancestor external subtitles (e.g., a parent English subtitle will not displace an embedded Indonesian track).

### Cover Art Precedence & Naming Conventions (S07)

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
