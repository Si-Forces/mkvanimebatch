#!/usr/bin/env bash
# Refuses a diff that adds the committer's own home directory, such as /home/<user>/... or
# C:\Users\<user>\...: an absolute path an agent copies from its session publishes the
# username and folder layout once pushed, and amending or force-pushing cannot take it back
# (GitHub keeps the old commit). Only this machine's $HOME (and USERPROFILE on Windows)
# counts, so generic fixtures such as /home/user/show stay allowed.
#
# Usage:
#   home-path.sh                     checks the unified diff on stdin
#   home-path.sh --published <sha>   checks each commit in <sha> that no remote-tracking ref
#                                    holds, one at a time: a path added in one commit and
#                                    removed in a later one is still in the pushed history
# Prints the file and line number of each added line that names the home directory and exits
# 1, else exits 0. Not the line's text: gate output gets pasted into PR descriptions, which
# nothing checks.
# Run by pre-commit, pre-push and scripts/jj-gate.sh; not a hook itself.
set -euo pipefail

if [ "${1:-}" = --published ]; then
  # A commit id only: a ref or path could hold the home, and git's error would print it.
  [[ "${2:-}" =~ ^[0-9a-fA-F]{4,64}$ ]] || { echo "❌ [home-path] usage: home-path.sh --published <sha>" >&2; exit 1; }
  diff=$(git log -p --root -U0 --no-color --no-ext-diff --no-textconv --format='commit %H' "$2" --not --remotes) ||
    { echo "❌ [home-path] cannot list the commits in $2 to check them." >&2; exit 1; }
else
  diff=$(cat) || { echo "❌ [home-path] cannot read the diff." >&2; exit 1; }
fi

bs='\'
# Replaces every <from> in <string> with <to>, into REPLY. A loop, not ${s//from/to}: how a
# replacement string's backslashes and quotes come out differs between bash 3.2, 4.3 and 5.2.
replace_all() { # <string> <from> <to>
  local rest=$1
  REPLY=""
  while [[ $rest == *"$2"* ]]; do
    REPLY+="${rest%%"$2"*}$3"
    rest=${rest#*"$2"}
  done
  REPLY+=$rest
}

# Trailing separators would stop "$HOME/x" from matching. A HOME of / becomes empty, as does a
# USERPROFILE of only a drive (C:\), and awk skips an empty home. USERPROFILE is taken with
# backslashes, whichever it was set with; the other forms come from that below.
home=${HOME:-}
while [[ $home == */ ]]; do home=${home%/}; done
replace_all "${USERPROFILE:-}" / "$bs"
profile=$REPLY
while [[ $profile == *"$bs" ]]; do profile=${profile%"$bs"}; done
[[ $profile =~ ^[A-Za-z]:$ ]] && profile=""

# The resolved form too, for a home reached through a symlink. Only an absent home has none:
# every other failure refuses rather than check less. A missing name looks the same as one
# under something that cannot be searched, so absent means its nearest existing ancestor can be.
cannot_resolve() { echo "❌ [home-path] cannot resolve HOME: $1." >&2; exit 1; }
real=""
if [ -n "$home" ]; then
  if real=$(cd "$home" 2> /dev/null && pwd -P); then
    real=${real%/}
  elif [ -L "$home" ]; then
    cannot_resolve "a dangling symlink or a symlink loop"
  elif [ -e "$home" ]; then
    cannot_resolve "not a folder this user can enter"
  else
    up=$home
    until [ -e "$up" ] || [ -L "$up" ] || [ "$up" = / ] || [ "$up" = . ]; do up=$(dirname "$up"); done
    (cd "$up" 2> /dev/null) || cannot_resolve "something above it is not a folder this user can search"
    real=""
  fi
fi

# USERPROFILE also with forward slashes, and with the doubled backslashes a TOML or JSON
# string needs.
replace_all "$profile" "$bs" /
forward=$REPLY
replace_all "$profile" "$bs" "$bs$bs"
homes=""
for h in "$home" "$real" "$profile" "$forward" "$REPLY"; do
  homes+="$h"$'\n'
done
[ -n "$home$profile" ] ||
  echo "⚠  [home-path] HOME and USERPROFILE name no home directory (unset, /, or a drive alone), so nothing is checked." >&2

# ENVIRON, not awk -v, which would read the backslashes of a Windows path as escapes.
# A home counts only as a whole path segment run: /home/al does not match /home/alice, though
# a full stop that ends a sentence still ends the path. A one-segment home such as /root must
# also start a path, so it does not match src/root.rs; a longer one names the user wherever it
# starts, as in -I/home/al/include or /mnt/backup/home/al. Case is ignored, as Windows paths are.
# File headers are the ---/+++ lines between "diff --git" and the first hunk; after that, a
# line starting "+++" is an added line that starts with "++".
hits=$(HOMES="$homes" awk '
  function names_home(line, h, unanchored,    s, p, q, before, after) {
    s = line
    while ((p = index(s, h)) > 0) {
      before = (p > 1) ? substr(s, p - 1, 1) : ""
      # Full stops then a name character continue the name (".bak"); otherwise they end a
      # sentence (". " or "...").
      for (q = p + length(h); substr(s, q, 1) == "."; q++) {}
      after = substr(s, q, 1)
      if ((unanchored || before !~ /[a-z0-9._~-]/) && after !~ /[a-z0-9._~-]/) return 1
      s = substr(s, p + 1)
    }
    return 0
  }
  BEGIN {
    n = split(tolower(ENVIRON["HOMES"]), home, "\n")
    # A separator after the first character: more than one segment, so it may start anywhere.
    for (i = 1; i <= n; i++) { t = substr(home[i], 2); anywhere[i] = index(t, "/") || index(t, "\\") }
  }
  /^commit [0-9a-f]+$/ { commit = substr($2, 1, 12) " "; next }
  /^diff --git / { header = 1; next }
  # git ends a name that holds a space with a tab.
  header && /^\+\+\+ / { file = substr($0, 5); sub(/^b\//, "", file); sub(/\t$/, "", file); next }
  # "@@ -a,b +c,d @@": c is the number of the first line the hunk leaves in the file.
  /^@@/ { header = 0; match($0, /\+[0-9]+/); number = substr($0, RSTART + 1, RLENGTH - 1) + 0; next }
  /^\+/ {
    # The \/ some JSON writers put for / is read as /, so one rule covers both spellings; so
    # is \\/, that escape inside another string.
    line = tolower(substr($0, 2))
    gsub(/\\+\//, "/", line)
    for (i = 1; i <= n; i++) {
      if (home[i] == "" || !names_home(line, home[i], anywhere[i])) continue
      printf "     %s%s:%d\n", commit, file, number
      break
    }
    number++
  }
  /^ / { number++ }
' <<< "$diff") || { echo "❌ [home-path] cannot scan the diff." >&2; exit 1; }

[ -z "$hits" ] && exit 0
echo "❌ [home-path] these added lines name your home directory, which would be published:" >&2
printf '%s\n' "$hits" >&2
echo "   Use a relative path, ~, or a placeholder such as <repo>, then rewrite the commit that adds it." >&2
exit 1
