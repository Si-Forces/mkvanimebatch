#!/usr/bin/env bash
# Review-gate ledger for AGENTS.md's "always review before committing" rule.
#
# A git hook cannot run the two-axis Standards+Spec review itself: that needs the
# agent runtime. What it CAN enforce is that no commit reaches origin/main without a
# review having been recorded for it. This keeps the decision at push time instead of
# trusting the author's judgement at commit time, which is where the rule kept getting
# skipped: every skip was "this diff is too small to need a review", and small diffs
# are exactly what slip through.
#
# Entries are keyed by TREE sha, not commit sha, so the ledger survives rebases and
# amend-only changes that move commits without changing content.
#
# Usage:
#   scripts/review-gate.sh record "<note>"                 record HEAD
#   scripts/review-gate.sh record-range <base> <head> "…"  record base..head (base excluded)
#   scripts/review-gate.sh status                           show unpushed commits lacking review
#   scripts/review-gate.sh verify-range <base> <head>      exit non-zero if any is unreviewed
#
# Limitation, stated plainly: this catches forgetting, not dishonesty. `record` writes
# whatever it is told. It is a forcing function, not an oracle.

set -euo pipefail

LEDGER="${REVIEW_LEDGER:-$(git rev-parse --show-toplevel)/.git/review-ledger.jsonl}"
CMD="${1:-status}"

die() { echo "[review-gate] $*" >&2; exit 1; }

tree_of() { git rev-parse "$1^{tree}"; }

note_json() { printf '%s' "$*" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))'; }

append_entry() { # <sha> <note-json>
  mkdir -p "$(dirname "$LEDGER")"
  printf '{"head":"%s","tree":"%s","at":"%s","note":%s}\n' \
    "$1" "$(tree_of "$1")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$2" >> "$LEDGER"
}

# Membership test for a single tree sha, done entirely in one process.
#
# Two earlier implementations were wrong:
#   * `sed` scraping the tree field accepted an unterminated `{"tree":"<hash>`, so a
#     truncated ledger satisfied the gate for a commit that was never reviewed.
#   * piping that into `grep -qx` made the gate NON-DETERMINISTIC: grep exits on the
#     first match and closes the pipe, python then dies with BrokenPipeError, and under
#     `set -o pipefail` the result depends on timing. The same push was blocked on one
#     run and allowed on the next.
# Parsing and comparing in a single python process avoids both.
is_recorded() { # <tree-sha> -> 0 if recorded
  [ -f "$LEDGER" ] || return 1
  python3 - "$LEDGER" "$1" <<'PY'
import json, sys

want = sys.argv[2]
try:
    handle = open(sys.argv[1], encoding="utf-8")
except OSError:
    sys.exit(1)
with handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue  # a malformed record cannot satisfy the gate
        if isinstance(record, dict) and record.get("tree") == want:
            sys.exit(0)
sys.exit(1)
PY
}

record() {
  [ $# -ge 2 ] || die 'usage: review-gate.sh record "<note>"'
  append_entry HEAD "$(note_json "$2")"
  echo "[review-gate] recorded review for HEAD $(git rev-parse --short HEAD) (tree $(tree_of HEAD | cut -c1-7))"
}

# A range review covers every commit in the reviewed diff. `base` is NOT recorded: its
# changes are not part of base..head, which is what the review actually saw. This
# matches git's own A..B semantics; the first implementation used A~1..B and silently
# stamped one commit beyond the reviewed range.
record_range() {
  [ $# -ge 4 ] || die 'usage: review-gate.sh record-range <base> <head> "<note>"'
  local base="$2" head="$3" n=0 sha
  local note; note="$(note_json "$4")"
  while read -r sha; do
    append_entry "$sha" "$note"
    n=$((n + 1))
  done < <(git rev-list --reverse "$base".."$head")
  echo "[review-gate] recorded review for $n commit(s) in $base..$head"
}

status() {
  local base unreviewed=0 sha tree mark short
  if git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
    base=$(git rev-parse origin/main)
  else
    base=$(git rev-parse HEAD~1)
  fi
  printf 'unpushed commits vs %s:\n' "$(git rev-parse --short "$base")"
  while read -r sha; do
    tree=$(tree_of "$sha"); short=$(git rev-parse --short "$sha")
    if is_recorded "$tree"; then mark="reviewed"; else mark="NO REVIEW"; unreviewed=$((unreviewed + 1)); fi
    printf '  %-9s %-10s %s\n' "$short" "$mark" "$(git log -1 --format=%s "$sha")"
  done < <(git rev-list --reverse "$base"..HEAD)
  if [ "$unreviewed" -ne 0 ]; then
    printf '\n[review-gate] %d commit(s) unreviewed. Run the Standards+Spec review, then:\n' "$unreviewed"
    echo "  scripts/review-gate.sh record-range $(git rev-parse --short "$base") $(git rev-parse --short HEAD) \"<what you reviewed>\""
  fi
}

# Verifies the commits in base..head -- the range git is actually pushing -- rather
# than the checked-out HEAD, so pushing a side branch cannot be waved through by
# having main reviewed.
verify_range() {
  local base="$1" head="$2" sha tree
  git rev-parse --verify "$base" >/dev/null 2>&1 || die "unknown ref: $base"
  git rev-parse --verify "$head" >/dev/null 2>&1 || die "unknown ref: $head"
  while read -r sha; do
    tree=$(tree_of "$sha")
    if ! is_recorded "$tree"; then
      {
        echo ""
        echo "❌ [review-gate] refusing to push: $(git rev-parse --short "$sha") has no recorded review."
        echo "   $(git log -1 --format=%s "$sha")"
        echo ""
        echo "   Run the code-review skill (Standards + Spec, parallel sub-agents) over"
        echo "   $base..$head, fix or explicitly accept the findings, then:"
        echo "     scripts/review-gate.sh record-range $(git rev-parse --short "$base") $(git rev-parse --short "$head") \"<what you reviewed>\""
        echo ""
        echo "   Emergency override: SKIP_REVIEW_GATE=1 git push"
      } >&2
      exit 1
    fi
  done < <(git rev-list "$base".."$head")
  echo "[review-gate] all $(git rev-list --count "$base".."$head") pushed commit(s) reviewed"
}

case "$CMD" in
  record)       record "$@" ;;
  record-range) record_range "$@" ;;
  status)       status ;;
  verify-range) [ $# -ge 3 ] || die 'usage: review-gate.sh verify-range <base> <head>'
                verify_range "$2" "$3" ;;
  *)            die 'usage: review-gate.sh {record "<note>" | record-range <base> <head> "<note>" | status | verify-range <base> <head>}' ;;
esac