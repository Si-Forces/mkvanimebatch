# 0008. Typed Engine Bridge, Preset Profiles, and Track Assignment Domain

> **Status**: Accepted (Extended by ADR-0011, ADR-0012; amended by ADR-0013; amended 2026-09-24, 2026-09-26, 2026-09-28 and 2026-10-06, implemented)
> **Date**: 2026-05-15
> **Context**: Media Engine Context & Desktop Application Bridge
> **Amends**: ADR-0004, ADR-0007

## Context
Following the in-process Engine Bridge cutover (ADR-0007), the desktop integration boundary retained legacy string-based CLI argument passing (`run_job(mode, args, assignments_json)`), hardcoded JavaScript presets in the webview, and an untyped flat map for subtitle assignments. Furthermore, ADR-0004 permitted stale manual subtitle assignments to silently fall back to automatic track matching, which risk producing unintended batch remux outputs after lengthy encoding runs.

## Decision
We decided to establish a strongly-typed domain contract across the Media Engine and Desktop Application contexts:
1. **Canonical `BatchRequest` Contract**: `mkv-core` defines `BatchRequest` as the single serializable execution contract consumed directly by Tauri's `run_job` and constructed by `mkv-cli` from CLI flags. Preflight validation (`BatchRequest::validate`) runs synchronously before acquiring desktop `JobSlot` locks, returning immediate errors for invalid directories, worker limits, or unparseable bitrates.
2. **Domain-Driven `PresetProfile`**: Encoding presets (`Balanced`, `Fast`, `HiFi`, `Custom`) are modeled in `mkv-core` containing audio transcode parameters (`bitrate`, `complexity`, `keep_fonts` — *historical; superseded by 2026-09-24 amendment*) and exposed to desktop UI via `get_preset_profiles()`. Hardware concurrency (`workers`, `io_jobs`) is strictly decoupled from presets and determined by system hardware detection. Explicit parameters on `BatchRequest` layer over preset baselines.
3. **`TrackAssignment` Domain Entity & Fail-Fast Preflight**: Manual track overrides are modeled as structured `TrackAssignment` entities keyed by relative video path (`Season 1/S01E01.mkv`). While the `TrackAssignment` entity structure anticipates multi-track stacking (e.g. dialogue and signs/songs), the current execution engine enforces 1:1 subtitle overrides (`track_type: "subtitle"`) until multi-track muxing is implemented in `remux.rs`. If an assigned asset or target video is missing on disk at preflight, or if path traversal or multiple assignments targeting the same video are detected, the engine fails fast and rejects the batch immediately (amending Section 1 of ADR-0004).
4. **Real-Time `JobSnapshot` Projection**: `JobState` in the desktop engine bridge reduces incoming `JobEvent` streams into an in-memory `JobSnapshot`, queryable via `get_job_snapshot()` to restore queue presentation across UI reloads until superseded by a new scan or job run.

## Considered Options
- **Retain String CLI Argument Passing (`run_job(mode, args, ...)`**): *Rejected*: Preserves fragile DOM-to-string serialization and double-parsing logic across the boundary.
- **Silent Fallback on Missing Assigned Subtitles (ADR-0004 Status Quo)**: *Rejected*: Silently ignoring explicit user track assignments wastes CPU and produces unexpected remuxes.
- **Packaging Worker Counts into Presets**: *Rejected*: Concurrency is a machine resource property, whereas presets represent release quality targets.

## Consequences
- Amends ADR-0004 Section 1: explicit assignments that cannot be resolved on disk now fail preflight instead of silently falling back.
- Supersedes ADR-0007 Considered Option ("Typed Frontend IPC Overhaul").
- Guarantees complete schema parity between GUI invocations and headless CLI batch scripts.
- Eliminates DOM string-splitting overhead in `job-controller.js`.

## Amendment — 2026-09-24

`keep_fonts` in Decision 2 was an inert setting, not a font-deletion policy.
Remove it from presets, requests, pipeline configuration, the CLI flag, and
the desktop UI in one documented breaking cutover. Extracted fonts remain
user output; do not delete pre-existing or user-supplied fonts. Implementing
a deletion policy instead was rejected because it would risk user data.

Decision 3 also applies at resolution time: if a Track Assignment disappears
after validation, abort the entire batch before media output. The affected
and other queued items end as `error` with distinct reasons, followed by one
failed batch completion and snapshot. Never select an automatic subtitle or
apply the missing assignment's language, title, or offset to another track.

Decision 4 restores an Active Batch across reloads, not every item from the
last Inspection Manifest. A Job Snapshot contains only participating items
in submitted order, their latest progress, and their outcomes; the restored
controller retains the active job ID and Stop capability. Unselected items
are not silently re-inspected during an active job.

## Amendment — 2026-09-26

Decision 3's fail-fast rule has **two** failure points. The 2026-09-24 amendment above recorded only the second. Both are recorded together here; that earlier text stands unedited.

- **Before `BatchStart` — a validation failure.** An assignment target that matches no discovered item fails the run outright. It settles **no** items and emits **no** `BatchComplete`. The run returns an error, writes no media, and the Engine Bridge supervisor settles the job exactly once as failed with an empty snapshot.
- **After `BatchStart` — a planning failure.** An assignment whose asset disappeared between validation and resolution settles the affected item and every remaining queued item as `error` with distinct reasons, followed by exactly one failed `BatchComplete`.

Both fail closed, and neither may ever substitute an automatically selected subtitle. Only the second produces per-item outcomes, because before `BatchStart` there is no run to report on.

This rule previously appeared in exactly one place in the repository — the Media Engine `CONTEXT.md` glossary — and in neither this ADR nor `architecture.md`, despite belonging here. It is now stated in `architecture.md` §8.1 as well, with this ADR as canonical owner.

## Amendment — 2026-09-28

- **Terminal event before `BatchStart`.** "Emits no `BatchComplete`" in the 2026-09-26 amendment describes the engine's event channel. The Engine Bridge supervisor still settles such a job by emitting its own `BatchComplete` with `success: false` and the run's error, then `job-done` with code `1`. The UI therefore always receives exactly one terminal event per job.
- **Preset payload.** `PresetProfile` carries `id`, `name`, `icon`, `bitrate`, and `complexity`; `get_preset_profiles()` returns exactly those fields. The unused `description` field was removed.

## Amendment — 2026-10-06

- **`BatchRequest::resolve` replaces `validate`.** Decision 1's preflight validation, `BatchRequest::validate`, is now private. `BatchRequest::resolve` runs it and then the conversion to a `PipelineConfig` (formerly `into_pipeline_config`, removed) as one synchronous step: validation, Preset Profile layering, and defaulting. It is the only production path from a request to a config. It still runs before the desktop `JobSlot` is reserved, and the first failure still wins. The decision itself stands.
- **Defaults are resolved there.** An unset bitrate and complexity come from the `balanced` Preset Profile (`DEFAULT_PRESET_ID`), not from `PipelineConfig::default`, which stays a fixed fixture for tests. An unset `workers` is `default_workers` and an unset `ffmpeg_threads` is the governor's value for those workers. An unset `io_jobs` resolves to 2 and an explicit `0` to the resolved worker count, so `PipelineConfig.io_jobs` is never 0 and the pipeline no longer reinterprets it. The ranges and the `io_jobs` default are named constants (`WORKERS_RANGE`, `IO_JOBS_RANGE`, `FFMPEG_THREADS_RANGE`, `COMPLEXITY_RANGE`, `DEFAULT_IO_JOBS`) shared by validation, its error messages, and the CLI help.
- **Single check for preset and bitrate.** The CLI no longer rejects `--preset` or `--bitrate` while parsing; `resolve` reports both for the CLI and the desktop alike. The bitrate error now reads `Invalid bitrate format: '<value>'. Must be a positive integer with an optional k/K/M suffix, like 128k or 192k.`
- **Effective Settings.** `EffectiveSettings` (`workers`, `io_jobs`, `ffmpeg_threads`, `bitrate`, `complexity`) is the serializable value a resolved config runs with, returned by `PipelineConfig::effective_settings`. `EffectiveSettings::defaults()` is that of a request that sets nothing, computed through the same code as `resolve`, so a client can show the defaults without a second copy of the rules.
- **Superseded names.** This replaces `BatchRequest::validate` and `BatchRequest::into_pipeline_config` wherever ADR-0007, ADR-0011, ADR-0013 and ADR-0014 name them, including ADR-0013's `test_into_pipeline_config_default_workers_hardware_concurrency`, now `test_resolve_default_workers_hardware_concurrency`. It also ends ADR-0007's statement that the CLI checks `--preset` and `--bitrate` while parsing, and ADR-0007's 2026-10-04 statement that `BatchPipeline::from_request` remains the CLI's entry point: `from_request` is removed, and the CLI resolves once and calls `BatchPipeline::new`, as the desktop does.

## Amendment — 2026-10-06 (desktop bridge)

- **Decision 4 carries the Effective Settings.** `JobSnapshot` gains `effective_settings`, the
  `EffectiveSettings` of the job's resolved config. `run_job` sets it once, when it begins the
  snapshot, and it is serialized to the webview with the rest. No `JobEvent` changes it: the
  `BatchStart` update keeps the settings of its own job and does not inherit another job's. It is
  `null` only for a snapshot that no `run_job` began. A restored controller shows a running job's
  worker and I/O job chips from it, so a reload no longer swaps the job's values for the defaults.
- **Resolution before the scan.** The desktop adds `resolve_batch_request(request)`: it
  canonicalizes `input_dir` with the helper `run_job` uses, calls `BatchRequest::resolve`, and returns
  the Effective Settings or the first error. It is synchronous and reserves no `JobSlot`, so
  Decision 1's ordering stands. The webview calls it with the request it built, before the forced
  preflight scan, while `selected_items` and `assignments` are still unset; `run_job` resolves the
  complete request after the scan. The webview's own concurrency range check
  (`validateConcurrency`) is removed, so the ranges have one copy, in `mkv-core`. An out-of-range
  whole number is refused with the engine's message. `workers`, `io_jobs` and `ffmpeg_threads` are
  `usize` on the wire, so a fractional or negative entry never reaches `resolve`: it fails as the
  command's arguments are deserialized, and the log shows that error text
  (`test_concurrency_fields_are_whole_non_negative_numbers`). A refusal ends the start through
  `abortStart`, like the other failures before the scan, so the status pill reads "failed": the
  webview holds the Run coordinator across the call to keep a second Run out, which the old
  synchronous check did not need. As that check did, it appends the error to the log without
  clearing it; the log is cleared only once the request is accepted.
- **Defaults.** `get_default_settings` returns `EffectiveSettings::defaults()` and replaces
  `get_default_workers` (ADR-0013, Amendment 11).
- **Preset catalog.** Decision 2 exposes the catalog through `get_preset_profiles()`, but the webview
  kept a built-in copy as a fallback. It is deleted: the catalog comes only from
  `get_preset_profiles`, and until it has loaded a preset click in the menu does nothing. The
  desktop still sends the form's explicit bitrate and complexity; the preset id remains a label.
