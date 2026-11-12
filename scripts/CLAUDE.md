# scripts/CLAUDE.md

How the git hooks, the review-gate ledger and the jj gates work. The rules for using them are in the root `AGENTS.md` ("Workflow rules").

## Git hooks and the review ledger

Enable the tracked hooks once per clone with `scripts/install-git-hooks.sh`, which sets `core.hooksPath` to `scripts/hooks` (worktrees share it):

- commit-msg refuses a message whose trailer block credits an AI agent (Claude, Copilot, `AI Bot`). It parses trailers with `git interpret-trailers`, so only real trailers count: a prose mention or a `git commit -v` context line naming one is not blocked, which keeps the commit that documents this hook committable. A co-author naming a person, including the maintainer's own `sifahaz <...@users.noreply.github.com>` line, is left alone. It refuses rather than rewriting the message, so dropping the attribution is visible.
- pre-commit runs workspace fmt and clippy when Rust, Cargo or `desktop-slint/ui` files are staged, and `node --check` on staged `desktop-tauri/ui` and `scripts/` JavaScript files. It refuses to run cargo while any of those files differs from the index, so the check matches the commit. It runs the home-path check (below) on the staged diff first, so a refusal shows even when the dirty-tree check then stops the hook.
- pre-push runs the home-path check on each commit it publishes (those on no remote-tracking ref, one at a time, so a path added and then removed still counts), then the **review-gate ledger** check on every pushed ref, refusing the push if any pushed commit not already on `origin/main` has no recorded review (skip it with `SKIP_REVIEW_GATE=1`). When HEAD is one of the pushed tips, it then runs `cargo test` and `node --test`, refusing if the working tree differs from HEAD, untracked Rust, Cargo, UI or `scripts/` files included (skip them with `SKIP_SLOW_GATES=1`). Other pushed tips, such as a second branch or an older tag, get only a warning. If HEAD is not pushed at all, that warning is the whole local test coverage.

`scripts/hooks/home-path.sh` is the **home-path check**. An agent that copies an absolute path from its session would publish the committer's username and folder layout, and once pushed, an amend or force-push cannot take it back: GitHub keeps the old commit until GitHub Support purges it. So the check refuses any added line in a diff that names the committer's own home:

- **Coverage:** local Git commits and pushes run it once the hooks are installed and
  `core.hooksPath` still selects `scripts/hooks`. Under jj, run `jj gate` before committing
  and use `jj gpush` to push; `jj commit` itself runs no gate. The identities come from the
  checking process's `HOME` and `USERPROFILE`, so another machine's home is not covered.
  [Cloud sessions](../docs/agents/cloud-sessions.md) install no hooks and have no enforced
  home-path check before publication; their maintainer check runs afterwards.
- **What counts:** `$HOME`, its resolved form, and `USERPROFILE` (with either slash, or the doubled backslashes of a TOML or JSON string), each also in the dashed spelling Claude Code gives a working directory in its scratchpad and memory paths (`-home-<user>-repo`, one `-` for each UTF-16 unit that is not an ASCII letter or digit, so two for an emoji), where it starts a path or follows a separator. Only the committer's home counts, so generic test paths such as `/home/user/show` pass.
- **How it matches:** wherever the home starts (as in `-I/home/<user>/include`), up to the end of a path segment, in any ASCII case. A full stop or ellipsis that ends a sentence, or the `..` of a git range, still ends the path. `\/` and `\\/` are read as `/`, and a run of slashes as one. The line is scanned with slash runs squeezed and with `\/` decoded, so a literal backslash before a slash in a POSIX home still matches even when the path repeats the slash. The finer rules are in the script's comments.
- **No home to look for:** a home of one segment, such as `/root` or `C:\app`, is the same on every machine and names nobody, so it is not looked for. When neither variable names a home (unset, `/`, a drive alone, or one segment) the check only warns: in a container or CI job whose `HOME` is `/root`, nothing is checked, and that warning is the only sign. A `HOME` it cannot resolve (a dangling symlink, a symlink loop, a folder it cannot enter or search) refuses.
- **The report:** each hit as `file:line`, or `<commit> file:line`, never the line's text,
  because gate output gets pasted into PR descriptions, which nothing checks. Recognized
  literal and dashed home occurrences in the original file name are replaced by `<home>`, ignoring
  ASCII case; other parts keep git's spelling (see Limits). Fix a refusal with a relative
  path, `~` or a placeholder such as `<repo>`, then rewrite the commit that adds it.
- **Where it runs:** pre-commit and `jj gate` check what is being committed: pre-commit runs `home-path.sh --staged`, which reads the staged diff itself, and `jj gate` pipes in the stack's diff from its fork point. pre-push and `jj gpush` run `home-path.sh --published <sha>` (a full commit id), which runs `git log` itself over each commit in `<sha>` that no remote-tracking ref holds, one at a time, so a path added and then removed still counts. A merge counts for the lines its resolution adds, which no parent has. pre-commit is stricter: when a conflicted merge is concluded with `git commit`, it diffs the index against `HEAD`, so a line the other parent brings in counts as added even when it is already published; `git commit --no-verify` is the way past it.
- **What git settings cannot change:** `--staged` and `--published` run git with one list of flags (`--text`, `--no-color`, `--no-ext-diff`, `--no-textconv`, `--no-relative`, the `a/` `b/` prefixes) and `--no-replace-objects`, and `--published` also with `--root` and no graft file, so forced colour, an external diff, a textconv driver, a `-diff` or `binary` attribute, a NUL byte that makes git take a file for binary, `diff.noprefix`, `git replace`, `info/grafts`, `log.showRoot=false` or `diff.relative` (which would drop each file outside the folder git runs from) can neither hide a line nor misname a file, but for a merge resolution in a binary file (see Limits). `jj diff` reads none of those settings, and has no `--text` of its own: for a file jj takes for binary it prints only that the files differ, so `jj gate` does not read one (see Limits). `jj gpush` does read it, because it runs `--published`.
- **Bypasses:** with the hooks installed and active, the check runs before
  `SKIP_REVIEW_GATE` and `SKIP_SLOW_GATES` are read, so neither skips it.
  `git commit --no-verify` (also `-n`), `git push --no-verify` and plain `jj git push` bypass
  the respective gates. An unconfigured clone, a redirected `core.hooksPath` or edited hooks
  can also avoid the check. With no qualifying home, the check runs but only warns (above).
- **Limits:** it does not reject file names alone, commit or tag messages (#220), or text
  outside git such as PR descriptions. `--published` does not read what a merge resolution
  adds to a file git takes for binary (#222), since `git log -c` ignores `--text`; pre-commit
  reads it when the merge is concluded with `git commit`. `jj gate` reads no file jj takes
  for binary at all, since `jj diff` has no `--text` (#222); `jj gpush` reads it. Of the Windows
  spellings it misses mixed separators inside the home (`C:\Users/<user>/x`), the WSL form
  (`/mnt/c/Users/<user>/x`), and backslashes escaped twice (`C:\\\\Users\\\\<user>`, four per
  separator). A flag or CSS modifier that begins with the dashed spelling (`--<a>-<b>` for a
  home `/<a>/<b>`) is refused. It misses a home with a space written as `%20` or `\ `, or a `/`
  written as `%2F` or `\u002f`, and refuses siblings such as `<home>+x`, `<home>./x`,
  `<home>..x` or `<home>é/x`; the ASCII boundary heuristic favors refusal. A file name git
  quotes (non-ASCII, or one holding a tab or quote) stays quoted; encoded home spellings in
  that name may remain visible. A commit any remote-tracking ref holds counts as published,
  so with a second remote, a commit pushed there first is not checked when it later goes to
  GitHub. A CI checkout or fetch can likewise make `--published <sha>` scan nothing; it is
  not a scan of already published history. A clone of the pre-scrub history, or one that
  fetches its old commits, still holds the old path; the current rewritten history does not.
  The old commits stay on GitHub until GitHub Support purges them.
- **Shell support:** the standalone checker supports Bash 3.2. The real pre-commit hook uses
  `mapfile -d`, requiring Bash 4.4 or newer, and the jj gate uses `mapfile`, requiring Bash 4
  or newer. The full `home-path.test.js` suite runs these hooks and therefore needs Bash 4.4
  or newer; on macOS select a newer Bash through `PATH` before running it.

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
