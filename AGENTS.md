# AGENTS.md

## Workflow rules

### Code review before every commit

Always run the code review skill (Standards + Spec axes, parallel sub-agents) on the full diff before committing. Fix or explicitly accept findings first, then commit.

### CI formatting & lint gate before committing

- **Rust Workspace**: Always run `cargo fmt --all` and verify with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` whenever Rust files are touched.
- **Frontend UI**: Always run `node --check desktop-tauri/ui/app.js` and `node --test desktop-tauri/ui/*.test.js` whenever UI files are touched.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues (`sifahaz/mkvanimebatch`). See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical triage roles mapped to GitHub issue labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context layout (`CONTEXT-MAP.md` at repo root pointing to bounded contexts `crates/mkv-core/CONTEXT.md` and `desktop-tauri/CONTEXT.md`, with system-wide ADRs under `docs/adr/`). See `docs/agents/domain.md`.
