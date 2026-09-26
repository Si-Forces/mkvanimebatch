# Merge-Readiness Implementation Record

**Audience:** implementation agent taking over PR #17 / branch `fix/documentation-compliance-contracts`.

**Base:** `origin/main` (`00a67857e5de28061029e6be1354d609cc38bcdf` at review time).

**Implementation head:** working tree based on `12145cd36866635e27cd139399069e082e731ef6`; the six planned slices are now implemented locally.

**Goal:** record the verified merge-readiness fixes and their verification contract without reopening the already-cleared S06 subtitle tie-break concern.

## Current implementation status

The six planned slices below are implemented in the working tree. Local Rust/UI gates, release builds, the required real-media regression, CLI alias smoke, and a Chromium IPC-stub desktop smoke pass. Final hosted PR checks must be rerun after committing/pushing the implementation. Do not treat the earlier green suite as proof of paths that were fixed later.

### Cleared finding: S06 subtitle tie-break

This is a review disposition, not a new media contract. Do **not** change `SubIndex::lookup_ranked` for the alleged equal-candidate tie-break regression. The current tuple ends in `std::cmp::Reverse(idx)` and uses `max_by_key`; therefore `Reverse(0) > Reverse(1)`, preserving the baseline's first-insertion-wins behavior represented by `-idx`. An independent probe against the current compiled `mkv-core` library produced:

```text
A,B -> Show - 01 [A].ass
B,A -> Show - 01 [B].ass
```

The implementation agent should preserve this behavior and avoid adding a speculative tie-break change.


## Non-negotiable contracts

- `BatchRequest` and `TrackAssignment` remain the typed cross-context interface.
- Item identity is the canonical root-relative ID produced by `get_item_id` / `clean_relative_path`; do not reintroduce bare-filename matching.
- A manual assignment that cannot be consumed must fail closed; never fall back to an automatic subtitle or apply its metadata elsewhere.
- Planning failures must settle every selected item and emit exactly one terminal `BatchComplete`; no media output may be written.
- Stop and hard abort settle one job exactly once. Cancellation wins over an incidental worker error after Stop.
- `JobSnapshot` and reload restore the ordered, selected Active Batch only; unselected manifest items are not silently reintroduced.
- Same-directory rescans preserve surviving selection and relative order; newly discovered items are unselected.
- `keep_fonts` remains removed. Do not add a compatibility alias.
- Media precedence remains: adjacent external subtitle → embedded → ancestor external → explicit fallback; cover ranking remains nearest applicable directory, then `_cover` > `.cover` > exact bare stem > `cover` > `folder`.
- No new dependency is needed.

## Dependency order

Implement in this order. Each slice is independently testable; do not batch all tests before all implementation.

### Slice 1 — Make Active Batch selection immutable during preflight

**Why first:** prevents a user action during the asynchronous forced inspection from changing the meaning of the request after IDs were captured.

**Files:**

- `desktop-tauri/ui/job-controller.js`, `JobController.syncGlobalControls` and/or the selection-action registration seam.
- `desktop-tauri/ui/app.js`, the Select All / Deselect All handlers.
- `desktop-tauri/ui/job-controller.test.js`, preferably using the existing `FakeElement` harness and a deferred `scanInputDirectory` promise.

**Implementation:**

1. Treat all queue-selection mutations as unavailable while `coordinator.activeMode !== null`, `coordinator.isStarting`, or the tracker is scanning for the active mode.
2. Disable the mode's bulk controls (`remux-select-all`, `remux-deselect-all`, `extract-select-all`, `extract-deselect-all`) while busy. Keep them disabled until the coordinator releases ownership.
3. Add a defensive guard in the bulk-action handlers themselves; DOM disabling alone is not the invariant. The handlers must return without mutating state when the active coordinator is busy.
4. Do not change the captured-selection algorithm. The request must continue to submit the IDs captured before the forced scan, and the post-scan vanished-ID check remains authoritative.

**Regression test (red first):**

- Arrange a same-directory queue with ordered selected IDs `[02, 01]`.
- Make the forced `scanInputDirectory` return a deferred promise.
- Start the controller, wait until preflight is pending, and attempt Deselect All / Select All through the real handler seam.
- Assert the tracker selection and eventual `run_job` request still use exactly `[02, 01]`; newly discovered `03` remains unselected.
- Assert controls are disabled during the wait and re-enabled after normal completion or abort.

**Acceptance:** no bulk action can mutate Queue Selection after `start()` captures it; no stale or widened batch is submitted.

### Slice 2 — Persist BatchStart order in the queue reducer

**Why second:** the active-reload race currently creates a state where `order` contains the Active Batch but `knownOrder` remains empty; the next same-directory rescan then treats all items as new/unselected.

**Files:**

- `desktop-tauri/ui/queue-state.js`, `reduceQueueEvent` `batch_start` branch.
- `desktop-tauri/ui/queue-state.test.js`.
- `desktop-tauri/ui/queue-view.test.js` for the end-to-end reducer/view path.

**Implementation:**

1. When BatchStart introduces a new participant ID, append it to both `state.order` and `state.knownOrder` if it is not already present.
2. Preserve the submitted BatchStart order exactly. Do not rebuild the order from the manifest or sort it again.
3. Keep `state.batchItemIds` as the participant set used by finalization.
4. While touching this branch, eliminate the repeated linear `state.order.includes` scan. Build a `Set` from the current order, append each new ID once, and update the set. This keeps the empty-snapshot BatchStart path linear rather than O(N²).

**Regression tests:**

- Start from an empty snapshot state, reduce a BatchStart with `[02, 01]`, then call `reconcileQueueSelection` with a manifest containing `[02, 01, 03]`.
- Assert order is `[02, 01, 03]`, `knownOrder` contains `[02, 01, 03]`, `02` and `01` retain their selected state, and `03` is unselected.
- Add the full reload → BatchStart → terminal completion → same-directory rescan sequence at the QueueTracker seam if practical.
- Add a large empty-state BatchStart performance regression with 10,000 `items`; use a generous time bound and assert the state contains all IDs exactly once. Do not use the existing 10,000 start/complete test as a substitute: it pre-populates items and sends no BatchStart item list.

**Acceptance:** an Active Batch submitted after reload remains the ordered/selected base for the next same-directory rescan, and large BatchStart hydration is linear.

### Slice 3 — Give cancellation precedence over worker errors

**Why third:** the supervisor currently converts every `Ok(Err(error))` into a failed terminal, even when Stop has already cancelled the token. That makes the backend snapshot disagree with the frontend's cancelled presentation.

**Files:**

- `desktop-tauri/src-tauri/src/commands.rs`, `start_supervised_job` terminal match.
- `desktop-tauri/src-tauri/src/commands.rs` supervisor tests near `forced_stop_aborts_only_worker_and_settles_once` and `cooperative_stop_ignores_stale_id_and_duplicate_completion`.
- `desktop-tauri/src-tauri/src/process.rs` only if the test exposes a snapshot transition gap; avoid unrelated lifecycle changes.

**Implementation:**

1. In the `Ok(Err(error))` arm, first test `cancel_for_supervisor.is_cancelled()`.
2. If cancellation is set, retain the error in the job log but emit `bridge_terminal(true, None)` rather than a failed terminal.
3. Preserve the existing panic and unexpected-cancellation arms.
4. Keep the existing exactly-once terminal/job-done/slot-release behavior. Do not add a second terminal from the worker error path.

**Regression test:**

- Start a supervised job with a worker that observes the cancellation token and then returns `Err("scan failed after stop")`.
- Assert the snapshot is `Cancelled`, unresolved items are cancelled, exactly one terminal event is emitted with `cancelled: true`, exactly one `job-done` is emitted with the cancellation code, and the error is present in the job log.
- Retain the existing duplicate-completion and stale-job-ID tests.

**Acceptance:** a Stop request cannot leave a cancelled frontend paired with an error backend snapshot.

### Slice 4 — Fail closed for undiscovered assignment targets

**Why fourth:** manual assignments are currently checked only when the planning loop encounters a matching discovered file. An existing target excluded from discovery can be silently ignored and automatic resolution can proceed.

**Files:**

- `crates/mkv-core/src/request.rs`, assignment validation and the canonical target-ID helper.
- `crates/mkv-core/src/pipeline.rs`, `run_remux` immediately after `scan_mkv_files` / `apply_selected_items` and before `BatchStart`.
- `crates/mkv-core/tests/test_pipeline.rs`, direct `PipelineConfig` regressions.
- `crates/mkv-core/tests/test_assets.rs` only if a resolver-level test is useful; prefer testing the public pipeline seam for this contract.
- `crates/mkv-cli/src/main.rs` only if a CLI-specific boundary assertion is needed; do not duplicate the core rule.

**Implementation:**

1. Define one canonical assignment target normalization path using the same root-relative ID rules as `get_item_id` and `clean_relative_path`.
2. Require assignment targets to be `.mkv` paths. Reject a target with another extension during cheap request validation.
3. In `run_remux`, build the effective assignment set, including legacy `sub_assignments` conversion, after discovery and before emitting `BatchStart`.
4. Build the discovered canonical ID set from the actual `files` vector. Require every effective assignment target to match exactly one discovered ID. Collect all unmatched targets and return one explicit planning error naming all of them.
5. Do not rely on `assignment_index.get(...)` returning `None` to detect this. A missing match is an error, not permission to run automatic resolution.
6. Preserve the existing later missing-asset check for a file that disappears after preflight. The new discovery check handles targets that were never part of the discovered batch; the later check handles TOCTOU disappearance.

**Regression tests:**

- Create `input/Output/hidden.mkv` and a valid assignment targeting `Output/hidden.mkv`, with a discovered sibling video and an automatic subtitle. Run direct `PipelineConfig` in remux mode. Assert the run fails, no output is written, and the error names `Output/hidden.mkv`.
- Add a non-MKV existing target case and assert request validation rejects it.
- Add a case with multiple unmatched assignments and assert all names appear in one error.
- Add a selected-items + assignment case proving valid assignments still resolve and preserve language/title/offset.
- Keep the existing missing-asset-after-validation test; strengthen its terminal assertion to collect every `BatchComplete`, assert exactly one total terminal, assert it is failed/non-cancelled, and assert it follows all item-error events.

**Acceptance:** no supplied assignment can disappear into automatic matching; every assignment is either consumed by exactly one discovered item or the whole batch fails before media work.

### Slice 5 — Enforce adjacency for bare covers

**Why fifth:** `CoverKind::Adjacent` currently checks only the filename stem. Because the resolver ranks ancestor directories, a root bare cover can incorrectly attach to a nested video with the same stem.

**Files:**

- `crates/mkv-core/src/assets.rs`, `CoverIndex::lookup_ranked` `CoverKind::Adjacent` arm.
- `crates/mkv-core/tests/test_assets.rs`, near `test_cover_nearest_directory_then_kind_and_natural_ties` and `test_unparsed_cover_and_source_art_fallback_boundary`.
- `crates/mkv-core/tests/test_real_remux.rs` only if the actual-media fixture needs a nested same-stem case; the resolver regression is the minimum required proof.

**Implementation:**

1. For `CoverKind::Adjacent`, require both:
   - `entry.file.stem == video.stem`; and
   - `entry.file.path.parent() == video.path.parent()`.
2. Keep `_cover` and `.cover` episode matching episode-aware and keep `cover` / `folder` directory-wide behavior unchanged.
3. Do not alter the already-correct cover ordering or source-art fallback semantics.

**Regression test:**

- Root contains `Show - 01.mkv` and `Show - 01.jpg`; nested `Season 01/Show - 01.mkv` has no local cover and has embedded art.
- Assert the nested video does not inherit the root bare cover and retains embedded art.
- Retain the existing same-directory bare-stem test so exact adjacent matching remains covered.

**Acceptance:** only an exact sibling bare-stem cover can satisfy the bare form; ancestor directory covers still work for `cover` and `folder`.

### Slice 6 — Make release media verification non-skipping

**Why sixth:** the only actual-media regression currently returns success when FFmpeg/MKVToolNix are absent, while the release workflow does not install them.

**Files:**

- `crates/mkv-core/tests/test_real_remux.rs`, prerequisite check at lines 77–83.
- `.github/workflows/release.yml`, Linux dependency installation and the verification job.
- `.github/workflows/ci.yml` only if the same required-job behavior is standardized there; do not remove the existing Linux installation.

**Implementation:**

1. Prefer a required-CI contract over a globally failing developer-local test:
   - add an environment switch or separate test target that requires the binaries in CI/release; or
   - make missing tools fail in the required test variant while retaining an explicit local opt-out.
2. Add `ffmpeg` and `mkvtoolnix` to the release Linux dependency installation, matching `.github/workflows/ci.yml`.
3. Ensure the release verification gate depends on the job that runs the non-skipping real-media regression.
4. Keep the test's actual assertions: subtitle language/content, cover attachment, and extracted font output. Do not replace it with a mock-only test.

**Acceptance:** a release cannot publish with the real-media regression silently skipped; local developers without media tools receive an explicit, documented skip/failure according to the chosen contract.

## Documentation and handoff cleanup

These are small but should be completed in the same change or an immediately following documentation-only commit:

1. Update the integration-suite inventory:
   - `architecture.md` around the `crates/mkv-core/tests` layout.
   - `README.md` Project Layout.
   - `test_real_remux.rs` makes the Rust integration inventory nine suites, not eight.
2. Add `.github/workflows/release.yml` to the ADR-0010 traceability row in `architecture.md` if that row is intended to enumerate the release gate files.
3. Update the PR verification narrative with the actual final counts and final-head CI run. The previous PR body contained stale counts and a stale billing-blocked-run comment; the current final-head run `36122518487` passed all checks.
4. Do not add the false S06 tie-break change or a “fix” based on the reversed-`Ord` misconception.

## Verification sequence for the implementation agent

Run focused tests after each slice, then the complete required gates.

### Focused Rust tests

```bash
cargo test -p mkv-core --test test_assets
cargo test -p mkv-core --test test_pipeline
cargo test -p mkv-core --test test_real_remux
cargo test -p mkv-anime-batch
```

For the new regressions, use exact test names while iterating. The real-media test must run with `ffmpeg`, `mkvmerge`, and `mkvextract` available; verify it did not take the skip path.

### Focused frontend tests

```bash
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/job-controller.test.js
node --test desktop-tauri/ui/queue-state.test.js
node --test desktop-tauri/ui/queue-view.test.js
node --test desktop-tauri/ui/app-inspection.test.js
```

The delayed-preflight test must prove the bulk controls are disabled and cannot mutate selection. The BatchStart test must start from an empty state, not a pre-populated manifest.

### Required repository gates

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
```

Because Rust files are touched, also build the desktop release path required by the project:

```bash
cargo build --release -p mkv-anime-batch
cargo build --release -p mkv-cli
```

### Runtime smoke checks

1. **CLI alias mode:** build `target/release/mkv-cli`, invoke it through a symlink/alias named `mkv-extract` with a real fixture, and verify:
   - default invocation uses extract mode;
   - explicit `--remux` wins and uses remux mode.
2. **Media precedence:** run the real FFmpeg/MKVToolNix regression and inspect its assertions/output; do not accept a skipped test.
3. **Desktop reload/stop:** run the native Tauri/WebView surface if the environment supports it. Verify a two-item Active Batch reloads only those ordered cards, restores progress, Stop targets the same job ID, and terminal completion releases controls. If native Tauri is unavailable, report that limitation explicitly; Chromium IPC-stub coverage is not equivalent.
4. **Windows:** require the final-head PR CI Windows workspace test and Windows desktop release build to pass. Do not infer Windows behavior from Linux.

## Review gate before commit

Per `AGENTS.md`, before committing:

1. Run the full Standards + Spec code-review skill on the complete diff, in parallel, against the appropriate fixed point.
2. Fix or explicitly accept every Required/Critical finding. Do not defer a known correctness issue.
3. Re-run the complete gates above after review fixes.
4. Confirm `git diff --check`, clean working tree, and final PR checks.
5. Update the PR description with observed commands and actual counts.

## Definition of done

- Bulk queue actions cannot change a preflight-captured selection.
- Active Batch order survives reload-before-BatchStart and later same-directory rescans.
- Cancellation always produces a cancelled snapshot/terminal, even when the worker returns an error after Stop.
- Every effective assignment matches exactly one discovered canonical MKV ID or the batch fails before media work.
- Bare covers require an exact adjacent sibling video; directory-wide cover behavior remains intact.
- Required release verification cannot silently skip the real-media regression.
- All focused tests, workspace gates, release builds, and required hosted checks pass.
- The false S06 tie-break finding remains removed from the implementation scope.
