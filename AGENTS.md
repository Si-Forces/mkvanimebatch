# AGENTS.md

## Workflow rules

### Code review before every commit

Always run the code review skill (Standards + Spec axes, parallel sub-agents) on the full diff before committing. Review sub-agents work from the diff and the source alone, with no cargo runs. Fix or explicitly accept findings first, then commit.

### CI formatting & lint gate before committing

- **Rust Workspace**: Always run `cargo fmt --all` whenever Rust files are touched. The pre-commit hook then checks `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` on the staged tree, and that run is the clippy gate. Test as described in "Test runs" below.
- **Frontend UI**: Always run `node --check desktop-tauri/ui/app.js` and `node --test desktop-tauri/ui/*.test.js` whenever UI files are touched.

### Test runs: targeted first, then the workspace once

This repo is developed on a 4-core, 8 GB laptop, where a full workspace build uses the whole machine. Build as little as the change needs:

- While iterating, run **targeted** tests: only the suite or module you changed, for example `cargo test -p mkv-core --test test_assets` or `cargo test -p mkv-core --lib remux_plan::`.
- Let the pre-push hook be the one `cargo test --workspace` run: push HEAD from a clean tree, without `SKIP_SLOW_GATES` (see "Git hooks" in CLAUDE.md). For a commit that stays local, run `cargo test --workspace` once, after the last change.
- Run one cargo process at a time across the main checkout and all worktrees, and in multi-agent work give cargo to a single agent.
- Run the bench self-check (see "Commands" in CLAUDE.md) only when you change `benches/`; Linux CI runs it.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues (`sifahaz/mkvanimebatch`). See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical triage roles mapped to GitHub issue labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context layout (`CONTEXT-MAP.md` at repo root pointing to bounded contexts `crates/mkv-core/CONTEXT.md` and `desktop-tauri/CONTEXT.md`, with system-wide ADRs under `docs/adr/`). See `docs/agents/domain.md`.
