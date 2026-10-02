#!/usr/bin/env bash
# Points git at the tracked hooks in scripts/hooks. Run once per clone; worktrees share it.
#
# The hooks run straight from the working tree, so an uncommitted edit to scripts/hooks or
# scripts/review-gate.sh changes the gates immediately.
set -euo pipefail

root=$(git rev-parse --show-toplevel)

# git ignores a missing or non-executable hook (at most with a hint), so check before
# claiming anything.
for f in scripts/hooks/pre-commit scripts/hooks/pre-push scripts/review-gate.sh; do
  if [ ! -x "$root/$f" ]; then
    echo "$f is missing or not executable; refusing to enable the hooks." >&2
    echo "Run: chmod +x $f  (or git checkout -- $f if it was deleted)" >&2
    exit 1
  fi
done
# Sourced by both hooks rather than run, so it only has to exist.
if [ ! -f "$root/scripts/hooks/hidden-changes.sh" ]; then
  echo "scripts/hooks/hidden-changes.sh is missing; refusing to enable the hooks." >&2
  echo "Run: git checkout -- scripts/hooks/hidden-changes.sh" >&2
  exit 1
fi

current=$(git config --get core.hooksPath || true)
if [ -n "$current" ] && [ "$current" != "scripts/hooks" ]; then
  echo "core.hooksPath is already '$current'; refusing to replace it." >&2
  exit 1
fi
git config core.hooksPath scripts/hooks

# core.hooksPath makes git ignore .git/hooks, including copies an older installer left there.
legacy=()
for hook in "$(git rev-parse --path-format=absolute --git-common-dir)"/hooks/*; do
  [[ -f "$hook" && "$hook" != *.sample ]] && legacy+=("$(basename "$hook")")
done
if [ "${#legacy[@]}" -gt 0 ]; then
  echo "Note: these hooks in .git/hooks no longer run: ${legacy[*]}"
  echo "      Delete them, or move any you still need into scripts/hooks."
fi

cat <<'MSG'
Hooks enabled (core.hooksPath = scripts/hooks):
  pre-commit : cargo fmt --check + clippy -D warnings when Rust or Cargo files are staged,
               node --check on staged UI modules
  pre-push   : the review-gate ledger check, then cargo test --workspace and node --test
               when HEAD is pushed (other pushed tips only get a warning)

Both also refuse while an assume-unchanged or skip-worktree file differs from the index,
since git diff cannot see that edit. The refusal lists the files and how to clear the bits.
To bypass: git commit --no-verify (skips all of pre-commit), SKIP_SLOW_GATES=1 git push.

Review a range with the code-review skill (Standards + Spec, parallel
sub-agents), then record it before pushing:
  scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"

Check what is still unreviewed:  scripts/review-gate.sh status
MSG
