#!/usr/bin/env bash
# The jj counterpart of scripts/hooks: jj never runs git hooks, so the same gates run here.
# Wired up per clone by scripts/install-jj-aliases.sh as `jj gate` and `jj gpush`.
#
# Usage:
#   scripts/jj-gate.sh check              fast gates over trunk()..@ (the pre-commit + commit-msg checks, home-path included)
#   scripts/jj-gate.sh push [push args]   home-path check + review ledger + test suites, then `jj git push [push args]` with no new snapshot
#   scripts/jj-gate.sh --decode-name <raw>  test-only entry point for decode_jj_name
#
# jj has no index, so "what is being committed" is the working-copy commit @. Every jj command
# snapshots the tree first, so the gates check @ as of that snapshot; the push reads its plan and
# pushes without a new one, so an edit made while the tests run is not pushed (though the tests
# may have seen it).
# Bypasses match the hooks: SKIP_REVIEW_GATE=1, SKIP_SLOW_GATES=1.
set -euo pipefail

die() { echo "❌ [jj-gate] $*" >&2; exit 1; }

# Decodes a jj-quoted bookmark, tag, or remote name in `jj git push --dry-run` into REPLY,
# failing closed on malformed quotes or unsupported escape sequences. Git ref names cannot
# contain control characters, so only \" and \\ are decoded; any other escape fails closed.
decode_jj_name() {
  local raw=$1
  if [[ "$raw" != \"* ]]; then
    [[ -n "$raw" && "$raw" != *\"* ]] || return 1
    REPLY=$raw
    return 0
  fi
  [[ "$raw" == \"*\" && ${#raw} -ge 2 ]] || return 1
  local inner=${raw:1:${#raw}-2}
  local out="" i=0 len=${#inner} ch next
  while [ "$i" -lt "$len" ]; do
    ch=${inner:i:1}
    if [ "$ch" = "\"" ]; then
      return 1
    elif [ "$ch" = "\\" ]; then
      i=$((i + 1))
      [ "$i" -lt "$len" ] || return 1
      next=${inner:i:1}
      case "$next" in
        "\"" | "\\") out+=$next ;;
        *) return 1 ;;
      esac
    else
      out+=$ch
    fi
    i=$((i + 1))
  done
  [ -n "$out" ] || return 1
  REPLY=$out
}
if [ "${1:-}" = --decode-name ]; then
  [ $# -eq 2 ] || exit 1
  decode_jj_name "$2" || die "cannot decode destination name"
  printf '%s' "$REPLY"
  exit 0
fi

command -v jj > /dev/null || die "jj is required"
# Every parsed command (ids, descriptions, paths and push plans) must ignore forced colour.
jj() { command jj --color never "$@"; }
root=$(jj workspace root 2> /dev/null) || die "not inside a jj workspace"
cd "$root"

# Does the commit-msg hook's job on each description in a revset (default trunk()..@), so the
# AI co-author trailer ban holds under jj too. A change with no description yet is skipped.
# Fails closed: an unreadable revset or description is a failure, not a pass. (This runs
# under `||`, where set -e is off, so every step checks its own status.)
check_messages() {
  local revset=${1:-'trunk()..@'} trailer_only=${2:-0} ids id msg fail=0
  ids=$(jj log -r "$revset" --no-graph -T 'commit_id ++ "\n"') ||
    { echo "❌ [jj-gate] cannot list the changes in $revset." >&2; return 1; }
  if [ "$trailer_only" -eq 0 ]; then
    printf '%s\n' "$ids" | node scripts/home-path-input.js messages 2> /dev/null |
      scripts/hooks/home-path.sh --records || { echo "❌ [jj-gate] message check refused." >&2; return 1; }
  fi
  msg=$(mktemp)
  for id in $ids; do
    jj log -r "commit_id($id)" --no-graph -T 'description' > "$msg" ||
      { echo "❌ [jj-gate] cannot read the description of commit ${id:0:12}." >&2; rm -f "$msg"; return 1; }
    [ -s "$msg" ] || continue
    if ! scripts/hooks/commit-msg "$msg" --trailers-only; then
      echo "   commit ${id:0:12}: fix it with: jj describe 'commit_id($id)'" >&2
      fail=1
    fi
  done
  rm -f "$msg"
  return "$fail"
}

# Keep the path globs, the theme-init.js rule and the test commands in sync with
# scripts/hooks/pre-commit and scripts/hooks/pre-push: both copies must gate the same files.
check() {
  local f fail=0 rust=0 input_type baseline checked conflicts
  local -a changed js=()
  # Every file the stack touches since trunk, deletions included: removing a .rs file can
  # break the build as surely as editing one. -t, not the hook's -z: jj prints paths
  # unquoted, so only a path holding a newline would split (none in this repo).
  mapfile -t changed < <(jj diff --from 'trunk()' --to @ --name-only)
  wait $! # a process substitution's failure is otherwise ignored

  for f in "${changed[@]}"; do
    case "$f" in
      *.rs | Cargo.toml | */Cargo.toml | Cargo.lock | desktop-slint/ui/*) rust=1 ;; # .slint and fonts compile into the crate
      desktop-tauri/ui/*.js | scripts/*.js) [ -f "$f" ] && js+=("$f") ;; # skip deletions
    esac
  done

  check_messages || return 1
  # What the stack adds since it forked from trunk (a diff from trunk itself would show trunk's
  # newer changes reversed); the push re-checks each published commit.
  baseline=$(commit_id 'fork_point(trunk() | @)') || die "cannot resolve one fork point"
  checked=$(commit_id @) || die "cannot resolve the checked snapshot"
  conflicts=$(jj log -r "commit_id($checked)" --no-graph -T conflict 2> /dev/null) || die "cannot inspect conflicts"
  [ "$conflicts" = false ] || die "cannot check unresolved conflicts faithfully"
  scripts/hooks/home-path.sh --between "$baseline" "$checked" || return 1

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
commit_id() {
  local id
  id=$(jj log -r "$1" --no-graph -T 'commit_id') || return 1
  [[ "$id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || return 1
  printf '%s' "$id"
}

push() {
  local arg dry again line u i remote="" in_plan=0 deletes=0 kind name what remote_sha to_id destination pub_id tag_oid tag_peeled sign_on_push
  local -a updates=() # "<remote sha, or 0 for a new ref> <local sha>", as the pre-push hook gets them
  local -a published_targets=() # tag object id for tags, or commit id for bookmarks
  local -a update_remotes=() # same indexes as updates, including across deletion-only sections
  local -a tag_refs=() tag_oids=() # scanned tag refs and object ids, re-verified right before push

  [ -x scripts/review-gate.sh ] ||
    die "scripts/review-gate.sh is missing or not executable. Run: chmod +x scripts/review-gate.sh  (or jj restore scripts/review-gate.sh)"

  for arg in "$@"; do
    case "$arg" in
      --config | --config=* | --config-file | --config-file=*)
        die "config overrides on the command line are not allowed; set them in config instead." ;;
      -R | -R* | --repository | --repository=* | --at-operation | --at-operation=* | --at-op | --at-op=*)
        die "repository and operation overrides on the command line are not allowed." ;;
    esac
  done

  sign_on_push=$(jj --ignore-working-copy config get git.sign-on-push 2> /dev/null) ||
    die "cannot read git.sign-on-push setting safely"
  [ "$sign_on_push" = false ] ||
    die "git.sign-on-push is enabled, which signs commits after checks; disable it and sign beforehand with jj sign."

  # jj decides what a bare `jj git push` moves, so ask it rather than re-deriving that.
  # The plan is the indented lines under `Changes to push to <remote>:`, each
  # `  bookmark: NAME [add to X]`, `[move forward from A to B]` or `[delete from A]` (tags
  # likewise); X, A and B are abbreviated commit ids. Fails closed: a plan line, or an output
  # with neither a plan nor "Nothing changed.", that this cannot read stops the push, so a
  # reworded jj never lets an unreviewed commit through.
  # One deliberate snapshot (its warnings show once), then the plan is read without one, so it
  # compares byte for byte with the second read before the push.
  jj log -r @ --no-graph -T '' > /dev/null 2>&1 || die "cannot snapshot the checked state safely"
  dry=$(jj --ignore-working-copy --config git.sign-on-push=false git push --dry-run "$@" 2>&1) || die "cannot read the push plan safely"
  while IFS= read -r line; do
    if [[ "$line" =~ ^Changes\ to\ push\ to\ (.+):$ ]]; then
      decode_jj_name "${BASH_REMATCH[1]}" || die "cannot decode remote name in the push plan safely. Bypass: jj git push"
      remote=$REPLY; in_plan=1; continue
    fi
    [[ "$line" =~ ^[[:space:]] && "$in_plan" -eq 1 ]] || { in_plan=0; continue; }
    [[ "$line" =~ ^[[:space:]]+(bookmark|tag):[[:space:]]+(.+)[[:space:]]\[(.*)\]$ ]] ||
      die "cannot read this line of the push plan safely. Bypass: jj git push"
    kind=${BASH_REMATCH[1]} name=${BASH_REMATCH[2]} what=${BASH_REMATCH[3]}
    case "$what" in
      "add to "*)               remote_sha=0; to_id=${what#add to } ;;
      "move "*" from "*" to "*) remote_sha=${what#*from }; remote_sha=${remote_sha%% to *}; to_id=${what##* to } ;;
      "delete "*)               deletes=$((deletes + 1)); continue ;; # nothing to review
      *) die "cannot read the push plan safely. Bypass: jj git push" ;;
    esac
    decode_jj_name "$name" || die "cannot decode destination name in the push plan safely. Bypass: jj git push"
    name=$REPLY
    case "$kind" in bookmark) destination="refs/heads/$name" ;; tag) destination="refs/tags/$name" ;; esac
    printf '%s' "$destination" | scripts/hooks/home-path.sh --ref || exit 1
    # Plan tokens are commit IDs, not general revsets or same-named bookmarks/tags.
    [[ "$remote_sha" =~ ^[0-9a-f]+$ && "$to_id" =~ ^[0-9a-f]+$ ]] ||
      die "cannot read commit ids in the push plan"
    [ "$remote_sha" = 0 ] || remote_sha=$(commit_id "commit_id($remote_sha)") || die "cannot resolve planned commit"
    to_id=$(commit_id "commit_id($to_id)") || die "cannot resolve planned commit"
    pub_id=$to_id
    if [ "$kind" = tag ]; then
      tag_oid=$(git --no-replace-objects rev-parse --verify --quiet "refs/tags/$name") ||
        die "cannot resolve tag object in the push plan safely"
      tag_peeled=$(git --no-replace-objects rev-parse --verify --quiet "$tag_oid^{commit}") ||
        die "cannot peel tag object in the push plan safely"
      [ "$tag_peeled" = "$to_id" ] ||
        die "tag object does not match the planned commit"
      pub_id=$tag_oid
      tag_refs+=("refs/tags/$name")
      tag_oids+=("$tag_oid")
    fi
    updates+=("$remote_sha $to_id")
    published_targets+=("$pub_id")
    update_remotes+=("$remote")
  done <<< "$dry"

  if [ "${#updates[@]}" -eq 0 ]; then
    # Only deletions, or nothing to push. A plan header with no line read under it, or any
    # other output, is a plan this did not recognise.
    { [ "$deletes" -gt 0 ] || [[ "$dry" == *"Nothing changed."* && "$dry" != *"Changes to push"* ]]; } ||
      die "cannot read the output of jj git push --dry-run safely. Bypass: jj git push"
  else
    # The trailer ban is part of the push, as commit-msg is part of every commit under git, and
    # so is pre-push's home-path check over each published commit.
    for i in "${!updates[@]}"; do
      u=${updates[$i]}
      check_messages "trunk()..commit_id(${u#* })" 1 || exit 1
      scripts/hooks/home-path.sh --published "${u%% *}" "${published_targets[$i]}" "${update_remotes[$i]}" || exit 1
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

  # Push exactly the plan the gates checked. A plain push would snapshot @ again, so a file
  # edited while the tests ran would land in a commit nobody checked; and a bookmark another
  # jj process moved meanwhile would change the plan, so refuse then.
  again=$(jj --ignore-working-copy --config git.sign-on-push=false git push --dry-run "$@" 2>&1) || die "cannot reread the push plan safely"
  [ "$again" = "$dry" ] || die "the push plan changed while the gates ran; rerun jj gpush."
  for i in "${!tag_refs[@]}"; do
    tag_oid=$(git --no-replace-objects rev-parse --verify --quiet "${tag_refs[$i]}") ||
      die "the push plan changed while the gates ran; rerun jj gpush."
    [ "$tag_oid" = "${tag_oids[$i]}" ] ||
      die "the push plan changed while the gates ran; rerun jj gpush."
  done
  jj --ignore-working-copy --config git.sign-on-push=false git push "$@" > /dev/null 2>&1 || die "push failed; inspect the remote state before retrying"
  echo "[jj-gate] checked push completed."
}

# The suites read the working tree, so they can only vouch for @ (or its sole parent when @ is empty).
# Other pushed tips go through untested here, with a warning, as in the pre-push hook.
slow_gates() {
  if [ "${SKIP_SLOW_GATES:-0}" = "1" ]; then
    echo "[jj-gate] SKIP_SLOW_GATES=1 — skipping test suites."
    return
  fi
  local u tip cur parent empty_wc covered=0 # empty_wc: @'s id when @ has no changes, else empty
  cur=$(commit_id @) || die "cannot resolve the working-copy commit"
  # An empty working copy covers only its sole parent. Preserve separators to check
  # each merge parent below without treating the merged tree as that parent's test coverage.
  parent=$(jj log -r '@-' --no-graph -T 'commit_id ++ "\n"') || die "cannot read the working-copy parents"
  empty_wc=$(jj log -r 'empty() & @' --no-graph -T 'commit_id') || die "cannot check whether the working copy is empty"
  for u in "$@"; do
    tip=${u#* }
    if [ "$tip" = "$cur" ] || { [ "$tip" = "$parent" ] && [ -n "$empty_wc" ]; }; then covered=1; fi
  done
  for u in "$@"; do
    tip=${u#* }
    if [ "$tip" = "$cur" ] || { [ "$tip" = "$parent" ] && [ -n "$empty_wc" ]; }; then
      continue
    elif [[ $'\n'"$parent"$'\n' == *$'\n'"$tip"$'\n'* ]]; then
      # With another tip covered, the tests run on @, which holds this one too.
      [ "$covered" -eq 1 ] ||
        die "the pushed parent has no working-copy test coverage, so the tests would not run on it. Push the working-copy commit too, or run: jj new 'commit_id($tip)', then rerun jj gpush. Bypass: SKIP_SLOW_GATES=1 jj gpush"
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
