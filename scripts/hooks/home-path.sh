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
  diff=$(git log -p -U0 --no-color --no-ext-diff --format='commit %H' "$2" --not --remotes) ||
    { echo "❌ [home-path] cannot list the commits in $2 to check them." >&2; exit 1; }
else
  diff=$(cat) || { echo "❌ [home-path] cannot read the diff." >&2; exit 1; }
fi

homes="" home=${HOME:-} profile=${USERPROFILE:-} bs='\'
# A trailing slash would stop "$HOME/x" from matching; a HOME of / becomes empty, and awk
# skips an empty home.
home=${home%/}
# The resolved form too, for a home reached through a symlink. "$home/" so an empty home
# resolves to /, which is then stripped like HOME, rather than to the current directory as
# `cd ""` does before bash 5.3.
real=$(cd "$home/" 2> /dev/null && pwd -P) || real=""
real=${real%/}
# USERPROFILE as written, with forward slashes, and with the doubled backslashes a TOML or
# JSON string needs. The doubling is a loop, not ${profile//...}: how a replacement string's
# backslashes and quotes come out differs between bash 3.2, 4.3 and 5.2.
doubled="" rest=$profile
while [[ $rest == *"$bs"* ]]; do
  doubled+="${rest%%"$bs"*}$bs$bs"
  rest=${rest#*"$bs"}
done
doubled+=$rest
for h in "$home" "$real" "$profile" "${profile//"$bs"//}" "$doubled"; do
  homes+="$h"$'\n'
done

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
