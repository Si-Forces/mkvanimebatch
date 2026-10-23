# scripts/CLAUDE.md

How the git hooks, the review-gate ledger and the jj gates work. The rules for using them are in the root `AGENTS.md` ("Workflow rules").

## Git hooks and the review ledger

Enable the tracked hooks once per clone with `scripts/install-git-hooks.sh`, which sets `core.hooksPath` to `scripts/hooks` (worktrees share it):

- commit-msg refuses a message whose trailer block credits an AI agent (Claude, Copilot, `AI Bot`). It parses trailers with `git interpret-trailers`, so only real trailers count: a prose mention or a `git commit -v` context line naming one is not blocked, which keeps the commit that documents this hook committable. A co-author naming a person, including the maintainer's own `sifahaz <...@users.noreply.github.com>` line, is left alone. It refuses rather than rewriting the message, so dropping the attribution is visible.
- pre-commit runs workspace fmt and clippy when Rust, Cargo or `desktop-slint/ui` files are staged, and `node --check` on staged UI files. It refuses to run cargo while any of those files differs from the index, so the check matches the commit.
- pre-push runs the **review-gate ledger** check on every pushed ref, refusing the push if any pushed commit has no recorded review (skip it with `SKIP_REVIEW_GATE=1`). When HEAD is one of the pushed tips, it then runs `cargo test` and `node --test`, refusing if the working tree differs from HEAD, untracked Rust, Cargo or UI files included (skip them with `SKIP_SLOW_GATES=1`). Other pushed tips, such as a second branch or an older tag, get only a warning. If HEAD is not pushed at all, that warning is the whole local test coverage.

The two dirty-tree checks (pre-commit's, when it runs cargo, and pre-push's, before its test gate) also refuse while an assume-unchanged or skip-worktree file is missing or differs from the index, since git diff cannot see that. Pre-commit looks only at the files cargo reads (Rust, Cargo and `desktop-slint/ui`); pre-push looks at every flagged file. A flagged file that still matches the index passes. The refusal says how to fix it. To bypass pre-commit's check, use `git commit --no-verify`, which skips all of commit-msg and pre-commit; to bypass pre-push's, use `SKIP_SLOW_GATES=1`, which skips its test gate. In a sparse checkout the out-of-cone files are missing, so pre-push refuses whenever it reaches its test gate, and pre-commit refuses when it runs cargo while any file cargo reads is out of the cone. Those refusals hold only while `scripts/` is in the cone: outside it the hooks are missing too, and git runs no gate at all.

The hooks run straight from the working tree, so an uncommitted edit to `scripts/hooks/` or `scripts/review-gate.sh` changes the gates at once.

After running the code-review skill, record the review:

```bash
scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"
scripts/review-gate.sh status    # commits since origin/main that still lack a review
```

The ledger keys each commit on the `git patch-id` of its diff, so a review survives a rebase or amend that leaves the diff unchanged. Repo scripts use `node`, not Python: ADR-0010 removed Python from dev and CI environments.

## Jujutsu (jj)

The repo is a colocated jj repo (`jj git init --colocate`): `.git` stays, so GitHub, `gh`, CI and `git push` work unchanged. **jj never runs git hooks**, so `jj commit` and `jj git push` bypass everything above. Run `scripts/install-jj-aliases.sh` once per clone to set the repo-level jj config:

- `jj gate` runs `scripts/jj-gate.sh check` over `trunk()..@`: the commit-msg trailer ban on each description (it calls `scripts/hooks/commit-msg`), workspace fmt and clippy when Rust or Cargo files changed since trunk, and `node --check` on changed UI modules. jj has no index, so the gate checks the working-copy commit `@`, which every jj command snapshots first.
- `jj gpush [args]` asks `jj git push --dry-run [args]` what would move (it refuses to push when it cannot read that plan). For each pushed ref it runs the trailer ban over `trunk()..<new tip>`, commits already on the remote branch included, and the review-gate ledger check over the commits not yet on the remote branch (for a new branch, those since its fork point with `origin/main`). It then runs `cargo test --workspace` and `node --test` when `@` (or `@-` with `@` empty) is a pushed tip. It refuses when `@` has changes on top of the pushed `@-`, warns for other tips, then reads the plan again and pushes it with no new snapshot (below). Use it instead of `jj git push`. `SKIP_REVIEW_GATE=1` and `SKIP_SLOW_GATES=1` skip the same gates as before. `git push` still fires `scripts/hooks/pre-push`.
- `working-copy.eol-conversion = "input"` is set because `.gitattributes` normalizes CRLF to LF: git applies it on its own, jj shows every CRLF file as modified without it. Never use `input-output`; it writes CRLF into every checkout and breaks shell shebangs. jj also records the executable bit that `core.fileMode=false` hides from git.
- Nothing stops an AI trailer at `jj describe` time; `jj gate` and `jj gpush` refuse it afterwards. There is no counterpart to the hooks' untracked-file and hidden-edit checks: jj snapshots every non-ignored file into `@`, so the tests run on `@` as pushed. The push takes no new snapshot (`--ignore-working-copy`), so an edit made while the tests run is not pushed, though the tests may have seen it; and it refuses when the plan changed while the gates ran, including when another jj command snapshotted `@` meanwhile.
- Scope `jj restore` to paths (`jj restore <path>`); a bare `jj restore` also discards new files in `@`.

Under jj the ledger records commits, and `HEAD` is `@-` only once the work is committed (`jj commit`, or `jj describe` then `jj new`). Review and record `<base>..HEAD` (`review-gate.sh` is plain git and cannot resolve `@-`) after that, not `@` before it.
