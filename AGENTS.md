# AGENTS.md

Cargo workspace for MKV Anime Batch: a media engine (`crates/mkv-core`), a thin CLI (`crates/mkv-cli`), a toolkit-agnostic Desktop Core (`crates/mkv-desktop-core`), and a Tauri v2 desktop app (`desktop-tauri`). `architecture.md` is the full system spec. Area docs, to read before changing:

- the git hooks or jj gates: `scripts/CLAUDE.md`
- the `mkv-core` batch pipeline: `crates/mkv-core/CLAUDE.md`
- the Engine Bridge or either desktop host (Tauri or Slint): `crates/mkv-desktop-core/CLAUDE.md`
- the Tauri host (`desktop-tauri/src-tauri`) or the webview frontend (`desktop-tauri/ui`): `desktop-tauri/CLAUDE.md`

## Commands

Node 20+ (CI uses 22). The frontend is plain JavaScript (ES modules plus the classic `theme-init.js`/`os-detect.js`), with no `package.json`, npm, or bundler. Repo scripts use Node, not Python (ADR-0010).

```bash
# Targeted Rust tests while iterating
cargo test -p mkv-core --test test_assets
cargo test -p mkv-core --test test_domain test_promote_file_basic_and_overwrite
cargo test -p mkv-core --lib remux_plan::
cargo test -p mkv-cli
cargo test -p mkv-desktop-core
cargo test -p mkv-anime-batch-slint -p mkv-anime-batch-slint-ui
cargo test -p mkv-anime-batch

# Real ffmpeg/MKVToolNix regression; fails if the tools are missing
MKV_REQUIRE_MEDIA_TOOLS=1 cargo test -p mkv-core --test test_real_remux
# Without the variable, a missing tool skips the suite
cargo test -p mkv-core --test test_real_remux

# Bench self-check; skips cells unless MKV_BENCH_CORPUS is set (Linux CI runs the self-check)
cargo test -p mkv-core --bench bench_real_media

# Frontend
node --check desktop-tauri/ui/app.js
node --test desktop-tauri/ui/*.test.js
node --test scripts/*.test.js

# Builds
cargo build --release -p mkv-cli
cargo build --release -p mkv-anime-batch        # desktop crate, no bundle
# Desktop bundle; install tauri-cli once: cargo install tauri-cli --version "^2" --locked
(cd desktop-tauri && cargo tauri build)
```

CI also runs `cargo audit` (ignores in `.cargo/audit.toml`) and actionlint. Its CLI smoke test requires `./target/release/mkv-cli --plan --no-cache --input-dir "$(mktemp -d)"` to print a manifest with `"version": 2` and `"items": []`.

## Architecture

- `mkv-core` owns the domain logic and batch pipeline. `mkv-cli` is a thin adapter (`args.rs`, `main.rs`); a binary named `mkv-extract` defaults to extract mode. `mkv-desktop-core` is the toolkit-agnostic Desktop Core (ADR-0016), including the Engine Bridge; `desktop-tauri/src-tauri` is a thin Tauri v2 host, and `desktop-tauri/ui` is plain JavaScript.
- The desktop host runs `mkv-core` in-process through `mkv-desktop-core`; there is no sidecar (ADR-0007). `mkv-desktop-core` must not depend on Tauri, Slint, winit or Skia; CI checks this with `cargo tree`.
- The Slint host (`desktop-slint`, ADR-0017) is being ported beside the Tauri one. Its generated UI crate (`desktop-slint/ui`) alone uses `unsafe_code = "deny"`. Workspace-wide cargo commands now compile Slint and download Skia; CI builds the two Slint crates only in the `desktop-slint` job.
- External media work goes through `media_tool.rs`: the `MediaToolRunner` port, `SubprocessRunner` as its adapter, and `ScriptedRunner` in tests. Exit code 1 from MKVToolNix is success with warnings; exit code 1 from `ffmpeg` is a failure.
- Concurrency: `workers` caps in-flight items; the `io_jobs` semaphore gates only mkvmerge/mkvextract and promotion, so FLAC→Opus transcoding overlaps with other items' muxing (ADR-0011). Only one desktop job runs at a time; Stop is two-phase: a 2 s cooperative cancel, then an abort.
- Invariants:
  - The subtitle precedence ladder: manual > same-directory external > embedded > nearest-ancestor external > `--assets-dir`; source tier beats language (ADR-0004/0008).
  - Outputs stage in `TempDir`s inside the destination and move with `promote_file`; cancellation cleans up by dropping (child processes are `kill_on_drop(true)`).
  - The font registry (`fonts.rs`) runs claim → stage → BLAKE2b dedup → promote/release; `FontClaimsGuard` releases any unpromoted claim on drop.
  - Panics are isolated, so never set `panic = "abort"`.
  - `unsafe_code = "forbid"` workspace-wide, except the generated Slint UI crate `desktop-slint/ui` on `deny`, where no hand-written code goes (ADR-0017, 2026-10-23 amendment).
- Desktop directory identities must use `display_canonical_path` (the canonical path without `\\?\`) and match byte-for-byte; the queue and Run preflight never normalize paths.
- All Intents are sent through the one promise chain in `app.js`; for `scan_directory`, `run`, and `stop`, the chain waits until they are sent, not answered (`UNCHAINED_INTENTS`).
- Linux packaging: the WebKitGTK DMABUF workaround is AppImage-on-NVIDIA only; don't extend it to deb/rpm (ADR-0015).

## Workflow rules

### Code review for every commit

Every change needs a Standards + Spec review before it is finalized; `jj gpush` refuses a non-empty commit without a recorded review. Run the code-review skill on the full committed diff (both axes in parallel sub-agents; the sub-agents work from the diff and source alone, with no cargo runs), then fix or explicitly accept the findings. `jj commit` is a local snapshot, so the practical sequence is commit → review → record → `jj gpush`; amend with `jj squash`/`jj describe` and re-review/re-record if findings land after the commit.

Record the review or `jj gpush` refuses the push:

```bash
scripts/review-gate.sh record-range origin/main HEAD "Standards+Spec over origin/main..HEAD, findings fixed"
scripts/review-gate.sh status    # commits since origin/main that still lack a review
```

Use the branch's actual fork point instead of `origin/main` if it differs. Review and record `<base>..HEAD`, not `@`: `review-gate.sh` is plain git and cannot resolve `@-`.

### Version control: jj

The repo is colocated (`jj git init --colocate`): `.git` still exists, but commit with jj, not `git commit`.

- New clone: run `scripts/install-git-hooks.sh` and `scripts/install-jj-aliases.sh` once. The repo-level jj config is not tracked; if `trunk()` is unknown, run `jj config set --repo 'revset-aliases."trunk()"' 'main@origin'`.
- `jj gate` before `jj new` or `jj commit`, and again after any amend: it bans AI co-author trailers and runs fmt/clippy or UI syntax checks for what changed.
- `jj gpush` instead of `jj git push`: it adds the review-ledger check and runs the test suites when `@` (or an empty `@-`) is the pushed tip. `jj` never runs git hooks, so plain `git push` still fires `scripts/hooks/pre-push`.
- Emergency bypasses: `SKIP_REVIEW_GATE=1` and `SKIP_SLOW_GATES=1`; plain `jj git push` bypasses all jj gates. Don't use these to skip work.
- Never end a commit description with an AI co-author trailer.
- `jj gpush` refuses a pushed `@-` when `@` has changes on top and `@` itself is not also pushed; commit or `jj new` first.
- Scope `jj restore` to paths: a bare `jj restore` also discards new files in `@`.
- Keep `working-copy.eol-conversion = "input"`. `input-output` writes CRLF into every checkout and breaks shell shebangs.

### CI formatting & lint gate before committing

- **Rust Workspace**: Always run `cargo fmt --all` whenever Rust files are touched. `jj gate` (or the git pre-commit hook) checks `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`; that run is the clippy gate. Test as described in "Test runs" below.
- **Frontend UI**: Always run `node --check desktop-tauri/ui/app.js` and `node --test scripts/*.test.js desktop-tauri/ui/*.test.js` whenever UI files or `scripts/` are touched. `theme-init.js` must stay an external classic script (CSP, and it must run before paint); `os-detect.js` is also loaded classically. The gate checks only `theme-init.js` as CommonJS, so keep `os-detect.js` valid when parsed as a module.

### Test runs: targeted first, then the workspace once

This repo is developed on a 4-core, 8 GB laptop, where a full workspace build uses the whole machine. Build as little as the change needs:

- While iterating, run **targeted** tests: only the suite or module you changed, for example `cargo test -p mkv-core --test test_assets` or `cargo test -p mkv-core --lib remux_plan::`.
- Let `jj gpush` (or the git pre-push hook) be the one `cargo test --workspace` run: push from a clean working copy, without `SKIP_SLOW_GATES`. For a commit that stays local, run `cargo test --workspace` once, after the last change.
- Run one cargo process at a time across the main checkout and all worktrees, and in multi-agent work give cargo to a single agent.
- Run the bench self-check only when you change `benches/`; Linux CI runs it.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues (`Si-Forces/mkvanimebatch`). Use the `gh` CLI; conventions are in `docs/agents/issue-tracker.md`.

### Triage labels

Canonical triage roles map to GitHub labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`); see `docs/agents/triage-labels.md` for the mapping and any missing label to create first.

### Domain docs

Read `CONTEXT-MAP.md` → `crates/mkv-core/CONTEXT.md` / `desktop-tauri/CONTEXT.md`, then the relevant ADRs in `docs/adr/`, before working in an area. Use the glossary's terms. If a change contradicts an ADR, say so explicitly rather than overriding it silently. See `docs/agents/domain.md` for the layout, `architecture.md` for the detailed spec, and `README.md` for CLI options.
