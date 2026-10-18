#!/usr/bin/env bash
# The jj counterpart of scripts/hooks: jj never runs git hooks, so the same gates run here.
# Wired up per clone by scripts/install-jj-aliases.sh as `jj gate` and `jj gpush`.
#
# Usage:
#   scripts/jj-gate.sh check              fast gates over trunk()..@ (the pre-commit + commit-msg checks)
#   scripts/jj-gate.sh push [push args]   review ledger + test suites, then `jj git push [push args]`
#
# jj has no index, so "what is being committed" is the working-copy commit @. Every jj command
# snapshots the tree first, so cargo and node below always see exactly @.
# Bypasses match the hooks: SKIP_REVIEW_GATE=1, SKIP_SLOW_GATES=1.
set -euo pipefail

die() { echo "❌ [jj-gate] $*" >&2; exit 1; }

command -v jj > /dev/null || die "jj is required"
root=$(jj workspace root) || die "not inside a jj workspace"
cd "$root"

# Does the commit-msg hook's job on each description in a revset (default trunk()..@), so the
# AI co-author trailer ban holds under jj too. A change with no description yet is skipped.
# Fails closed: an unreadable revset or description is a failure, not a pass. (This runs
# under `||`, where set -e is off, so every step checks its own status.)
check_messages() {
  local revset=${1:-'trunk()..@'} ids id msg fail=0
  ids=$(jj log -r "$revset" --no-graph -T 'commit_id ++ "\n"') ||
    { echo "❌ [jj-gate] cannot list the changes in $revset." >&2; return 1; }
  msg=$(mktemp)
  for id in $ids; do
    jj log -r "$id" --no-graph -T 'description' > "$msg" ||
      { echo "❌ [jj-gate] cannot read the description of commit ${id:0:12}." >&2; rm -f "$msg"; return 1; }
    [ -s "$msg" ] || continue
    if ! scripts/hooks/commit-msg "$msg"; then
      echo "   commit ${id:0:12}: fix it with: jj describe ${id:0:12}" >&2
      fail=1
    fi
  done
  rm -f "$msg"
  return "$fail"
}

# Keep the path globs, the theme-init.js rule and the test commands in sync with
# scripts/hooks/pre-commit and scripts/hooks/pre-push: both copies must gate the same files.
check() {
  local f fail=0 rust=0 input_type
  local -a changed js=()
  # Every file the stack touches since trunk, deletions included: removing a .rs file can
  # break the build as surely as editing one. -t, not the hook's -z: jj prints paths
  # unquoted, so only a path holding a newline would split (none in this repo).
  mapfile -t changed < <(jj diff --from 'trunk()' --to @ --name-only)
  wait $! # a process substitution's failure is otherwise ignored

  for f in "${changed[@]}"; do
    case "$f" in
      *.rs | Cargo.toml | */Cargo.toml | Cargo.lock) rust=1 ;;
      desktop-tauri/ui/*.js | scripts/*.js) [ -f "$f" ] && js+=("$f") ;; # skip deletions
    esac
  done

  check_messages || fail=1

  if [ "$rust" -eq 1 ]; then
    echo "[jj-gate] cargo fmt --check (workspace)..."
    cargo fmt --all -- --check || { echo "❌ [jj-gate] cargo fmt failed. Run: cargo fmt --all" >&2; fail=1; }
    echo "[jj-gate] cargo clippy -D warnings (workspace, all targets)..."
    cargo clippy --workspace --all-targets -- -D warnings || { echo "❌ [jj-gate] clippy failed." >&2; fail=1; }
  fi

  for f in "${js[@]}"; do
    # theme-init.js must stay a classic script (CSP, see its header); the rest are modules.
    case "$f" in */theme-init.js) input_type=commonjs ;; *) input_type=module ;; esac
    node --check --input-type="$input_type" < "$f" ||
      { echo "❌ [jj-gate] node --check failed: $f" >&2; fail=1; }
  done

  return "$fail"
}

# Full commit id of a revset that names one commit.
commit_id() { jj log -r "$1" --no-graph -T 'commit_id'; }

push() {
  local dry line u in_plan=0 deletes=0 kind name what remote_sha to_id
  local -a updates=() # "<remote sha, or 0 for a new ref> <local sha>", as the pre-push hook gets them

  [ -x scripts/review-gate.sh ] ||
    die "scripts/review-gate.sh is missing or not executable. Run: chmod +x scripts/review-gate.sh  (or jj restore scripts/review-gate.sh)"

  # jj decides what a bare `jj git push` moves, so ask it rather than re-deriving that.
  # The plan is the indented lines under `Changes to push to <remote>:`, each
  # `  bookmark: NAME [add to X]`, `[move forward from A to B]` or `[delete from A]` (tags
  # likewise); X, A and B are abbreviated commit ids. Fails closed: a plan line, or an output
  # with neither a plan nor "Nothing changed.", that this cannot read stops the push, so a
  # reworded jj never lets an unreviewed commit through.
  dry=$(jj git push --dry-run "$@" 2>&1) || { printf '%s\n' "$dry" >&2; exit 1; }
  while IFS= read -r line; do
    if [[ "$line" =~ ^Changes\ to\ push\  ]]; then in_plan=1; continue; fi
    [[ "$line" =~ ^[[:space:]] && "$in_plan" -eq 1 ]] || { in_plan=0; continue; }
    [[ "$line" =~ ^[[:space:]]+(bookmark|tag):[[:space:]]+(.+)[[:space:]]\[(.*)\]$ ]] ||
      die "cannot read this line of the push plan: $line. Bypass: jj git push"
    kind=${BASH_REMATCH[1]} name=${BASH_REMATCH[2]} what=${BASH_REMATCH[3]}
    case "$what" in
      "add to "*)               remote_sha=0; to_id=${what#add to } ;;
      "move "*" from "*" to "*) remote_sha=${what#*from }; remote_sha=${remote_sha%% to *}; to_id=${what##* to } ;;
      "delete "*)               deletes=$((deletes + 1)); continue ;; # nothing to review
      *) die "cannot read the push plan for $kind $name: [$what]. Bypass: jj git push" ;;
    esac
    [ "$remote_sha" = 0 ] || remote_sha=$(commit_id "$remote_sha") || die "cannot resolve $remote_sha ($kind $name)"
    to_id=$(commit_id "$to_id") || die "cannot resolve $to_id ($kind $name)"
    updates+=("$remote_sha $to_id")
  done <<< "$dry"

  if [ "${#updates[@]}" -eq 0 ]; then
    # Only deletions, or nothing to push. A plan header with no line read under it, or any
    # other output, is a plan this did not recognise.
    { [ "$deletes" -gt 0 ] || [[ "$dry" == *"Nothing changed."* && "$dry" != *"Changes to push"* ]]; } ||
      die "cannot read the output of jj git push --dry-run:"$'\n'"$dry"$'\n'"Bypass: jj git push"
  else
    # The trailer ban is part of the push, as commit-msg is part of every commit under git.
    for u in "${updates[@]}"; do
      check_messages "trunk()..${u#* }" || exit 1
    done
    if [ "${SKIP_REVIEW_GATE:-0}" = "1" ]; then
      echo "[review-gate] SKIP_REVIEW_GATE=1 — pushing unreviewed."
    else
      for u in "${updates[@]}"; do
        # shellcheck disable=SC2086 # two ids, split on purpose
        scripts/review-gate.sh verify-push $u || exit 1
      done
    fi
    slow_gates "${updates[@]}"
  fi

  jj git push "$@"
}

# The suites read the working tree, so they can only vouch for @ (or @- when @ is empty).
# Other pushed tips go through untested here, with a warning, as in the pre-push hook.
slow_gates() {
  if [ "${SKIP_SLOW_GATES:-0}" = "1" ]; then
    echo "[jj-gate] SKIP_SLOW_GATES=1 — skipping test suites."
    return
  fi
  local u tip cur parent empty_wc covered=0 # empty_wc: @'s id when @ has no changes, else empty
  cur=$(commit_id @) parent=$(commit_id '@-') empty_wc=$(jj log -r 'empty() & @' --no-graph -T 'commit_id')
  for u in "$@"; do
    tip=${u#* }
    if [ "$tip" = "$cur" ] || { [ "$tip" = "$parent" ] && [ -n "$empty_wc" ]; }; then covered=1; fi
  done
  for u in "$@"; do
    tip=${u#* }
    if [ "$tip" = "$cur" ] || { [ "$tip" = "$parent" ] && [ -n "$empty_wc" ]; }; then
      continue
    elif [ "$tip" = "$parent" ]; then
      # With another tip covered, the tests run on @, which holds this one too.
      [ "$covered" -eq 1 ] ||
        die "the working copy has changes on top of the pushed commit, so the tests would not run on it. Move them aside (jj new / jj squash) or: SKIP_SLOW_GATES=1 jj gpush"
    else
      echo "⚠  [jj-gate] ${tip:0:12} is not @ or @-; the local test suites do not cover it." >&2
    fi
  done
  [ "$covered" -eq 1 ] || return 0

  echo "[jj-gate] cargo test --workspace..."
  cargo test --workspace || die "cargo test failed."
  echo "[jj-gate] node --test scripts/*.test.js desktop-tauri/ui/*.test.js..."
  node --test scripts/*.test.js desktop-tauri/ui/*.test.js || die "node --test failed."
}

case "${1:-}" in
  check) check ;;
  push)  shift; push "$@" ;;
  *)     die 'usage: jj-gate.sh {check | push [jj git push args]}' ;;
esac
