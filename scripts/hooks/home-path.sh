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
# Prints each added line that names the home directory and exits 1, else exits 0.
# Run by pre-commit, pre-push and scripts/jj-gate.sh; not a hook itself.
set -euo pipefail

if [ "${1:-}" = --published ]; then
  [ -n "${2:-}" ] || { echo "❌ [home-path] usage: home-path.sh --published <sha>" >&2; exit 1; }
  diff=$(git log -p -U0 --no-color --no-ext-diff --no-textconv --format='commit %H' "$2" --not --remotes) ||
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

[ -n "${HOME:-}${USERPROFILE:-}" ] ||
  echo "⚠  [home-path] neither HOME nor USERPROFILE is set, so there is no home directory to look for." >&2

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
# under a folder that cannot be searched, so absent means its nearest existing ancestor can be.
cannot_resolve() { echo "❌ [home-path] cannot resolve HOME ($home): $1." >&2; exit 1; }
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
    (cd "$up" 2> /dev/null) || cannot_resolve "$up cannot be searched"
    real=""
  fi
fi

# Each form as written and with the \/ some JSON writers put for /; USERPROFILE also with
# forward slashes, and with the doubled backslashes a TOML or JSON string needs.
homes=""
add_home() { homes+="$1"$'\n'; }
replace_all "$profile" "$bs" /
forward=$REPLY
for h in "$home" "$real" "$forward"; do
  add_home "$h"
  replace_all "$h" / "$bs/"
  add_home "$REPLY"
done
add_home "$profile"
replace_all "$profile" "$bs" "$bs$bs"
add_home "$REPLY"

# ENVIRON, not awk -v, which would read the backslashes of a Windows path as escapes.
# A home counts only as a whole path segment run: /home/al does not match /home/alice, though
# a full stop that ends a sentence still ends the path. A one-segment home such as /root must
# also start a path, so it does not match src/root.rs; a longer one names the user wherever it
# starts, as in -I/home/al/include or /mnt/backup/home/al. Case is ignored, as Windows paths are.
# File headers are the ---/+++ lines between "diff --git" and the first hunk; after that, a
# line starting "+++" is an added line that starts with "++".
hits=$(HOMES="$homes" awk '
  function names_home(line, h, anywhere,    s, p, q, before, after) {
    s = line
    while ((p = index(s, h)) > 0) {
      before = (p > 1) ? substr(s, p - 1, 1) : ""
      # Full stops then a name character continue the name (".bak"); otherwise they end a
      # sentence (". " or "...").
      for (q = p + length(h); substr(s, q, 1) == "."; q++) {}
      after = substr(s, q, 1)
      if ((anywhere || before !~ /[a-z0-9._~-]/) && after !~ /[a-z0-9._~-]/) return p + length(line) - length(s)
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
  header && /^\+\+\+ / { file = substr($0, 5); sub(/^b\//, "", file); next }
  /^@@/ { header = 0; next }
  !header && /^\+/ {
    line = substr($0, 2)
    for (i = 1; i <= n; i++) {
      p = (home[i] == "") ? 0 : names_home(tolower(line), home[i], anywhere[i])
      if (!p) continue
      # The match with some context, not a whole minified or generated line.
      from = (p > 40) ? p - 40 : 1
      printf "     %s%s: %s%s%s\n", commit, file, (from > 1 ? "..." : ""), substr(line, from, 120), (from + 120 <= length(line) ? "..." : "")
      break
    }
  }
' <<< "$diff") || { echo "❌ [home-path] cannot scan the diff." >&2; exit 1; }

[ -z "$hits" ] && exit 0
echo "❌ [home-path] these added lines name your home directory, which would be published:" >&2
printf '%s\n' "$hits" >&2
echo "   Use a relative path, ~, or a placeholder such as <repo>, then rewrite the commit that adds it." >&2
exit 1
