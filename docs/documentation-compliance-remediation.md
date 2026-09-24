# Documentation Compliance Remediation

**Status:** Product decisions ratified 2026-09-24; implementation applied locally. Hosted Windows verification is performed by the pull-request workflows; this document itself changes no runtime behavior.

## Goal and scope

Bring the current Rust Media Engine, standalone CLI, Desktop Application, and delivery checks into agreement with their active documentation. The audit found behavior defects, conflicting specifications, and overstatements. Do not treat a passing suite as proof of these paths: the baseline audit passed 250 Rust tests and 90 UI tests while the selection and media mismatches remained reproducible.

This plan covers the 15 findings in the audit response. Preserve Manifest v2, the typed `BatchRequest`/`TrackAssignment` interface, canonical root-relative item IDs, one active desktop job, and the existing external MKVToolNix/FFmpeg requirement. No Python/sidecar compatibility path, new frontend framework, or new dependency is needed.

### Ratified decisions governing implementation

1. **Subtitle precedence:** manual Track Assignment > same-directory external subtitle > embedded track > nearest matching ancestor external subtitle > explicit `assets_dir` fallback. An adjacent English subtitle beats an embedded Indonesian track; a parent English subtitle does not. ADR-0004 records this implemented revision.
2. **Cover names and precedence:** recognize `cover.<ext>` and `folder.<ext>` as directory covers, and `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, and `<video-stem>.<ext>` as episode covers. Only the bare-stem form requires an exact adjacent video stem. The nearest applicable directory wins first; within it, episode names rank `_cover` > `.cover` > bare stem, then directory names rank `cover` > `folder`. Equal tiers use natural filename order and then full filename. Inapplicable `*_cover` images are never directory covers. Local or ancestor covers supersede embedded art; explicit `assets_dir` covers are fallback only when embedded art is absent. ADR-0004 records this implemented convention.
3. **Font retention:** remove the inert `keep_fonts` setting across the CLI, desktop UI, and typed schemas in one breaking cutover with release notes. Extracted fonts remain user output; never delete pre-existing or user-supplied fonts. ADR-0008 records the implemented removal.
4. **Ambiguous chapters and UI budgets:** correct documentation rather than add unsafe natural-sort chapter pairing or promise a hard 8 ms budget for every DOM update. Only a single unmatched chapter and single eligible video are automatically paired. The ~8 ms deadline is best effort for manifest card creation, not a 60 FPS guarantee.
5. **Active reload:** a Job Snapshot represents the ordered, selected Active Batch, not every unselected item in the last Inspection Manifest. Do not silently rescan to reconstruct an active queue.

## Behavioral specification and acceptance

| ID | Priority | Required observable contract |
| --- | --- | --- |
| S01 | P0 | Every same-directory rescan, including pre-run inspection, user-initiated rescan, and completed-manifest reuse, preserves surviving Queue Selection and relative order. Newly discovered items in that directory are unselected until the user selects them; a fresh browse of a different directory retains the all-selected default. A vanished selected item fails before `run_job`; never launch a wider or partial batch. |
| S02 | P1 | Release desktop builds unwind unexpected Rust task panics so the Engine Bridge can emit a terminal failure and release the job slot. No configuration may claim task panic isolation while using `panic = "abort"`. |
| S03 | P1 | A manual subtitle assignment missing at validation or resolution fails closed, including for an unparseable video name. Never choose an automatic subtitle or apply the missing assignment's offset/language/title to another track. A planning-time failure after `BatchStart` aborts the entire batch before any media output: the affected item and all remaining queued items end as `error` with distinct reasons, followed by exactly one failed `BatchComplete` and a failed snapshot. Valid assignments still supersede automatic selection. |
| S04 | P1 | Cooperative Stop and its two-second hard-abort path both settle the active job exactly once: unresolved items cancelled, a cancelled batch snapshot, `job-done` with matching job ID, and controls released. A stale Stop cannot affect a newer job. |
| S05 | P1 | Reload during an active job restores its ID, status, canonical ordered Active Batch items (not unselected manifest items), most recent percentage, and live Stop/event subscriptions without rescanning. Terminal `duration_sec` remains engine-authoritative; an in-flight elapsed display can use a saved start timestamp. Completed snapshots do not reserve a new job. |
| S06 | P1 / decision ratified | A parent English `.ass` cannot displace an embedded Indonesian subtitle when no same-directory external subtitle exists. The nearest matching ancestor follows embedded tracks, and explicit `assets_dir` remains last. |
| S07 | P2 / decision ratified | Nearest applicable directory wins before cover type. Within a directory, episode `_cover` > `.cover` > exact adjacent bare stem > directory `cover` > `folder`; equal-tier candidates use natural filename order then full filename. Inapplicable `*_cover` is not a directory cover. Local or ancestor external covers supersede embedded art; explicit `assets_dir` covers apply only when embedded art is absent. |
| S08 | P2 / decision ratified | Extracted fonts always remain user output. Remove `keep_fonts` from CLI, UI, presets, request/config types, tests, and active docs in the approved breaking cutover; keep transactional staging cleanup and deduplication. |
| S09 | P2 | Explicit `selected_items` must each resolve to one *discovered* MKV item, or `--plan`/remux/extract returns an error before output writing or batch execution. A plan may scan to validate the selection. A valid MKV mixed with `notes.txt` is not a partial success; an unselected, empty directory retains its existing successful empty-plan behavior. |
| S10 | P2 | Executable name `mkv-extract` supplies only the default mode. Explicit `--remux` or `--extract` wins; the normal `mkv-cli` default stays remux. |
| S11 | Docs | Document that leftover XML is automatically claimed only if exactly one unmatched chapter and one eligible unchaptered video exist. Multiple ambiguous files remain unclaimed; do not attach by arbitrary natural-sort pairing. |
| S12 | Docs | State that initial manifest cards are scheduled in best-effort ~8 ms chunks; progress updates render synchronously and log batches are flushed in one animation frame today. Remove the unconditional 60 FPS/"all visual updates" guarantees unless measured and implemented separately. |
| S13 | P2 | Windows CI and release verification run `cargo test --workspace` including desktop Rust tests, and compile the Windows release desktop before the required status gate. A documented technical exception requires a separate decision if actual runner evidence demonstrates a blocker; a green CLI-only run is not workspace-wide coverage. |
| S14 | Docs | The architecture traceability matrix and `docs/agents/domain.md` include accepted ADR-0013 and ADR-0014. Make the 2026-09-24 amendments to ADR-0004 and ADR-0008 discoverable and distinguish ratified contracts from still-pending implementation. |
| S15 | Docs | Nonexistent `out_dir` is allowed and created when execution writes output; an existing regular file is rejected at validation. Planning/dry-run must not create media output directories. Correct `crates/mkv-core/CONTEXT.md:22`, not the working validation rule. |

### Baseline evidence to preserve as regressions

- S01: `JobController.start` forces a rescan before reading selected IDs (`desktop-tauri/ui/job-controller.js:195-233`); `QueueTracker.populateFromInspection` resets selection (`queue-view.js:204-215`, `queue-state.js:126-149`). The audit reproduced `02,01` selected becoming `01,02,03` after a rescan.
- S02/S04: `Cargo.toml:21-22` sets release `panic = "abort"`; `process.rs:506-520` aborts the task whose `job-done` emitter runs later in `commands.rs:168-233`.
- S03/S06: `assets.rs:674-705` still falls back from a missing assignment and searches an ancestor-populated external index before embedded tracks; `pipeline.rs:1019-1074` carries override metadata independently. An actual remux picked parent English over embedded Indonesian.
- S07/S08: `scan.rs:428-432` ignores `folder.jpg`; an actual remux attached no cover. `request.rs:417` writes `keep_fonts` to a config field that execution never reads; actual extract runs with and without `--keep-fonts` retained the same font.
- S09/S10: `request.rs:359-365` accepts an existing non-MKV path; `manifest.rs:183-196` and `pipeline.rs:193-223` drop unmatched selections. The audit observed successful partial/zero-item runs. `main.rs:35-48` applies executable-name mode after explicit flag parsing; an actual `mkv-extract --remux --dry-run` reported extraction.

## Implementation plan — in dependency order

### Phase A — prevent unintended media work (S01, S03, S09)

**S01: preserve Queue Selection through forced inspection.**

1. In `desktop-tauri/ui/job-controller.js`, capture the queue's canonical selected IDs and ordering **before** `scanInputDirectory(..., { force: true })`, along with the existing assignment snapshot. Freeze the relevant controls while awaiting inspection, as `JobCoordinator` already does.
2. In `desktop-tauri/ui/app.js` and `queue-view.js`, retain prior queue state before `startScan()` clears it and pass it through the scan-token-checked `acceptInspection` path to reconciliation in `queue-state.js`. On **every** same-directory rescan or `reuseInspection`, keep surviving IDs' selection and order and append new IDs as unselected; only a fresh different-directory browse defaults to all selected. Keep assignments only for surviving IDs and ensure rendered card checkboxes reflect reconciled selection rather than the current hardcoded `checked` attribute. An older response must never overwrite a newer scan.
3. Compare the captured selected IDs with the new manifest **before** invoking `run_job`. If any disappeared, show a specific error and leave the job unlaunched; if none remain, do not convert that to an unrestricted request. Submit exactly the captured, verified ordering in `BatchRequest.selected_items`; filter assignments against it.
4. Add one controller/queue integration regression using a fake inspection that really **replaces** the manifest during Start. Assert submitted IDs, preserved card order/checkboxes, excluded newly discovered items, and a vanished selected item preventing `run_job`. Include a user-initiated same-directory rescan with a newly discovered item. Existing controller tests stub `scanInputDirectory` without replacing queue state, so they cannot detect this regression.

**S03: fail closed at the Asset Resolver seam.**

1. Keep `BatchRequest::validate()`'s synchronous missing-file check in `crates/mkv-core/src/request.rs`. In `crates/mkv-core/src/assets.rs`, return an explicit error when `assigned_subtitle` no longer exists, including the unparsed-video path; never continue to local/embedded/fallback tracks. Propagate it through `resolve_for` and `pipeline.rs` before scheduling remux work. Since `run_remux` already emits `BatchStart` before planning (`pipeline.rs:668-693`), emit `ItemComplete(error)` for the affected item with the missing-assignment reason and for each remaining queued item with a distinct batch-planning-aborted reason, followed by exactly one failed `BatchComplete`; do not return a bare `Err` with a running snapshot or write media output.
2. Never attach `matched_assignment` metadata to a track unless `res_assets` resolved that same assignment. Remove the stale fallback comment referring to superseded ADR-0001/0004. Preserve positive and negative offset precedence for valid assignments.
3. Regression: validate an assignment, delete the asset before resolution while another automatic subtitle is available, and assert a failed batch, all item outcomes `error` with distinct reasons, no completed remux, and no reassigned metadata. Include an unparseable video filename and direct `PipelineConfig` callers, not just CLI/desktop validation.

**S09: reject unmatched selected items at both stages.**

1. Extend the cheap `BatchRequest::validate()` check to reject obviously non-MKV selections (case-insensitive `.mkv`) before a desktop job slot is acquired; retain existing containment, existence, duplicate, and assignment checks. Do not run a full second directory scan solely for validation.
2. Change `pipeline::apply_selected_items` to return an error naming **every unmatched requested ID** after the normal discovery scan. Call it in both remux and extract paths before `BatchStart` or any output work; this also catches ignored directories and changed files. In CLI `generate_plan_manifest`, compare the requested IDs against the filtered manifest rather than failing only when *all* are unmatched.
3. Regression: mixed valid MKV + existing `notes.txt`, all invalid, and an existing `.mkv` outside the discovered set must fail in plan/remux/extract. Confirm valid selected order and omission-of-selection behavior remain unchanged.

### Phase B — make desktop lifecycle truthful (S02, S04, S05)

**S02: align release panic policy.**

1. Remove `panic = "abort"` from the root `[profile.release]` in `Cargo.toml` (Rust's normal unwind behavior), or set it to `"unwind"` explicitly. Do not rely on a `cargo test --release` pass alone: test harness panic behavior can differ from the packaged binary.
2. Keep the existing Engine Bridge `JoinHandle` error path and verify a panic in a task is translated into one terminal error event and one `job-done`, with the slot released. Build the desktop in release mode to verify the effective profile and packaging path. Expect a size tradeoff; crash containment takes priority over binary size.

**S04: abort the worker, not its terminal emitter.**

1. In `desktop-tauri/src-tauri/src/commands.rs`/`process.rs`, keep a supervisor responsible for draining event/log channels, reducing `JobSnapshot`, emitting one `BatchComplete` and one `job-done`, and finally releasing the `JobSlot`. Give the two-second stop escalation an abort handle for the **inner pipeline task**, not the outer supervisor that owns finalization. Make the cancellation flag determine the terminal status when that inner task is aborted; do not report an intentional abort as a panic.
2. Deduplicate completion by job ID so a late cooperative completion cannot race a forced-stop completion. Preserve the job-ID check already used by `stop_job`. If shutdown cannot confirm task cleanup, report failure instead of reporting a completed teardown.
3. Regression: use a deliberately pending inner task with a short injected timeout in bridge tests; force escalation and assert cancelled snapshot/items, exactly one batch terminal event and `job-done`, released slot, and a `JobController` that leaves `stopping`. Exercise normal cooperative Stop and a stale job ID too. Do not add a production panic or hanging path just for tests.

**S05: restore an active controller, not only cards.**

1. Add explicit ordered Active Batch item IDs to `JobSnapshot` in `process.rs` from `BatchStart.items`; keep its map for O(1) lookup. Record the latest `ItemProgress.progress` and per-item start timestamp on relevant events, clamping progress as the live reducer does. Preserve engine-provided terminal `duration_sec` unchanged. Do not populate the snapshot with unselected Inspection Manifest items.
2. Update `desktop-tauri/ui/queue-state.js` to render only participating snapshot items in their saved order, set progress on processing cards, and use the saved start only for the approximate running timer. In `app.js`/`job-controller.js`, attach ID-filtered listeners before fetching/reducing the snapshot (buffer events around the fetch), then restore the matching controller's `jobId`, lifecycle, coordinator state, telemetry mode and Stop availability. Do not rescan an active release directory; completed/error/cancelled snapshots only restore presentation.
3. Regression: reload an active two-item job selected from a larger manifest after a 70% progress event; assert only the two active cards, stable order, determinate 70% fill, enabled Stop targeting the same ID, and later completion updates. Include completion during hydration and an unrelated/stale job event. A live Tauri reload smoke check is needed for the real IPC seam.

### Phase C — reconcile media behavior with one published rule (S06, S07, S08)

**S06: implement the ratified subtitle ladder.**

1. ADR-0004 records the ratified precedence revision; align `architecture.md` and the Media Engine context with it before changing selection. Preserve explicit-assignment fail-closed semantics from the ADR-0008 amendment.
2. In `crates/mkv-core/src/assets.rs`, ask `SubIndex` for an exact same-directory match before inspecting embedded subtitles. Only if neither matches should the resolver consider ancestor index entries and then explicit `assets_dir` fallback. Keep matching/score rules in `SubIndex` rather than copying them into the caller.
3. Regression: same-directory English external beats embedded Indonesian as specified; without same-directory external, embedded Indonesian beats parent English; without embedded, nearest matching ancestor beats `assets_dir`. Inspect the subtitle language and content of an actual remux, not just a method call.

**S07: make advertised cover names deterministic.**

1. Update `scan_assets` and `CoverIndex` in `scan.rs`/`assets.rs` together. Support the agreed global and episode names without treating arbitrary `*.jpg` files or inapplicable `*_cover` names as directory-wide covers. Check bare `<video-stem>.<ext>` against an exact sibling MKV stem; preserve supported extension and MIME handling, existing episode parsing and ancestor traversal.
2. Rank by nearest applicable directory first; within a directory use `_cover` > `.cover` > exact bare video stem > `cover` > `folder`. For equal tiers, use natural filename order then full filename, never filesystem enumeration order. Local and ancestor external covers supersede embedded source art; an explicit `assets_dir` cover applies only without embedded art.
3. Regression: each advertised filename results in the correct actual remux attachment; `cover` beats `folder`, a local directory cover beats an ancestor episode cover, ancestor `folder` beats embedded art, and explicit `assets_dir` art does not replace embedded art. Assert that an unrelated JPEG and an inapplicable `logo_cover.jpg` with no matching video remain unclaimed and that results stay stable under different directory-entry orders. Update README, architecture, context and ADR traceability for the accepted revision.

**S08: remove the approved, misleading font-retention knob.**

1. Delete `keep_fonts` from `PresetProfile`, `BatchRequest`, `PipelineConfig`, CLI parsing/help, desktop form/controller/preset wiring, and tests that only assert a copied boolean. Keep `FontRegistry::cleanup_staging()` and promoted-font outputs; do not delete user-supplied directories. Update README and both contexts to say extract mode retains extracted fonts and remux uses discovered fonts for attachments.
2. Check all callers of the exported Rust types and the Tauri serialized request before cutover; remove obsolete field copying and docs in the same change. Record the breaking CLI/IPC change in release notes. Regression: extraction writes a font attachment to the intended output; a remux still embeds the chosen fonts. Do not retain a no-op compatibility alias.

### Phase D — smaller contract and assurance corrections (S10–S15)

**S10:** In `crates/mkv-cli/src/main.rs`, choose executable-name default before parsing explicit mode flags (including Windows `.exe` and symlink names). Add `--remux` to help. Exercise actual `mkv-extract --remux --dry-run` and its default extract mode; an explicit flag must win.

**S11:** Correct `architecture.md:283-284` to describe the existing conservative one-to-one `claim_leftover_chapters` rule (`assets.rs:797-832`). Keep a regression for two ambiguous chapters/two videos yielding no claims. No automatic natural-sort attachment of ambiguous chapters.

**S12:** Correct `desktop-tauri/CONTEXT.md:18` and `architecture.md:494-496,636-644`: the ~8 ms deadline applies to incremental manifest card creation (`queue-view.js:242-269`); live `handleEvent` and batched log flushing are not hard-bounded. Remove "constant 60 FPS" and universal frame-budget language. Only add bounded log/event rendering if a repeatable burst test shows a responsiveness problem.

**S13:** Replace Windows' core/CLI-only test branches in both `.github/workflows/ci.yml:70-76` and `.github/workflows/release.yml:72-79` with `cargo test --workspace`, including desktop Rust tests. Compile a Windows release-mode desktop (`cargo build --release -p mkv-anime-batch` or the existing `cargo tauri build --no-bundle`) **before** the required status gate in both workflows. If a hosted-runner failure proves this impossible, surface evidence and request a new exception decision; do not silently retain CLI-only coverage or label it "workspace tests".

**S14:** Add ADR-0014 to `architecture.md:647-665`, update `docs/agents/domain.md:10,21` to include 0013–0014, and link the 2026-09-24 amendments in ADR-0004 and ADR-0008. Distinguish accepted decisions from implementation pending in all traceability descriptions.

**S15:** Change only the incorrect output-directory sentence in `crates/mkv-core/CONTEXT.md:22`. `BatchRequest::validate` correctly rejects an existing file but permits a new output directory (`request.rs:167-174`); execution creates it (`remux.rs:307-318`, `extract.rs:84-90`). Confirm `--plan`/`--dry-run` do not create that output path. Do not introduce a preflight directory-creation side effect.

## Verification and rollout

1. **Before each slice:** reproduce its reported behavior on current HEAD; preserve one regression that fails on a plausible reintroduction. Test consumer-visible outcomes (selected IDs, MKV track language, cover attachment, terminal event count, file existence), not just copied fields or source text.
2. **After each slice:** run the smallest relevant Rust or Node test and a smoke scenario on the affected surface. For desktop changes, exercise a real Tauri webview for rescan selection, reload, Stop and hard-abort behavior when an interactive runtime is available; if unavailable, report the limitation explicitly. For release/Windows behavior, inspect actual GitHub Actions results rather than claiming local Linux verification covers Windows.
3. **Final local gates after Rust/UI changes:** `cargo fmt --all`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `node --check desktop-tauri/ui/app.js`, and `node --test desktop-tauri/ui/*.test.js`. Run one end-to-end CLI remux/extract fixture with `ffmpeg`, `mkvmerge` and `mkvextract` available. Audit docs/ADRs for conflicting precedence and deprecated fields; no half-migrated paths.
4. **Before any commit:** follow `AGENTS.md`: run the code-review skill on the full diff on Standards and Spec axes in parallel, fix or explicitly accept findings, then commit. No commit is part of this specification-writing task.

**Completion criterion:** every S01–S15 contract has acceptance evidence, active documentation states the same rule as implemented code, and the ratified subtitle, cover, font, reload, and Windows gate decisions are applied end to end. Local implementation and verification are complete; hosted Windows evidence must come from the associated pull request's required checks.
