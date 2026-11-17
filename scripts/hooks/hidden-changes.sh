# Sourced by pre-commit, pre-push and scripts/mutants-diff.sh; not a hook itself.
#
# git diff trusts assume-unchanged and skip-worktree entries, so edits to those files are
# invisible to the hooks' dirty-tree checks. These helpers find the ones that really differ.

# Counts assume-unchanged or skip-worktree paths whose working-tree file is missing or
# differs from its index blob, printing "<changed> <missing>" (or nothing when clean).
# A flagged file that still matches the index is fine: that is the usual way to keep a
# machine-local file out of commits.
hidden_changes() { # [pathspec...]
  local entry tag mode blob path target actual changed=0 missing=0
  # Entries read "<tag> <mode> <blob> <stage>\t<path>"; tag h = assume-unchanged,
  # S = skip-worktree, s = both.
  while IFS= read -r -d '' entry; do
    read -r tag mode blob _ <<< "${entry%%$'\t'*}"
    [[ "$tag" == [a-zS] ]] || continue
    path=${entry#*$'\t'}
    if [ "$mode" = 120000 ] && [ -L "$path" ]; then # the blob of a symlink is its target
      target=$(readlink -- "$path" 2> /dev/null) || {
        echo "❌ [hidden-changes] cannot inspect an assume-unchanged or skip-worktree symlink." >&2
        return 1
      }
      actual=$(printf '%s' "$target" | git hash-object --stdin 2> /dev/null) || {
        echo "❌ [hidden-changes] cannot inspect an assume-unchanged or skip-worktree symlink." >&2
        return 1
      }
      [ "$actual" = "$blob" ] || changed=$((changed + 1))
    elif [ -f "$path" ] && [ ! -L "$path" ]; then
      actual=$(git hash-object -- "$path" 2> /dev/null) || {
        echo "❌ [hidden-changes] cannot inspect an assume-unchanged or skip-worktree file." >&2
        return 1
      }
      [ "$actual" = "$blob" ] || changed=$((changed + 1))
    else
      missing=$((missing + 1)) # missing, or a gitlink: reported, since it cannot be compared
    fi
  done < <(git ls-files -s -v -z -- "$@" 2> /dev/null)
  wait $! || return 1
  [ $((changed + missing)) -eq 0 ] || printf '%d %d\n' "$changed" "$missing"
}

explain_hidden_changes() { # "<changed> <missing>"
  local changed=0 missing=0
  read -r changed missing <<< "$1"
  if [ "${changed:-0}" -gt 0 ]; then
    echo "   $changed file(s) differ from the index, but an assume-unchanged or skip-worktree bit" >&2
    echo "   hides that from git diff, so staging or stashing them does nothing." >&2
    echo "   List flagged files with: git ls-files -v | grep -E '^([a-z]|S) '" >&2
    echo "   Clear the bits one flag per call (combined, skip-worktree stays set):" >&2
    echo "     git update-index --no-assume-unchanged <path>" >&2
    echo "     git update-index --no-skip-worktree <path>" >&2
  fi
  if [ "${missing:-0}" -gt 0 ]; then
    echo "   $missing flagged file(s) are missing from the working tree, so the checks cannot see them." >&2
    echo "   List flagged files with: git ls-files -v | grep -E '^([a-z]|S) '" >&2
    echo "   In a sparse checkout, restore them with: git sparse-checkout disable" >&2
    echo "   (clearing skip-worktree instead would turn them into deletions). Otherwise clear the" >&2
    echo "   bit with git update-index --no-assume-unchanged <path>, then restore or delete them." >&2
  fi
}
