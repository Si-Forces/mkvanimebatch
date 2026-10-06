# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

## Commands

The fmt/clippy/test gate commands are listed in AGENTS.md.

```bash
# One package / one integration suite / one test
cargo test -p mkv-core
cargo test -p mkv-core --test test_assets
cargo test -p mkv-core --test test_domain <test_name_substring>

# Real-media regression: drives real ffmpeg/mkvmerge/mkvextract and skips when they are missing.
# CI on Linux sets MKV_REQUIRE_MEDIA_TOOLS=1 so a missing tool fails the test instead.
MKV_REQUIRE_MEDIA_TOOLS=1 cargo test -p mkv-core --test test_real_remux

# Real-media bench: local only, see its header. It runs its self-check, then skips when
# MKV_BENCH_CORPUS is unset; CI on Linux runs this, which `cargo test --workspace` (and pre-push) does not build.
cargo test -p mkv-core --bench bench_real_media

# One frontend suite (Node 20+, node:test, no package.json/npm)
node --test desktop-tauri/ui/queue-state.test.js

# Builds
cargo build --release -p mkv-cli                  # CLI
cargo build --release -p mkv-anime-batch          # desktop crate (no bundle)
(cd desktop-tauri && cargo tauri build)           # desktop app; needs `cargo install tauri-cli --version "^2" --locked`

# CLI smoke test used by CI
./target/release/mkv-cli --plan --no-cache --input-dir <empty-dir>   # emits manifest {"version": 2, "items": []}
```

`cargo audit` also runs in CI. Ignored advisories, each with its reason, are in `.cargo/audit.toml`.

### Git hooks and the review ledger

Enable the tracked hooks once per clone with `scripts/install-git-hooks.sh`, which sets `core.hooksPath` to `scripts/hooks` (worktrees share it):

- pre-commit runs workspace fmt and clippy when Rust or Cargo files are staged, and `node --check` on staged UI files. It refuses to run cargo while any of those files differs from the index, so the check matches the commit.
- pre-push runs the **review-gate ledger** check on every pushed ref, refusing the push if any pushed commit has no recorded review. When HEAD is one of the pushed tips, it then runs `cargo test` and `node --test`, refusing if the working tree differs from HEAD, untracked Rust, Cargo or UI files included (skip them with `SKIP_SLOW_GATES=1`). Other pushed tips, such as a second branch or an older tag, get only a warning. If HEAD is not pushed at all, that warning is the whole local test coverage.

Both dirty-tree checks also refuse while an assume-unchanged or skip-worktree file is missing or differs from the index, since git diff cannot see that. A flagged file that still matches the index passes. The refusal says how to fix it. To bypass, use `git commit --no-verify`, which skips all of pre-commit, or `SKIP_SLOW_GATES=1`. In a sparse checkout the out-of-cone files are missing, so pre-push always refuses, and pre-commit refuses when any of them are Rust or Cargo files. Those refusals hold only while `scripts/` is in the cone: outside it the hooks are missing too, and git runs no gate at all.

The hooks run straight from the working tree, so an uncommitted edit to `scripts/hooks/` or `scripts/review-gate.sh` changes the gates at once.

After running the code-review skill, record the review:

```bash
scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"
scripts/review-gate.sh status    # commits since origin/main that still lack a review
```

The ledger keys each commit on the `git patch-id` of its diff, so a review survives a rebase or amend that leaves the diff unchanged. Repo scripts use `node`, not Python: ADR-0010 removed Python from dev and CI environments.

## Architecture

A Cargo workspace with three members that share one version:

- `crates/mkv-core`: the media engine library, which holds all the domain logic.
- `crates/mkv-cli`: a thin CLI adapter (`args.rs` parses the flags, `main.rs` runs the batch or `--plan`). If the binary is renamed to `mkv-extract`, it defaults to extract mode.
- `desktop-tauri/src-tauri` (package `mkv-anime-batch`): a Tauri v2 host that runs `mkv-core` **in-process** (no sidecar, ADR-0007), with a plain-ES-module frontend in `desktop-tauri/ui` (no framework, no bundler).

Media Tools (`mkvmerge`, `mkvextract`, and `ffmpeg` with libopus) run as child processes. Every run goes through `media_tool.rs`: the `MediaToolRunner` port, `SubprocessRunner` as its adapter, and one exit-code policy per tool family (exit 1 is success with warnings for MKVToolNix, a failure for `ffmpeg`). Tests script the port with `ScriptedRunner` in `tests/common`.

### Engine flow (`mkv-core`)

`BatchRequest` (`request.rs`, resolved into a `PipelineConfig` by `BatchRequest::resolve`) → `BatchPipeline` (`pipeline.rs`):

1. Discovery (`scan.rs`).
2. Selected-items filtering.
3. Probing. A native EBML parser (`ebml.rs`) runs first and falls back to `mkvmerge -J`. Results are cached in `PersistentInspectCache`, keyed by path plus size and mtime. Bulk probing, for both Inspect and remux runs, goes through `inspect.rs` (`inspect_videos`: one cancellable, cache-backed fan-out).
4. Per-directory asset resolution (`assets.rs`).
5. Planning (remux mode: `RemuxPlanner` in `remux_plan.rs` decides one Remux Plan per item; `pipeline.rs` reports it).
6. Execution through a tokio `JoinSet` of planned jobs drawn from one global queue. Remux execution is in `remux.rs` and extraction in `extract.rs`.

Concurrency has two limits (ADR-0011):

- `workers` caps how many items are in flight.
- The `io_jobs` semaphore covers only the mkvmerge/mkvextract and promotion phases, so FLAC→Opus transcoding overlaps with other items' muxing.

Invariants to preserve:

- **Subtitle precedence ladder (ADR-0004/0008)**: manual `TrackAssignment` > same-directory external > embedded > nearest-ancestor external > `--assets-dir`. Source tier beats language score. The scoring rules live in `domain.rs`.
- **Atomic outputs**: work is staged in `TempDir`s inside the destination and moved into place with `promote_file`. `SubprocessRunner` spawns child processes with `kill_on_drop(true)`, so cancellation (`CancellationToken`, raced with `tokio::select!`) cleans up by dropping.
- **Font registry (`fonts.rs`)**: claim → stage → BLAKE2b dedup → promote/release. `FontClaimsGuard` releases any unpromoted claim on drop.
- **Panics are isolated**: a panicking item task becomes an `error` outcome, and the desktop supervisor catches pipeline panics at the `JoinHandle`. **Never set `panic = "abort"`** in the release profile.
- `unsafe_code = "forbid"` is set across the workspace.

### Engine ↔ desktop bridge

`commands.rs` and `process.rs` define the Tauri commands (`inspect_directory`, `resolve_batch_request`, `run_job`, `stop_job`, `get_default_settings`, `get_job_snapshot`, …). The engine streams `JobEvent`s and log lines over two tokio mpsc channels. A supervisor task forwards them to the webview as `job-event`, `job-log` (batched), and `job-done`. It also reduces the events into a `JobSnapshot`, which carries the job's Effective Settings, so a webview reload can rebuild the queue and the worker chips.

Only one job runs at a time. Stopping is two-phase: a cooperative cancel with a 2 s wait, then an abort of the pipeline task.

Directory identities that cross the bridge must use the display form of the canonical path, with the `\\?\` prefix stripped (`display_canonical_path`). The Queue Tracker matches directories by string comparison, so a mismatch silently breaks selection and ordering.

On the frontend:

- `queue-state.js` holds a pure reducer that applies `JobEvent`s to queue state.
- `queue-view.js` renders the cards.
- `job-controller.js` builds the `BatchRequest` and starts and stops runs.
- The coordinators allow one active job across the Remux and Extract views, and deduplicate in-flight inspections.

Linux-only env defaults are set in `lib.rs` (`init_linux_env`, ADR-0015). The WebKitGTK DMABUF workaround is scoped to the AppImage on purpose. Don't extend it to deb/rpm.

## Domain docs

Before working in an area, read `CONTEXT-MAP.md` → `crates/mkv-core/CONTEXT.md` / `desktop-tauri/CONTEXT.md` (the glossary; use its terms) and the relevant ADRs in `docs/adr/`. If a change contradicts an ADR, say so explicitly; don't override it silently. `architecture.md` is the detailed system spec. `README.md` has the full CLI options table.
