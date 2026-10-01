#!/usr/bin/env bash
# Review-gate ledger for AGENTS.md's "always review before committing" rule.
#
# A git hook cannot run the two-axis Standards+Spec review itself: that needs the
# agent runtime. What it CAN enforce is that no commit reaches origin/main without a
# review having been recorded for it. This keeps the ledger honest rather than
# trusting the author's judgement at commit time, which is where the rule kept
# getting skipped.
#
# The ledger is keyed by TREE sha, not commit sha, so it survives rebases and
# amends that change only the messages. Recording is deliberately cheap so there is
# no incentive to skip it.
#
# Usage:
#   scripts/review-gate.sh record "<note>"   record that HEAD has been reviewed
#   scripts/review-gate.sh status             show which unpushed commits lack review
#   scripts/review-gate.sh verify [ref]       exit non-zero if any commit is unreviewed
#
# Limitation, stated plainly: this catches forgetting, not dishonesty. `record`
# writes whatever you tell it. It is a forcing function, not an oracle.

set -euo pipefail

LEDGER="${REVIEW_LEDGER:-$(git rev-parse --show-toplevel)/.git/review-ledger.jsonl}"
CMD="${1:-status}"
NOTE="${2:-}"

tree_of() { git rev-parse "$1^{tree}"; }

recorded_trees() {
  [ -f "$LEDGER" ] || return 0
  sed -n 's/.*"tree"[[:space:]]*:[[:space:]]*"\([0-9a-f]*\)".*/\1/p' "$LEDGER"
}

record() {
  local head tree
  head=$(git rev-parse HEAD)
  tree=$(tree_of HEAD)
  mkdir -p "$(dirname "$LEDGER")"
  printf '{"head":"%s","tree":"%s","at":"%s","note":%s}\n' \
    "$head" "$tree" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(printf '%s' "$NOTE" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
    >> "$LEDGER"
  echo "[review-gate] recorded review for HEAD ${head:0:7} (tree ${tree:0:7})"
}

# A range review covers every commit inside it, so record them all. This is the normal
# case: the code-review skill is always run over a diff, never over a single commit.
record_range() {
  local from="$1" to="$2" n=0 sha tree note_json
  note_json=$(printf '%s' "$NOTE" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
  mkdir -p "$(dirname "$LEDGER")"
  while read -r sha; do
    tree=$(tree_of "$sha")
    printf '{"head":"%s","tree":"%s","at":"%s","note":%s}\n' \
      "$sha" "$tree" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$note_json" >> "$LEDGER"
    n=$((n+1))
  done < <(git rev-list --reverse "$from"~1.."$to")
  echo "[review-gate] recorded review for $n commit(s) in $from..$to"
}

status() {
  local base unreviewed=0
  base=$(git rev-parse --verify --quiet origin/main >/dev/null 2>&1 && git rev-parse origin/main \
         || git rev-parse HEAD~1)
  printf 'unpushed commits vs %s:\n' "$(git rev-parse --short "$base")"
  while read -r sha; do
    local tree short mark
    tree=$(tree_of "$sha"); short=$(git rev-parse --short "$sha")
    if recorded_trees | grep -qx "$tree"; then mark="reviewed"; else mark="NO REVIEW"; unreviewed=$((unreviewed+1)); fi
    printf '  %-9s %-10s %s\n' "$short" "$mark" "$(git log -1 --format=%s "$sha")"
  done < <(git rev-list --reverse "$base"..HEAD)
  [ "$unreviewed" -eq 0 ] || printf '\n[review-gate] %d commit(s) unreviewed. Run the Standards+Spec review, then: scripts/review-gate.sh record "note"\n' "$unreviewed"
  return 0
}

verify() {
  local base sha tree
  base="${1:-origin/main}"
  git rev-parse --verify "$base" >/dev/null 2>&1 || { echo "[review-gate] unknown ref: $base" >&2; exit 1; }
  while read -r sha; do
    tree=$(tree_of "$sha")
    if ! recorded_trees | grep -qx "$tree"; then
      echo "" >&2
      echo "❌ [review-gate] refusing to push: $(git rev-parse --short "$sha") has no recorded review." >&2
      echo "   $(git log -1 --format=%s "$sha")" >&2
      echo "" >&2
      echo "   Run the code-review skill (Standards + Spec, parallel sub-agents) over" >&2
      echo "   $base..HEAD, fix or explicitly accept the findings, then:" >&2
      echo "     scripts/review-gate.sh record \"<what you reviewed>\"" >&2
      echo "" >&2
      echo "   Override for an emergency with: SKIP_REVIEW_GATE=1 git push" >&2
      exit 1
    fi
  done < <(git rev-list "$base"..HEAD)
  echo "[review-gate] all $(git rev-list --count "$base"..HEAD) unpushed commit(s) reviewed"
}

case "$CMD" in
  record)       [ -n "$NOTE" ] || { echo "usage: review-gate.sh record \"<note>\"" >&2; exit 1; }; record ;;
  record-range) [ -n "$NOTE" ] || { echo "usage: review-gate.sh record-range <from> <to> \"<note>\"" >&2; exit 1; }
                 record_range "$2" "$3" ;;
  status)       status ;;
  verify)       verify "${2:-}" ;;
  *)            echo "usage: review-gate.sh {record \"<note>\" | record-range <from> <to> \"<note>\" | status | verify [ref]}" >&2; exit 1 ;;
esac