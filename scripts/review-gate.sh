#!/usr/bin/env bash
# Review-gate ledger for AGENTS.md's "always review before committing" rule.
#
# A git hook cannot run the Standards+Spec review itself: that needs the agent runtime.
# What it CAN enforce is that no commit is pushed without a review recorded for it, which
# moves the check to push time, where "this diff is too small to review" cannot skip it.
#
# Entries are keyed by `git patch-id` of the commit's diff against its first parent. A
# review survives a rebase or amend that leaves the diff unchanged, but a different change
# that happens to land on a reviewed tree (a revert, say) still needs its own review. A
# commit with an empty diff has nothing to review and always passes.
#
# Uses node, not python (ADR-0010). JSON.parse also rejects NaN/Infinity.
#
# Usage:
#   scripts/review-gate.sh record "<note>"                   record HEAD
#   scripts/review-gate.sh record-range <base> <head> "…"    record base..head (base excluded)
#   scripts/review-gate.sh status                             commits not on origin/main lacking a review
#   scripts/review-gate.sh verify-push <remote-sha> <local-sha>   the pre-push check
#
# This catches forgetting, not dishonesty: `record` writes whatever it is told.

set -euo pipefail

die() { echo "[review-gate] $*" >&2; exit 1; }

command -v node > /dev/null || die "node is required"

# The common git dir is shared by every worktree of this clone.
LEDGER="${REVIEW_LEDGER:-$(git rev-parse --path-format=absolute --git-common-dir)/review-ledger.jsonl}"

require_commit() {
  git rev-parse --verify --quiet "$1^{commit}" ||
    die "$1 does not resolve to a commit (fetch first, or the remote tip is unknown)"
}

patch_id_of() { # <commit> -> patch id, empty when the commit changes nothing
  local parent
  parent=$(git rev-parse --verify --quiet "$1^") || parent=$(git hash-object -t tree /dev/null)
  git diff-tree -p --binary "$parent" "$1" | git patch-id --verbatim | cut -d' ' -f1
}

# Fork point from origin/main: where the commits that still need a review start.
review_base() { # <commit>
  git rev-parse --verify --quiet origin/main > /dev/null ||
    die "no origin/main; cannot determine a review base (fetch first)"
  git merge-base origin/main "$1" ||
    die "$1 shares no history with origin/main"
}

record_commit() { # <commit> <note-json> -> 1 when the diff is empty and nothing is recorded
  local sha patch
  sha=$(require_commit "$1") || exit 1
  # Callers test this in an `if`, where set -e is off.
  patch=$(patch_id_of "$sha") || die "cannot compute the patch id of $sha"
  [ -n "$patch" ] || return 1
  # Dies rather than returns: a failed write must not read as an empty diff.
  mkdir -p "$(dirname "$LEDGER")" || die "cannot create the directory for $LEDGER"
  printf '{"commit":"%s","patch":"%s","at":"%s","note":%s}\n' \
    "$sha" "$patch" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$2" >> "$LEDGER" ||
    die "cannot write the review ledger at $LEDGER"
}

is_reviewed() { # <commit> -> 0 if reviewed
  local patch
  # Callers test this in an `if`, where set -e is off: a failed patch-id must not read as
  # an empty diff and pass.
  patch=$(patch_id_of "$1") || return 1
  [ -n "$patch" ] || return 0
  [ -f "$LEDGER" ] || return 1
  # One node process per check: a malformed line is skipped, so it can never satisfy the gate.
  node -e '
const want = process.argv[2];
for (const line of require("fs").readFileSync(process.argv[1], "utf8").split("\n")) {
  let entry;
  try { entry = JSON.parse(line); } catch { continue; }
  if (entry && entry.patch === want) process.exit(0);
}
process.exit(1);
' "$LEDGER" "$patch"
}

note_json() {
  # `--` stops node from reading a note such as "--version" as its own option.
  node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' -- "$1"
}

record() {
  [ $# -ge 2 ] || die 'usage: review-gate.sh record "<note>"'
  local note
  note=$(note_json "$2")
  if record_commit HEAD "$note"; then
    echo "[review-gate] recorded review for HEAD $(git rev-parse --short HEAD)"
  else
    echo "[review-gate] HEAD has an empty diff; nothing to record"
  fi
}

# Records base..head with git's own A..B semantics: base itself was not part of the diff
# the review saw, so it is not recorded.
record_range() {
  [ $# -ge 4 ] || die 'usage: review-gate.sh record-range <base> <head> "<note>"'
  local base head note commits sha n=0
  base=$(require_commit "$2")
  head=$(require_commit "$3")
  note=$(note_json "$4")
  commits=$(git rev-list --reverse "$base..$head")
  for sha in $commits; do
    if record_commit "$sha" "$note"; then n=$((n + 1)); fi
  done
  echo "[review-gate] recorded review for $n commit(s) in $2..$3 (empty diffs need none)"
}

status() {
  local base commits sha mark unreviewed=0
  base=$(review_base HEAD)
  commits=$(git rev-list --reverse "$base..HEAD")
  printf 'commits since the fork point with origin/main (%s):\n' "$(git rev-parse --short "$base")"
  for sha in $commits; do
    if is_reviewed "$sha"; then mark="reviewed"; else mark="NO REVIEW"; unreviewed=$((unreviewed + 1)); fi
    printf '  %-9s %-10s %s\n' "$(git rev-parse --short "$sha")" "$mark" "$(git log -1 --format=%s "$sha")"
  done
  if [ "$unreviewed" -ne 0 ]; then
    printf '\n[review-gate] %d commit(s) unreviewed. Run the Standards+Spec review, then:\n' "$unreviewed"
    echo "  scripts/review-gate.sh record-range $(git rev-parse --short "$base") HEAD \"<what you reviewed>\""
  fi
}

# Checks the commits git is actually pushing, not the checked-out HEAD, so pushing a side
# branch cannot be waved through by having main reviewed. A new remote branch (all-zero
# remote sha) is checked from its fork point with origin/main. Fails closed on any
# endpoint it cannot resolve.
verify_push() { # <remote-sha> <local-sha>
  local base head commits sha n=0
  head=$(require_commit "$2")
  if [[ "$1" =~ ^0+$ ]]; then base=$(review_base "$head"); else base=$(require_commit "$1"); fi
  # An assignment, not a for-list substitution, so a failed rev-list trips set -e.
  commits=$(git rev-list "$base..$head")
  for sha in $commits; do
    if ! is_reviewed "$sha"; then
      {
        echo ""
        echo "❌ [review-gate] refusing to push: $(git rev-parse --short "$sha") has no recorded review."
        echo "   $(git log -1 --format=%s "$sha")"
        echo ""
        echo "   Run the code-review skill (Standards + Spec, parallel sub-agents) over"
        echo "   $(git rev-parse --short "$base")..$(git rev-parse --short "$head"), fix or explicitly accept the findings, then:"
        echo "     scripts/review-gate.sh record-range $(git rev-parse --short "$base") $(git rev-parse --short "$head") \"<what you reviewed>\""
        echo ""
        echo "   Emergency override: SKIP_REVIEW_GATE=1 git push"
      } >&2
      exit 1
    fi
    n=$((n + 1))
  done
  echo "[review-gate] all $n pushed commit(s) reviewed"
}

case "${1:-status}" in
  record)       record "$@" ;;
  record-range) record_range "$@" ;;
  status)       status ;;
  verify-push)  [ $# -ge 3 ] || die 'usage: review-gate.sh verify-push <remote-sha> <local-sha>'
                verify_push "$2" "$3" ;;
  *)            die 'usage: review-gate.sh {record "<note>" | record-range <base> <head> "<note>" | status | verify-push <remote-sha> <local-sha>}' ;;
esac
