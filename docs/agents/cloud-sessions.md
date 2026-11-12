# Claude Code cloud sessions

A cloud session (`CLAUDE_CODE_REMOTE=true`) commits and pushes its own `claude/...` branch with plain git, so the jj and git-hook rules in `AGENTS.md` apply only to local clones:

- Don't install jj or the hooks there. jj would detach HEAD from the session branch, pre-push would refuse a push without a recorded review, and commit-msg would refuse a Claude co-author trailer.
- Use `git commit`, and run by hand what the gates would: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and targeted tests for Rust, Cargo or Slint UI changes, and the Node checks (`AGENTS.md`, "CI formatting & lint gate") for UI or `scripts/` changes. CI runs the rest; the Standards + Spec review is the maintainer's check below.
- The trailer ban still holds. `.claude/settings.json` sets `attribution.commit` to `""` for every Claude Code session, but upstream reports say it can be ignored (see the `scripts/hooks/commit-msg` header), and nothing in the cloud enforces the ban. After each `git commit`, read `git log -1 --format=%B`, and if a trailer names an AI agent as co-author, remove it with `git commit --amend -m "<the message without that trailer>"` before the next commit.

## The maintainer's check

In a local clone before merging a cloud branch (not a cloud-session step):

1. Run `jj git fetch`, then `jj new <branch>@origin` and `jj gate` for the trailer ban,
   the net-diff home-path check, fmt, clippy and JavaScript syntax checks.
2. Check each incoming commit, including a path added and removed within the branch:

   ```bash
   main_tip=$(git rev-parse --verify 'refs/remotes/origin/main^{commit}')
   cloud_tip=$(git rev-parse --verify 'refs/remotes/origin/<branch>^{commit}')
   scripts/hooks/home-path.sh --published "$main_tip" "$cloud_tip"
   ```

   Substitute the fetched branch name for `<branch>`. Use full ids from this fetch; fetching
   the topic must not make it count as already checked.
3. Run the code-review skill over `origin/main..origin/<branch>`, fix or accept findings,
   then `scripts/review-gate.sh record-range origin/main origin/<branch> "..."`.

A GitHub merge consults neither local gates nor the ledger, so the maintainer must complete
these steps before merging. The home identities are the maintainer's HOME and USERPROFILE.
The cloud branch is already published when they run: refusing its incoming history can keep
it out of main, but cannot retract an exposed path from GitHub.
