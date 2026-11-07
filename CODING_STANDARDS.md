# CODING_STANDARDS.md

Judgement-call rules for reviewers: the code-review skill's Standards axis and the usual workflow's review agents. Mechanical rules belong in `jj gate` and CI, not here. Each rule cites the defect a review missed.

## Filesystem paths

- **Resolve every outcome.** Code that resolves a path (`canonicalize`, `symlink_metadata`, `read_link`, `resolve_spelling`) handles each outcome by name: exists, absent, dangling symlink, symlink loop, permission denied. Only a truly absent name may count as a folder still to be created; every other failure refuses with an error (fail closed). _#193: a dangling `--out-dir` symlink read as absent, so two items wrote one Sidecar._
- **Expected paths join per component.** A test that compares against a path the code returned builds the expected path with one `Path::join` per component (`tmp.join("Specials").join("chapters.xml")`). Creating fixtures from `"a/b"` strings is fine. _#176: red on Windows CI only._

## Tests

- **Every guard bites.** Each check, bound or branch the diff adds has a test that fails when that guard is removed. The implementer proves it with `scripts/mutants-diff.sh` and lists the surviving mutants in the PR notes. Flag a guard with no test reaching it, and a survivor with no stated reason. _#174's notes validation and #181's Tracks/Attachments bounds: every test still passed with the guard deleted._

## Spec

- **Deferred constraints are tracked.** A spec constraint the diff leaves unmet is either met or filed as an issue that names the lines; a PR body alone vanishes after merge. _#174: the Slint-only publishing constraint._
