# Finish the home-path guard issues

Status: design confirmed on 2026-11-16; implementation follows the completion criteria below.
This document describes intended work, not shipped coverage.
See [scripts/CLAUDE.md](../scripts/CLAUDE.md) for the current contract and
[the glossary](../scripts/GLOSSARY.md) for terms.

## Confirmed decisions

- Finish the five open home-path issues: [#220](https://github.com/Si-Forces/mkvanimebatch/issues/220),
  [#222](https://github.com/Si-Forces/mkvanimebatch/issues/222),
  [#226](https://github.com/Si-Forces/mkvanimebatch/issues/226),
  [#227](https://github.com/Si-Forces/mkvanimebatch/issues/227), and
  [#228](https://github.com/Si-Forces/mkvanimebatch/issues/228).
  The review-ledger follow-up, #221, is separate work.
- Retain the existing home identities and supported spellings derived from the checking
  process's HOME and USERPROFILE. The existing identity qualification, matching, and redaction
  rules remain the basis for the new checks.
- An issue is finished after its fix, regression coverage, applicable mutation checks,
  documentation, and review are complete, and its PR has merged and the issue is closed.
- Agent-driven PR creation and body updates must use a repository-owned publishing command.
  It checks the exact prepared PR body before invoking gh. Direct gh commands, browser edits,
  and other API clients remain documented bypasses. See
  [ADR-0018](adr/0018-home-path-publication-checks.md) for this workflow boundary.
- Push checks also inspect published annotated tag messages.
- Fix both the local jj gate binary-content gap and push-time binary merge coverage.
  Published binary merge checks accept lines already present in a corresponding parent file,
  including a line moved to a different position by the resolution.
- New and updated refs must target commits, directly or through nested annotated tags.
  Refuse blob and tree targets before either SKIP setting. Ref deletions remain allowed.
- The local Git commit-msg hook checks the entire proposed message, including comments and
  verbose diff context that Git may later discard. This deliberately accepts local false
  positives because the hook cannot reliably infer CLI cleanup overrides. jj and push checks
  inspect stored messages. This policy was explicitly agreed during the design interview.

## Design constraints

- Use one identity, matching, and redaction contract for the new input types. Add explicit
  text and name inputs without treating arbitrary metadata as diff hunks.
- Preserve bounded streaming, arbitrary-byte handling, and refusal on unreadable inputs,
  scanner errors, or unsafe identity resolution. Reports identify locations without echoing
  private text or unredacted names.
- Preserve each check's comparison: staged Git uses index versus HEAD; jj gate uses the net
  diff from the fork point; published merges compare content with each parent. Existing text
  hunks keep Git's combined-diff semantics, including conservative moved-line false positives.
  The new binary merge fallback accepts matching raw lines already held by a parent file.
  Changed binary files must not be treated as entirely added: inherited or unchanged content
  alone is not a hit.
- Do not strip comments or scissors blocks before the Git hook's home-path check. Identical
  proposed bytes can be retained or discarded under different cleanup options that the hook
  does not receive. Run the home-path check before the AI-trailer parser can echo an offending
  trailer; keep that parser's separate scope intact. Make unreadable-message errors safe too.
  Test raw draft refusal under strip, whitespace, verbatim, editor, and verbose modes, including a
  retained home spelling after a literal scissors marker. jj and publication checks use the
  stored message bytes without display-encoding transformations.
- Inspect every newly published annotated tag message in a nested chain, including a new annotation
  pointing to an already-known commit. Commit-range traversal alone does not cover this.
- Capture the final prepared PR body, check it, and publish those same bytes. Any footer is
  part of that body before checking. Body generation or editing after checking would break
  the guarantee.
- Document the checked PR command in AGENTS.md and the issue-tracker guide. Preserve the usual
  workflow worker's publication prohibition and direct its handoff to the checked command.
  Direct REST fallbacks from external skills remain bypasses; global skills are not changed.
- Every new Git object read ignores replace refs and grafts, as the existing home-path reads
  do: use --no-replace-objects and GIT_GRAFT_FILE=/dev/null. This covers message, tag, tree,
  blob, and baseline reads; it does not expand this work into #221's review-ledger changes.
- Every new Git diff read, including name-status/raw listings, uses the applicable shared
  protective flags. Patch reads reuse the complete diff_flags contract: zero context, forced
  text, no color, no relative paths, no external diff or textconv, and fixed prefixes. Path
  listings keep their NUL protocol while neutralizing relative paths, color, and diff drivers.
  Test hostile settings and mutation witnesses on every new adapter. Include direct --staged
  and --published scans from a subdirectory with diff.relative=true and a home-named zero-byte
  addition outside that subdirectory, so a missing --no-relative cannot hide the filename.
- Check names before emitting diagnostics that could contain them. In particular, do not
  echo an unchecked jj push plan or raw command error containing names or source-file paths.

## Input adapters and binary deltas

- **Filenames:** use a separate byte-preserving, NUL-delimited path input from name-status/raw
  changes, or an equivalent jj path source. Diff hunk headers alone miss empty additions and
  pure renames and also describe ordinary modifications. Keep filename delimiters outside the
  content NUL stripper. Apply the existing leading-slash convention when matching repo-relative
  names; safely quote and redact reports with a :filename marker. Published merges check names
  introduced relative to every parent, retaining the distinction from local net-diff checks.
- **Ref names:** the checked value is the full destination ref. Git supplies that name;
  expand jj bookmark and tag plan names to refs/heads/<name> and refs/tags/<name>. Include the
  case where HOME is /home/<user> and the short bookmark is home/<user>/topic, so both push
  workflows reject the same destination. Local source names do not determine the check.
- **jj binary content:** resolve the same fork-point baseline and checked snapshot already
  used by the local net diff. In the colocated Git store, derive forced-text binary deltas
  between those snapshots, rather than scanning the target blob in full or calling
  --published on the whole stack. Verify snapshot fidelity against real jj binary fixtures.
  If the baseline is not singular, a needed object is unreadable, or binary conflict state
  cannot be represented faithfully, refuse instead of silently checking less. Keep refusal
  safe for private paths.
- **Published binary merges:** stream forced-text, zero-context candidate additions against
  a parent. For a home-identity hit, check the exact raw candidate line against corresponding
  parent-file contents through streamed reads, independent of its position. Report it only
  if no parent file already holds it. Intersecting diff coordinates alone is insufficient:
  moving a line can make every parent diff call it an addition. Keep memory bounded by the
  current lines; use only private coordinate scratch records, never whole blobs, diffs, or
  whole-file line sets. Keep ordinary published history batched and pay for the binary
  fallback only where needed. Measure repeated and many-candidate cases to avoid excessive
  per-line subprocesses or repeated work. Test copied and moved inherited lines, an unchanged
  home-bearing binary prefix with a clean addition, genuinely introduced lines, octopus
  merges, and the local net-diff case where an intermediate leak was removed. Extend the
  manual binary benchmark to check memory growth and elapsed time, without a CI timing gate.
- **PR bodies:** expose explicit prepared-body input for create and body updates. Capture the
  input once into a private snapshot, validate it, then forward the same bytes through
  gh --body-file - without rereading the source path or holding the whole body in memory.
  Preserve trailing newlines. Reject duplicate/conflicting body options and modes that could
  generate or edit unchecked text, including --fill*, --editor, --web, --recover, and bodyless
  creation. Reject -T/--template under the same rule. Creation requires an explicit title
  and --head identifying an already-pushed branch; disable interactive prompts and automatic
  branch publication. Edits are non-interactive too. Verify this invocation with the stub
  publisher. The title passes through without identity validation. Check the final footer too.
  The guarantee covers descriptions; PR-title checking
  is outside the agreed five-issue description scope.
- **Tag annotations:** traverse newly published tag objects independently of commit ranges,
  preserving the destination's old-object boundary and checking all new nested annotations.
  Test an update from an old inner annotation to a new outer annotation at the same destination:
  the new outer body is checked and the old inner body stays within the trusted old boundary.
  Exercise nested annotation objects through real Git pre-push. jj tests cover the actual
  bookmark/tag publication capabilities of the installed jj; do not claim jj creates or
  preserves annotations it does not support.

## Required outcomes from the issues

- **#220 — commit messages:** check messages in the Git commit hook, jj gate, and both push
  workflows, including messages in commits created through hook bypasses. Reports identify
  the message line without printing its contents. Also check published annotated tag messages.
- **#222 — binary content:** detect home-path lines introduced by binary merge resolutions,
  while accepting lines already present in a parent. Fix and test the jj gate binary-content
  gap too.
- **#226 — filenames:** check added and renamed destination paths, including pure renames.
  Deletions and unchanged contextual filenames do not become hits. Preserve arbitrary filename
  handling and redact reports.
- **#227 — PR descriptions:** validate prepared bodies before both creation and updates.
  Clean text reaches publication unchanged; scan or read errors stop publication. Agent-driven
  creation and body updates go through the repository-owned publishing command.
- **#228 — ref names:** check new or updated destination branch, bookmark, and tag names before
  either SKIP variable can take effect. Allow deletions and clean destination names, and redact
  diagnostics.

## Confirmation

The user confirmed the shared design after the council review and resolved the whole-draft
commit-hook policy during that review.

## Delivery and evidence

Start with the reusable text input needed by message checks and PR publishing. Keep issue
acceptance criteria visible as filename, ref-name, binary, and publication wiring are added;
engineering can choose the smallest sequence of focused changes that satisfies them.

For each new guard, add a regression that fails with that guard removed. Include real Git
and available jj gate tests, binary merge acceptance and refusal, pure renames, destination
ref updates and deletions, zero-byte additions, and nested annotations. For #228, exercise
Git pre-push and jj gpush against isolated bare remotes, with neither SKIP setting, each one
independently, and both together. Include blob- and tree-target pushes, including nested tags
ending at those targets, refused before SKIP settings. Prove that a refused destination
name leaves the remote absent or unchanged, clean updates and deletions pass, and removing
each new guard makes its regression fail. A fake publication adapter must prove that a refused
or unreadable PR body causes no GitHub write, while a clean body is unchanged. Include source
mutation after capture, conflicting body flags, unsafe error paths, and replace/graft attempts
to substitute clean messages, annotations, or binary content. Assert that a home-bearing AI
trailer never reaches the trailer parser's raw diagnostic before the home-path refusal.
Update affected exact mutation anchors and add cases for new guard wiring. Run the required
Node checks and suites; preserve the existing memory contract and inspect any changed
streaming paths for regressions. Follow the repository's Standards + Spec review and review
ledger workflow before publishing fixes.

The agreed scope and acceptance criteria govern implementation and review.
