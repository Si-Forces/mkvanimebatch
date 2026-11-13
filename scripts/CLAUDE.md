# scripts/CLAUDE.md

How the git hooks, the review-gate ledger and the jj gates work. The rules for using them are in the root `AGENTS.md` ("Workflow rules").

## Git hooks and the review ledger

Enable the tracked hooks once per clone with `scripts/install-git-hooks.sh`, which sets `core.hooksPath` to `scripts/hooks` (worktrees share it):

- commit-msg refuses a message whose trailer block credits an AI agent (Claude, Copilot, `AI Bot`). It parses trailers with `git interpret-trailers`, so only real trailers count: a prose mention or a `git commit -v` context line naming one is not blocked, which keeps the commit that documents this hook committable. A co-author naming a person, including the maintainer's own `sifahaz <...@users.noreply.github.com>` line, is left alone. It refuses rather than rewriting the message, so dropping the attribution is visible.
- pre-commit runs workspace fmt and clippy when Rust, Cargo or `desktop-slint/ui` files are staged, and `node --check` on staged `desktop-tauri/ui` and `scripts/` JavaScript files. It refuses to run cargo while any of those files differs from the index, so the check matches the commit. It runs the home-path check (below) on the staged diff first, so a refusal shows even when the dirty-tree check then stops the hook.
- pre-push runs the home-path check on each commit it publishes (the destination ref’s old tip through its new tip, so fetched add/remove history still counts), then the **review-gate ledger** check on every pushed ref, refusing the push if any pushed commit not already on `origin/main` has no recorded review (skip it with `SKIP_REVIEW_GATE=1`). When HEAD is one of the pushed tips, it then runs `cargo test` and `node --test`, refusing if the working tree differs from HEAD, untracked Rust, Cargo, UI or `scripts/` files included (skip them with `SKIP_SLOW_GATES=1`). Other pushed tips, such as a second branch or an older tag, get only a warning. If HEAD is not pushed at all, that warning is the whole local test coverage.

`scripts/hooks/home-path.sh` is the **home-path check**. An agent that copies an absolute path from its session would publish the committer's username and folder layout, and once pushed, an amend or force-push cannot take it back: GitHub keeps the old commit until GitHub Support purges it. So the check refuses any added line in a diff that names the committer's own home:

- **Coverage:** active installed Git hooks, or explicit `jj gate` and `jj gpush` use. `jj commit`
  runs no gate. Identities come from the checking process’s `HOME` and `USERPROFILE`; another
  machine’s home is not detected. [Cloud sessions](../docs/agents/cloud-sessions.md) need the
  maintainer’s per-commit check after fetching; it cannot undo their prior publication.
- **Identities:** HOME as set and resolved, and USERPROFILE with either slash or JSON-doubled
  backslashes. Plain and as-set identities also count in Claude Code’s dashed spelling: one
  dash per non-ASCII-alphanumeric UTF-16 unit (two for an emoji), computed with Node. The
  JSON-doubled representation is not a separate dashed identity. Newline identities, including
  a resolved HOME, refuse. A Unicode replacement character also refuses because Node may
  have substituted it for undecodable identity bytes. Only a truly absent HOME may remain
  unresolved; dangling symlinks, loops and inaccessible ancestors refuse. CDPATH is ignored.
- **Matching:** added hunk lines only, in ASCII case-insensitive form. Plain homes can start
  anywhere (`-I/home/<user>/include`) and end at a segment boundary; sentence punctuation and
  git ranges count as boundaries. Dashed homes reject an ASCII-alphanumeric prefix or suffix;
  `_`, `+`, `.` and `-` before one qualify. Repeated slashes and JSON-escaped slashes count;
  literal POSIX backslashes are preserved too. Generic paths naming another user pass.
- **No qualifying identity:** one-segment homes such as `/root` or `C:\app`, `/`, a drive alone
  and unset variables name nobody. With the required tools available, if neither variable
  qualifies, warn and check nothing.
  This includes typical root containers. A longer resolved HOME is judged independently.
- **Report:** `file:line` or `<commit> file:line`, never added text. Decode Git-quoted filenames
  before redacting supported home spellings, then safely quote them again. Keep a leading
  slash while removing Git’s `b/` prefix so repo-relative `home/<user>/...` is redacted too.
  Redaction checks suffix boundaries and merges overlapping spans, including dashed matches
  starting inside another home; unrelated sibling names retain their spelling. Use relative
  paths, `~` or placeholders such as `<repo>`, then rewrite the commit that added the hit.
- **Inputs:** `--staged` reads the index. `jj gate` supplies the net diff since the fork point;
  it does not inspect intermediate commits. `--published <old> <new> [remote]` reads every
  commit in `old..new`, even if fetched or held by another remote. Both ids must be full.
  For a new ref (`old=0`), only `refs/remotes/<remote>/main`, when known, is excluded, and
  only when that remote has one effective push URL equal to its fetch URL. Unknown URLs,
  different/multiple push URLs or a missing main baseline scan all reachable history. Existing
  refs use the destination old tip from the push protocol/plan, never every remote ref.
  Pushes pass that range before either SKIP variable. Cloud checks pass explicit main/tip ids.
- **Git settings:** the shared diff flags force text, no color, no external diff/textconv,
  no relative paths and fixed prefixes. Replace refs and grafts are ignored; root commits
  count. A merge counts additions absent from every parent. Pre-commit is stricter: its
  index-vs-HEAD diff can refuse a line another parent already held. The shell retains neither
  diffs nor reports; scanner memory follows the largest line. Git has its own memory costs.
- **Bypasses:** `git commit --no-verify`, `git push --no-verify`, plain `jj git push`, missing
  or redirected hooks, and edited scripts. `SKIP_REVIEW_GATE` and `SKIP_SLOW_GATES` never skip
  this check. GitHub merges run neither local gate nor review ledger.
- **Limits:** filenames alone (#226), PR descriptions (#227), and commit/tag messages (#220)
  remain unchecked. Binary merge resolutions and all binary files in `jj gate` remain
  unread (#222); ordinary binary additions are checked at push time. Unsupported encodings
  include mixed Windows separators, WSL drive paths, a Windows path with each backslash
  escaped twice (four backslashes in the line), `%20`,
  shell-escaped spaces, `%2F` and Unicode-escaped slashes. Conservative boundaries refuse
  siblings such as `<home>+x`, `<home>./x`, `<home>..x` or `<home>é/x`, and flag/CSS names
  starting with a dashed identity. Previously published GitHub objects require GitHub Support
  to purge; changing this checker or rewriting a branch cannot retract them.
- **Tools:** the checker requires Node (repo minimum 20) and Bash 3.2+. Pre-commit needs Bash
  4.4+ (`mapfile -d`), and the jj gate needs Bash 4+. The full hook test suite requires Bash
  4.4+; on macOS select a newer Bash through PATH. jj output is forced to plain text for all
  parsed commands, even with `ui.color="always"`.

The two dirty-tree checks (pre-commit's, when it runs cargo, and pre-push's, before its test gate) also refuse while an assume-unchanged or skip-worktree file is missing or differs from the index, since git diff cannot see that. Pre-commit looks only at the files cargo reads (Rust, Cargo and `desktop-slint/ui`); pre-push looks at every flagged file. A flagged file that still matches the index passes. The refusal says how to fix it. `scripts/mutants-diff.sh` makes the same check over every flagged file, since cargo-mutants copies the working tree. To bypass pre-commit's check, use `git commit --no-verify`, which skips all of commit-msg and pre-commit; to bypass pre-push's, use `SKIP_SLOW_GATES=1`, which skips its test gate. In a sparse checkout the out-of-cone files are missing, so pre-push refuses whenever it reaches its test gate, and pre-commit refuses when it runs cargo while any file cargo reads is out of the cone. Those refusals hold only while `scripts/` is in the cone: outside it the hooks are missing too, and git runs no gate at all.

The hooks run straight from the working tree, so an uncommitted edit to `scripts/hooks/` or `scripts/review-gate.sh` changes the gates at once.

After running the code-review skill, record the review:

```bash
scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"
scripts/review-gate.sh status    # commits since origin/main that still lack a review
```

The ledger keys each commit on the `git patch-id` of its diff, so a review survives a rebase or amend that leaves the diff unchanged. Repo scripts use `node`, not Python: ADR-0010 removed Python from dev and CI environments.

The push check (pre-push and `jj gpush`) skips commits already on `origin/main`. They are assumed reviewed, having landed through a PR, but a squash merge gives them a patch-id no ledger entry has, so a branch rebased onto `main` would otherwise be refused for `main`'s own commits.

## Jujutsu (jj)

The repo is a colocated jj repo (`jj git init --colocate`): `.git` stays, so GitHub, `gh`, CI and `git push` work unchanged. **jj never runs git hooks**, so `jj commit` and `jj git push` bypass everything above. Run `scripts/install-jj-aliases.sh` once per clone to set the repo-level jj config:

- `jj gate` runs `scripts/jj-gate.sh check` over `trunk()..@`: the commit-msg trailer ban on each description (it calls `scripts/hooks/commit-msg`), the home-path check on the stack's diff, workspace fmt and clippy when Rust, Cargo or `desktop-slint/ui` files changed since trunk, and `node --check` on changed `desktop-tauri/ui` and `scripts/` JavaScript files. jj has no index, so the gate checks the working-copy commit `@`, which every jj command snapshots first.
- `jj gpush [args]` asks `jj git push --dry-run [args]` what would move (it refuses to push when it cannot read that plan). For each pushed ref it runs the trailer ban over `trunk()..<new tip>`, commits already on the remote branch included, the home-path check on each commit it publishes, as pre-push does, and the review-gate ledger check over the commits on neither the remote branch nor `origin/main` (for a new branch, those since its fork point with `origin/main`). It then runs `cargo test --workspace` and `node --test` when `@` (or `@-` with `@` empty) is a pushed tip. It refuses when `@` has changes on top of the pushed `@-`, warns for other tips, then reads the plan again and pushes it with no new snapshot (below). Use it instead of `jj git push`. `SKIP_REVIEW_GATE=1` and `SKIP_SLOW_GATES=1` skip the same gates as before. `git push` still fires `scripts/hooks/pre-push`.
- `working-copy.eol-conversion = "input"` is set because `.gitattributes` normalizes CRLF to LF: git applies it on its own, jj shows every CRLF file as modified without it. Never use `input-output`; it writes CRLF into every checkout and breaks shell shebangs. jj also records the executable bit that `core.fileMode=false` hides from git.
- Nothing stops an AI trailer at `jj describe` time; `jj gate` and `jj gpush` refuse it afterwards. There is no counterpart to the hooks' untracked-file and hidden-edit checks: jj snapshots every non-ignored file into `@`, so the tests run on `@` as pushed. The push takes no new snapshot (`--ignore-working-copy`), so an edit made while the tests run is not pushed, though the tests may have seen it; and it refuses when the plan changed while the gates ran, including when another jj command snapshotted `@` meanwhile.
- Scope `jj restore` to paths (`jj restore <path>`); a bare `jj restore` also discards new files in `@`.

Under jj the ledger records commits, and `HEAD` is `@-` only once the work is committed (`jj commit`, or `jj describe` then `jj new`). Review and record `<base>..HEAD` (`review-gate.sh` is plain git and cannot resolve `@-`) after that, not `@` before it.
