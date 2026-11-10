# Claude Code cloud sessions

A cloud session (`CLAUDE_CODE_REMOTE=true`) commits and pushes its own `claude/...` branch with plain git, so the jj and git-hook rules in `AGENTS.md` apply only to local clones:

- Don't install jj or the hooks there. jj would detach HEAD from the session branch, pre-push would refuse a push without a recorded review, and commit-msg would refuse a Claude co-author trailer.
- Use `git commit`, and run by hand what the gates would: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and targeted tests for Rust, Cargo or Slint UI changes, and the Node checks (`AGENTS.md`, "CI formatting & lint gate") for UI or `scripts/` changes. CI runs the rest; the Standards + Spec review is the maintainer's check below.
- The trailer ban still holds. `.claude/settings.json` sets `attribution.commit` to `""` for every Claude Code session, but upstream reports say it can be ignored (see the `scripts/hooks/commit-msg` header), and nothing in the cloud enforces the ban. After each `git commit`, read `git log -1 --format=%B`, and if a trailer names an AI agent as co-author, remove it with `git commit --amend -m "<the message without that trailer>"` before the next commit.

## The maintainer's check

In a local clone before merging a cloud branch (not a cloud-session step): `jj git fetch`, `jj new <branch>@origin`, then `jj gate` (the trailer ban over the branch's commits and the home-path check on its diff, plus fmt, clippy and `node --check` for what changed), the code-review skill over `origin/main..origin/<branch>`, and `scripts/review-gate.sh record-range origin/main origin/<branch> "..."`. A merge on GitHub consults neither the gate nor the ledger, so this is the branch's only enforced trailer and home-path check and its Standards + Spec review.
