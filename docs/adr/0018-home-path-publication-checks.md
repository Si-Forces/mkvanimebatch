# 0018. Check home identities before publishing through supported workflows

Status: accepted on 2026-11-16 for #220, #222, #226, #227, and #228.

The home-path guard prevents accidental publication of the checking environment's home identities.
Its existing identity, spelling, matching, and redaction rules apply across file contents,
added and renamed filenames, commit and annotated tag headers and messages, published ref names,
and prepared PR bodies.

Git hooks and explicit jj gates cover local Git publication. Agent-driven PR creation and body
updates must use a repository-owned command that checks the exact prepared PR body before invoking
gh; a read or scan failure stops publication. This adds a required publishing step in exchange for
checking descriptions before GitHub receives them. A check after publication cannot provide that
guarantee. Direct gh commands, browser edits, other API clients, and local gate bypasses remain
documented bypasses. Under `jj gpush`, `jj git push` chooses the tag object to send at push
time, leaving a brief concurrent-replacement race between the gate's final tag-object
re-check and the push itself.

New and updated refs must target commits, directly or through nested annotated tags. Guarded
publication refuses blob and tree targets before either SKIP setting; deletions remain allowed.
A commit-only history walk otherwise checks zero commits for those targets, and the later
review or test gates that normally refuse them can be skipped. Supporting noncommit publication
would need a separate scanning contract.

The local Git commit-msg hook checks the entire proposed message, including comments and
verbose diff context. Git can pass identical proposed bytes to the hook under cleanup modes
that either retain or discard those bytes, without exposing the CLI cleanup override to the
hook. We accept local false positives to avoid missing retained home spellings. The hook does
not strip comments or scissors blocks for the home-path check; the AI-trailer parser keeps its
separate scope. jj and push checks inspect stored message bytes. This avoids adding a mandatory
Git commit wrapper solely to reconstruct cleanup behavior.

See [the implementation plan](../home-path-guard.md) for the agreed scope and completion criteria,
and [scripts/CLAUDE.md](../../scripts/CLAUDE.md) for the currently shipped coverage.
