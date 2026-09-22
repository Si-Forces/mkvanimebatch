# 0014. CLI Selected Items Execution and Manifest Filtering

> **Status**: Accepted
> **Date**: 2026-09-22
> **Context**: Media Engine Context & Standalone CLI Adapter
> **Amends**: ADR-0011

## Context

In [ADR-0011](./0011-selected-items-queue-and-concurrency-pipelining.md), the system introduced `selected_items: Option<Vec<String>>` to `BatchRequest` in the [Media Engine Context](../../crates/mkv-core/CONTEXT.md) and exposed queue selection and card reordering in the Desktop Application's `QueueTracker`. This allowed desktop users to isolate specific episodes, exclude unneeded videos, and dictate custom execution order.

However, the standalone CLI adapter (`crates/mkv-cli`) was left without any mechanism to specify or sequence selected items. CLI invocations always processed the entire set of discovered MKV files in the input directory. Users running headless batch operations, automating CI/CD pipelines, or testing transcode configurations on single episodes were forced to process all files or manually isolate episodes into temporary working directories. Furthermore, running `--plan` in the CLI lacked any capability to inspect or simulate the manifest for a selective batch.

## Decision

We decided to establish first-class selected item execution and manifest filtering across `mkv-cli` and `mkv-core`:

1. **Dual CLI Selection Flags**:
   In `crates/mkv-cli`, introduce two complementary flags:
   - `--selected-item <PATH>`: Repeatable argument representing a single literal file path. It does *not* split on commas, providing unambiguous and deterministic support for anime release filenames containing commas (e.g. `[Group] Show - 01 [1080p, AAC].mkv`).
   - `--selected-items <LIST>`: Repeatable argument accepting a comma-delimited list of file paths, with surrounding whitespace trimmed from each item, designed for convenient scripting and compact terminal invocation.
   - Both flags can be repeated and mixed in a single command invocation; the CLI accumulates them into an ordered list preserving user-defined execution sequence.
   - Selection flags strictly require `--input-dir`; attempting to specify selections without `--input-dir` fails fast with an immediate CLI error.

2. **Path Normalization & CWD Fallback**:
   To ensure ergonomic usage across different working directories:
   - Input paths may be passed relative to `input_dir`, relative to the current working directory (CWD), or as absolute paths.
   - Any path matching or prefixed with `input_dir` is stripped and normalized to a clean relative item path matching the engine's internal item IDs.
   - If a given path does not directly match relative to `input_dir`, but exists as a valid path relative to CWD, it is canonicalized against CWD and verified to reside within `input_dir`.
   - Paths resolving outside `input_dir` are rejected immediately with an explicit fail-fast error: `Selected item '<path>' is outside input directory '<input_dir>'`.

3. **Manifest Filtering on `--plan`**:
   When `--plan` is combined with selection flags:
   - `mkv-core` introduces `InspectionManifest::filter_selected(&self, selected_items: &[String]) -> InspectionManifest` in `manifest.rs`.
   - The method retains only manifest items whose `id` or relative path matches the selected list, reordering them strictly to match the requested selection sequence.
   - Subtitle candidates (`subtitle_candidates`) within the manifest are pruned to include only candidates for releases represented in the filtered item set.
   - In `mkv-cli`, `--plan` applies `manifest.filter_selected(&selected)` before printing the final JSON output to stdout.

## Considered Options

- **Single Comma-Delimited Flag Only (`--selected-items`)**: *Rejected*: Anime fansub release conventions routinely include metadata tags with commas (e.g. `[1080p, x265, FLAC]`, `Show, The - 01.mkv`). Requiring complex shell escaping or backslash quoting leads to subtle parsing errors. Providing literal `--selected-item` alongside comma-splitting `--selected-items` solves this cleanly.
- **Manifest Filtering Exclusively in the CLI Adapter**: *Rejected*: Manifest generation, item identity resolution, and subtitle candidate association are domain invariants belonging in `mkv-core`. Placing `filter_selected` on `InspectionManifest` makes filtering reusable across the engine bridge, CLI, and integration test suites.
- **Silent Dropping of Unmatched or External Paths**: *Rejected*: Violates fail-fast design principles established in ADR-0008 and ADR-0011. Paths outside `input_dir` or pointing to nonexistent files indicate script errors or misconfiguration that must be flagged immediately.

## Consequences

- Complete feature parity between the Desktop Application and the standalone CLI for episode selection and custom execution ordering.
- Robust, escape-free support for arbitrary anime filenames containing commas, brackets, spaces, and Unicode characters.
- Headless scripts and CI runners can generate scoped planning manifests (`--plan`) and execute targeted remux/extract jobs without directory isolation workarounds.
- 100% backward compatibility: omitting selection flags continues to discover and process all directory files in natural directory order.
