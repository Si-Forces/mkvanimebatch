# 0011. Selected Items Execution Queue, Concurrency Pipelining, and Enriched Track Assignment

> **Status**: Accepted (Amended by ADR-0012, ADR-0013, ADR-0014; amended 2026-09-29)
> **Date**: 2026-08-01
> **Context**: Media Engine Context & Desktop Application Bridge
> **Amends**: ADR-0008

## Context
Following v1.0.0, batch remux execution was strictly directory-bound: `BatchPipeline` partitioned files by parent directory and processed directories sequentially. In multi-directory releases (e.g. `Season 1/`, `Season 2/`, `Specials/`), worker tasks stalled at directory boundaries, and probing within each directory executed synchronously file-by-file. Furthermore, `BatchRequest` offered no mechanism for the desktop `QueueTracker` to exclude episodes or specify custom execution sequences, and manual `TrackAssignment` in the GUI was restricted to clicking pre-discovered subtitle candidates without file browsing or metadata overrides.

## Decision
We decided to establish an explicit execution queue contract, a unified global concurrency pipeline, and enriched track assignment overrides across both bounded contexts:

1. **`Selected Items` Specification**:
   `BatchRequest` gains `selected_items: Option<Vec<String>>`. When present, `mkv-core` filters and sequences batch items strictly matching this slice of relative video paths/IDs. When `None`, the engine executes all discovered files in standard directory order for complete backward compatibility. Preflight validation (`BatchRequest::validate`) strictly enforces fail-fast integrity: path traversal (`..`), absolute paths, non-existent files, empty lists, duplicate items, and track assignments targeting unselected videos are immediately rejected before acquiring execution locks.

2. **Dual-Governor Global Work Pipelining**:
   - **Concurrent Probing**: Preflight video probing within and across release directories is parallelized using a bounded semaphore (`(workers * 2).clamp(2, 16)`), with thread-safe persistence through `PersistentInspectCache`.
   - **Unified Global Queue**: Planned jobs across all release directories are scheduled into a single global `tokio::task::JoinSet` bounded by `workers`, eliminating worker starvation at directory boundaries while preserving per-directory asset discovery.
   - **Dual-Governor Resource Guard**: CPU-intensive Opus audio transcoding (`ffmpeg`) executes within the worker slot, while disk-intensive remux multiplexing (`mkvmerge`) is guarded by `io_semaphore` permits (defaulting to `io_jobs`), maximizing hardware throughput without disk head contention or uncontrolled scratch space accumulation.

3. **Enriched 1:1 Track Assignment GUI**:
   The desktop track assignment modal is upgraded from candidate-only selection to a full override interface exposing arbitrary subtitle file picking via native file dialogs (`rfd`), custom ISO-639-2 language codes, track titles, and millisecond time offsets (`offset_ms`). Multi-track stacking remains deferred per ADR-0008, maintaining 1:1 subtitle override semantics in `remux.rs`.

4. **Desktop Queue Selection & Ordering**:
   `QueueTracker` introduces card-level inclusion checkboxes, header bulk actions (`Select All`, `Deselect All`), dynamic selection count telemetry, and accessible `▲` / `▼` card reordering, transmitting the active selection and sequence to `run_job`.

## Considered Options
- **Decoupled Producer-Consumer Transcode Queue**: *Rejected*: Dedicating separate worker pools for audio transcoding and muxing adds complex buffer management and temporary disk bloat compared to overlapping within Tokio's async worker tasks.
- **Permissive Preflight Fallback for Selected Items**: *Rejected*: Silently dropping missing or duplicated items would mask user configuration errors and contradict ADR-0008's fail-fast guarantees.
- **Full Multi-Track Stacking in this Iteration**: *Rejected*: Multi-track audio and subtitle multiplexing introduces substantial command-line matrix complexity in `mkvmerge_args`; enriched 1:1 subtitle overrides deliver immediate user value (fixing desync and missing tracks) with zero risk to pipeline stability.

## Consequences
- Eliminates CPU and worker starvation across multi-directory release batches.
- Significantly speeds up preflight inspection on large batches via parallel probing.
- Allows desktop users to prioritize, reorder, or selectively remux specific episodes.
- Gives users full control over external subtitle synchronization and metadata tagging.
- Preserves 100% backward compatibility for headless CLI invocations and existing presets.

## Amendment — 2026-09-29 (CLI backward compatibility consequence)

The "Preserves 100% backward compatibility for headless CLI invocations and existing presets"
consequence above no longer holds. The deprecated `--sub-assignments` alias was removed from
`mkv-cli` in the 2026-09-28 cleanup; passing it now fails with
`Unknown argument: --sub-assignments` (`--assignments` still accepts the legacy flat map). ADR-0012
Decision 4 records that removal.

The contract this ADR defines is unchanged: `selected_items`, the unified global queue, the
dual governor, and 1:1 subtitle overrides behave as specified above.
