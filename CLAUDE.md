# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

## Commands

The fmt/clippy/test gate commands are listed in AGENTS.md.

```bash
# One package / one integration suite / one test
cargo test -p mkv-desktop-core
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
node --test desktop-tauri/ui/queue-view.test.js

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

- commit-msg refuses a message whose trailer block credits an AI agent (Claude, Copilot, `AI Bot`). It parses trailers with `git interpret-trailers`, so only real trailers count: a prose mention or a `git commit -v` context line naming one is not blocked, which keeps the commit that documents this hook committable. A co-author naming a person, including the maintainer's own `sifahaz <...@users.noreply.github.com>` line, is left alone. It refuses rather than rewriting the message, so dropping the attribution is visible.
- pre-commit runs workspace fmt and clippy when Rust or Cargo files are staged, and `node --check` on staged UI files. It refuses to run cargo while any of those files differs from the index, so the check matches the commit.
- pre-push runs the **review-gate ledger** check on every pushed ref, refusing the push if any pushed commit has no recorded review (skip it with `SKIP_REVIEW_GATE=1`). When HEAD is one of the pushed tips, it then runs `cargo test` and `node --test`, refusing if the working tree differs from HEAD, untracked Rust, Cargo or UI files included (skip them with `SKIP_SLOW_GATES=1`). Other pushed tips, such as a second branch or an older tag, get only a warning. If HEAD is not pushed at all, that warning is the whole local test coverage.

The two dirty-tree checks in pre-commit and pre-push also refuse while an assume-unchanged or skip-worktree file is missing or differs from the index, since git diff cannot see that. A flagged file that still matches the index passes. The refusal says how to fix it. To bypass, use `git commit --no-verify`, which skips all of commit-msg and pre-commit, or `SKIP_SLOW_GATES=1`. In a sparse checkout the out-of-cone files are missing, so pre-push always refuses, and pre-commit refuses when any of them are Rust or Cargo files. Those refusals hold only while `scripts/` is in the cone: outside it the hooks are missing too, and git runs no gate at all.

The hooks run straight from the working tree, so an uncommitted edit to `scripts/hooks/` or `scripts/review-gate.sh` changes the gates at once.

After running the code-review skill, record the review:

```bash
scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"
scripts/review-gate.sh status    # commits since origin/main that still lack a review
```

The ledger keys each commit on the `git patch-id` of its diff, so a review survives a rebase or amend that leaves the diff unchanged. Repo scripts use `node`, not Python: ADR-0010 removed Python from dev and CI environments.

## Architecture

A Cargo workspace with four members that share one version:

- `crates/mkv-core`: the media engine library, which holds all the domain logic.
- `crates/mkv-cli`: a thin CLI adapter (`args.rs` parses the flags, `main.rs` runs the batch or `--plan`). If the binary is renamed to `mkv-extract`, it defaults to extract mode.
- `crates/mkv-desktop-core`: the toolkit-agnostic Desktop Core (ADR-0016), which holds the Engine Bridge and depends on no UI toolkit.
- `desktop-tauri/src-tauri` (package `mkv-anime-batch`): a Tauri v2 host that runs `mkv-core` **in-process** through `mkv-desktop-core` (no sidecar, ADR-0007), with a plain-ES-module frontend in `desktop-tauri/ui` (no framework, no bundler).

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

The bridge lives in `crates/mkv-desktop-core`: the active job in `job.rs` (`JobState`: the Job Slot, the supervisor, the two-phase stop), the `JobSnapshot` and its reducer in `snapshot.rs`, the Desktop Core actor in `actor.rs` (the Intents, the View State), each mode's Settings Draft in `draft.rs`, each mode's queue (Inspection Manifest, Queue Selection, Track Assignments, rows) in `queue.rs`, the Inspection Coordinator in `inspection.rs`, and the run, stop and shutdown logic in `control.rs`. Its tasks run on a tokio `Handle` the adapter passes to `JobState::new` and `Core::spawn`. In `desktop-tauri/src-tauri/src`, `commands.rs` and `process.rs` define the thin Tauri commands that call it (`dispatch`, `get_state`, `resolve_batch_request`, `run_job`, `stop_job`, `clear_cache`, …), and `publisher.rs` holds the `AppHandle` `Publisher`. The engine streams `JobEvent`s and log lines over two tokio mpsc channels. A supervisor task forwards them through a `JobSink`, which in the app is the actor's `ActorSink` and in tests records them with no Tauri app. The supervisor reduces the events into its own private `JobSnapshot`, the settlement ledger it settles the job and computes `job-done` from, so teardown never depends on the actor. The actor reduces the same events with the same `JobSnapshot::apply` into the published snapshot, which carries the job's Effective Settings. It owns each mode's queue, scan and Settings Draft (the form's values as typed), applies the Intents `dispatch` sends (a scan, a selection, a reorder, a Track Assignment, a draft edit, a Preset Profile), and publishes the whole View State (`{seq, job, defaults, presets, remux, extract}`) as `view-state` at most every 66 ms, at once before a `dispatch` reply except to a draft Intent (coalesced by that cap), and before `job-done`. It forwards `job-log` (batched) at once. It rebuilds only the ModeView of a mode that changed. A webview reload calls `get_state`, so it loses only the log (S4).

Only one job runs at a time. Stopping is two-phase: a cooperative cancel with a 2 s wait, then an abort of the pipeline task.

Directory Identities that cross the bridge must use the display form of the canonical path, with the `\\?\` prefix stripped (`display_canonical_path`). That covers the identity a `ScanDirectory` Intent resolves the typed path to (`inspection::canonicalize_directory`), as well as `canonical_dir`, a ModeView's `directory`, the Job Snapshot's `input_dir`, and `BatchStart.input_dir`. The Desktop Core's queue and the JS preflight compare directories byte for byte and never normalize a path, so a mismatch silently breaks selection and ordering.

On the frontend:

- `app.js` applies each `view-state` (dropping one whose `seq` is not newer) and boots from `get_state`.
- `queue-view.js` renders the published rows and turns selection and reorder clicks into Intents; it holds no queue rules. The Inspector in `app.js` sends the Track Assignment Intents (`assign_track`, `clear_track`).
- `directory-scan.js` sends the `ScanDirectory` Intent.
- `settings-draft.js` mirrors each mode's Settings Draft: a field's edit sends `edit_draft`, and a published draft is written back into every field without focus. `preset-menu.js` renders the published `presets` and sends `set_preset`; `settings-chips.js` reads the published `defaults`. Every Intent goes through one promise chain in `app.js`, so the core gets them in the order sent; the chain waits for a `scan_directory` to be sent, not answered, since its answer waits for the inspection.
- `job-controller.js` builds the `BatchRequest`, guards the selection through preflight, starts and stops runs, and adopts a job a reload finds.
- `job-coordinator.js` allows one active job across the Remux and Extract views.

Linux-only env defaults are set in `lib.rs` (`init_linux_env`, ADR-0015). The WebKitGTK DMABUF workaround is scoped to the AppImage on an NVIDIA driver on purpose. Don't extend it to deb/rpm.

## Domain docs

Before working in an area, read `CONTEXT-MAP.md` → `crates/mkv-core/CONTEXT.md` / `desktop-tauri/CONTEXT.md` (the glossary; use its terms) and the relevant ADRs in `docs/adr/`. If a change contradicts an ADR, say so explicitly; don't override it silently. `architecture.md` is the detailed system spec. `README.md` has the full CLI options table.
