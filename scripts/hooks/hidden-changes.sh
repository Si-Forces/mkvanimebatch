# Sourced by pre-commit and pre-push; not a hook itself.
#
# git diff trusts assume-unchanged and skip-worktree entries, so edits to those files are
# invisible to the hooks' dirty-tree checks. These helpers find the ones that really differ.

# Prints each assume-unchanged or skip-worktree path whose working-tree file is missing or
# differs from its index blob. A flagged file that still matches the index is fine: that is
# the usual way to keep a machine-local file out of commits.
hidden_changes() { # [pathspec...]
  local entry tag mode blob path actual
  # Entries read "<tag> <mode> <blob> <stage>\t<path>"; tag h = assume-unchanged,
  # S = skip-worktree, s = both.
  git ls-files -s -v -z -- "$@" | while IFS= read -r -d '' entry; do
    read -r tag mode blob _ <<< "${entry%%$'\t'*}"
    [[ "$tag" == [a-zS] ]] || continue
    path=${entry#*$'\t'}
    if [ "$mode" = 120000 ] && [ -L "$path" ]; then # the blob of a symlink is its target
      actual=$(printf '%s' "$(readlink -- "$path")" | git hash-object --stdin)
    elif [ -f "$path" ] && [ ! -L "$path" ]; then
      actual=$(git hash-object -- "$path")
    else
      actual="" # missing, or a gitlink: reported, since it cannot be compared
    fi
    [ "$actual" = "$blob" ] || printf '%s\n' "$path"
  done
}

explain_hidden_changes() { # <paths>
  local path missing="" changed=""
  while IFS= read -r path; do
    if [ -e "$path" ] || [ -L "$path" ]; then changed+="$path"$'\n'; else missing+="$path"$'\n'; fi
  done <<< "$1"
  if [ -n "$changed" ]; then
    echo "   These differ from the index, but an assume-unchanged or skip-worktree bit hides" >&2
    echo "   that from git diff, so staging or stashing them does nothing:" >&2
    printf '%s' "$changed" | sed 's/^/     /' >&2
    echo "   Clear the bits one flag per call (combined, skip-worktree stays set):" >&2
    echo "     git update-index --no-assume-unchanged <path>" >&2
    echo "     git update-index --no-skip-worktree <path>" >&2
  fi
  if [ -n "$missing" ]; then
    echo "   These are missing from the working tree, so the checks cannot see them:" >&2
    printf '%s' "$missing" | sed 's/^/     /' >&2
    echo "   In a sparse checkout, restore them with: git sparse-checkout disable" >&2
    echo "   (clearing skip-worktree instead would turn them into deletions). Otherwise clear the" >&2
    echo "   bit with git update-index --no-assume-unchanged <path>, then restore or delete them." >&2
  fi
}
