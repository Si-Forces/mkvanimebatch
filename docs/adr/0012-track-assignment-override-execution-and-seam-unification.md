# 0012. Track Assignment Override Execution and Seam Unification

> **Status**: Accepted (amended 2026-09-26)
> **Date**: 2026-09-20
> **Context**: Media Engine Context & Desktop Application Bridge
> **Amends**: ADR-0008, ADR-0011

## Context

In [ADR-0008](./0008-typed-engine-bridge-and-track-assignment-model.md) and [ADR-0011](./0011-selected-items-queue-and-concurrency-pipelining.md), the system established the `TrackAssignment` domain entity across the [Media Engine Context](../../crates/mkv-core/CONTEXT.md) and [Desktop Application Context](../../desktop-tauri/CONTEXT.md), and upgraded the desktop track assignment modal to capture rich override attributes including millisecond time synchronization offsets (`offset_ms`), custom ISO-639-2 language codes (`language`), and descriptive track labels (`title`).

However, the internal pipeline flattened them to `sub_assignments: HashMap<String, PathBuf>`, dropping these attributes (`offset_ms`, `language`, `title`). Consequently, the downstream remux executor only received the subtitle file path and fell back to global defaults (`sub_offset`, default language, and default label). Furthermore, the standalone CLI retained a legacy `--sub-assignments` flag alongside `--assignments`, exposing an inconsistent parameter seam across execution interfaces.

## Decision

We decided to eliminate `sub_assignments` consumption in `pipeline.rs` and unify the track assignment execution seam across both bounded contexts:

1. **Eliminate `sub_assignments` Consumption in `pipeline.rs`**:
   The `Batch Pipeline` (`pipeline.rs`) discontinues flattening assignments into `HashMap<String, PathBuf>`. Instead, the full `TrackAssignment` entities from `BatchRequest::assignments` are indexed by canonical item ID / relative video path and carried directly through pipeline planning.

2. **Propagate Full `TrackAssignment` via `PlannedJob`**:
   `PlannedJob` in `remux.rs` directly stores the resolved `TrackAssignment` (or its specific override properties: `custom_offset_ms`, `custom_language`, and `custom_title`) for each target video.

3. **Pass Attributes to `build_mkvmerge_args()` with Explicit Precedence**:
   When constructing command-line arguments for `mkvmerge`:
   - `custom_offset_ms` supersedes global `sub_offset`. When no custom offset is configured on the assignment, the global `sub_offset` is applied.
   - `custom_language` overrides default language for the subtitle track (`job.subtitle_mkv_lang`).
   - `custom_title` overrides default label for the subtitle track (`job.subtitle_label`).

4. **Deprecate CLI `--sub-assignments`**:
   In `mkv-cli`, the legacy `--sub-assignments` flag is formally deprecated in favor of `--assignments` (which accepts full JSON-serialized `Vec<TrackAssignment>` or path mappings parsed via `parse_assignments_json`). Legacy key-value JSON maps continue to be parsed into standard `TrackAssignment` entities with `track_type: "subtitle"`, maintaining backward compatibility while establishing `TrackAssignment` as the sole canonical entity.

## Considered Options

- **Retain `sub_assignments` with Parallel Override Maps**: *Rejected*: Maintaining separate lookup maps for paths, offsets, languages, and titles violates domain encapsulation and risks synchronization defects between the bridge and pipeline.
- **Defer Track Overrides until Multi-Track Stacking**: *Rejected*: Users require per-episode subtitle offset calibration and language tagging immediately; blocking 1:1 override execution on future multi-track multiplexing leaves user-configured GUI values inert.

## Consequences

- 1:1 parity between GUI track override modal, CLI `--assignments`, and `mkvmerge` command execution.
- User-specified time offsets, custom language codes, and track titles are accurately applied to multiplexed files.
- Eliminates lossy intermediate data transformation (`sub_assignments`) at the boundary between `request.rs` and `pipeline.rs`.
- Preserves full backward compatibility for existing scripts passing legacy JSON maps through `parse_assignments_json`.

## Amendment — 2026-09-26

Decision 3 established that `custom_offset_ms` supersedes the global `sub_offset`, but left two things unspecified: what an **explicit zero** means, and what **unit** each offset is expressed in. Both are now fixed.

- **Override, never additive.** A per-assignment offset replaces the global one; the two never sum. The global applies only when the assignment carries no offset at all.
- **`offset_ms = 0` means explicitly unshifted.** Zero is a *value*, not an absence. It suppresses the global offset entirely and emits no sync argument for that track. This is the correct semantic for an episode that is already synchronised and must not inherit the batch-wide shift — the most common reason to reach for a per-episode override.
- **Milliseconds everywhere.** Both offsets are integer milliseconds, and the global is renamed `default_sub_offset_ms` so the unit is visible at every call site. The previous split — fractional seconds globally, integer milliseconds per assignment — turned `--sub-offset 0.5` into `500` and invited arithmetic errors at the boundary.

> The `if ms != 0` guard in `build_mkvmerge_args` reads like an off-by-one bug. It is not. The guard is precisely what gives an explicit zero its meaning. Do not "fix" it.

This also closes a reachability gap. The desktop controller normalised a literal `"0"` to `null`, so the engine's explicit-zero capability could not be exercised from the GUI at all — a decision recorded in the Media Engine glossary was unreachable from the only interface most users have. The controller now sends `0`. The unit normalisation, the rename, and the controller change ship together: split apart, they leave a window in which the UI emits a value the engine reads differently than intended.
