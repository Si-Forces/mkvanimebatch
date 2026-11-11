#!/usr/bin/env bash
# Refuses a diff that adds the committer's own home directory, such as /home/<user>/... or
# C:\Users\<user>\...: an absolute path an agent copies from its session publishes the
# username and folder layout once pushed, and amending or force-pushing cannot take it back
# (GitHub keeps the old commit). Only this machine's $HOME (and USERPROFILE on Windows)
# counts, so generic fixtures such as /home/user/show stay allowed.
#
# Usage:
#   home-path.sh                     checks a git unified or combined diff on stdin
#   home-path.sh --staged            checks the staged diff, so an unstaged edit can neither
#                                    hide nor fake a hit
#   home-path.sh --published <sha>   checks each commit in <sha> (a full commit id) that no
#                                    remote-tracking ref holds, one at a time: a path added in
#                                    one commit and removed in a later one is still in the
#                                    pushed history. A merge counts for the lines its
#                                    resolution adds, which no parent has.
# Prints the file and line number of each hit, redacting recognized home spellings from the
# file name, and exits 1, else exits 0. Not the line's text: gate output gets pasted into PR
# descriptions, which nothing checks.
# Run by pre-commit, pre-push and scripts/jj-gate.sh; not a hook itself.
set -euo pipefail

usage() { echo "❌ [home-path] usage: home-path.sh [--staged | --published <sha>]" >&2; exit 1; }
# What both git commands pass, so that no git setting hides a line or misnames a file. The
# prefixes are set because diff.noprefix and diff.mnemonicPrefix would change them (a file's
# name is read from its +++ line, so a/ is there only to match b/), and --no-relative because
# diff.relative would drop each file outside the current folder. --text
# prints the lines of a file git takes for binary (by a -diff or binary attribute, or a NUL in
# its first 8000 bytes), where it would otherwise say only that the files differ; a merge's
# combined diff (-c) ignores it, so what a resolution adds to such a file stays unread. git's
# own errors are dropped: they could print a path that holds the home. So are NUL bytes, in
# every mode (--text prints the ones a file holds): bash drops one with a warning that names
# this script's path, which may hold the home.
diff_flags=(-U0 --text --no-color --no-relative --no-ext-diff --no-textconv
  --src-prefix=a/ --dst-prefix=b/)
case ${1:-} in
  --published)
    # A full commit id only: a ref or path could hold the home, and git's error would print
    # it. A push sends the original objects, so git log must not read git replace refs or an
    # info/grafts file, which could hide a commit from it; the graft file named here cannot
    # exist.
    [ $# -eq 2 ] && [[ $2 =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || usage
    diff=$(GIT_GRAFT_FILE=/dev/null/none git --no-replace-objects log \
      -p --root -c "${diff_flags[@]}" \
      --format='commit %H' "$2" --not --remotes 2> /dev/null | tr -d '\0') ||
      { echo "❌ [home-path] cannot list the commits in $2 to check them (git log $2 says why)." >&2
        exit 1; }
    ;;
  --staged)
    # A git replace ref on HEAD could stand a tree that already holds the line in for the one
    # the commit extends.
    [ $# -eq 1 ] || usage
    diff=$(git --no-replace-objects diff --cached "${diff_flags[@]}" 2> /dev/null | tr -d '\0') ||
      { echo "❌ [home-path] cannot read the staged diff (git diff --cached says why)." >&2
        exit 1; }
    ;;
  *)
    [ $# -eq 0 ] || usage # a sha without --published would wait on stdin
    diff=$(tr -d '\0') || { echo "❌ [home-path] cannot read the diff." >&2; exit 1; }
    ;;
esac

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

# Makes doubled separators single, into REPLY.
squeeze() { # <path>
  local sep
  REPLY=$1
  for sep in / "$bs"; do
    while [[ $REPLY == *"$sep$sep"* ]]; do replace_all "$REPLY" "$sep$sep" "$sep"; done
  done
}

# Trailing separators would stop "$HOME/x" from matching. A doubled one would too, so each
# variable counts as set and squeezed; for HOME, as set matters only for a doubled backslash,
# since a run of slashes in the scanned line is read as one. USERPROFILE is taken with
# backslashes, whichever it was set with; the other forms come from that below.
home_as_set=${HOME:-}
while [[ $home_as_set == */ ]]; do home_as_set=${home_as_set%/}; done
replace_all "${USERPROFILE:-}" / "$bs"
profile_as_set=$REPLY
while [[ $profile_as_set == *"$bs" ]]; do profile_as_set=${profile_as_set%"$bs"}; done
squeeze "$profile_as_set"
profile=$REPLY

# The resolved form too, for a home reached through a symlink. Only an absent home has none:
# every other failure refuses rather than check less. A missing name looks the same as one
# under something that cannot be searched, so absent means its nearest existing ancestor can be.
# HOME is resolved as it is set, not squeezed: a backslash is an ordinary character in a POSIX
# file name, so a squeezed doubled one names a different folder, and cd reads a run of / as one
# by itself. Stripping the trailing separators is enough to keep cd "" out of reach.
cannot_resolve() { echo "❌ [home-path] cannot resolve HOME: $1." >&2; exit 1; }
real=""
if [ -n "$home_as_set" ]; then
  # A relative HOME as ./HOME, so cd neither searches CDPATH nor reads a leading - as an option.
  # A drive path (C:/Users/x) is left alone: it is absolute on Git Bash, the one place a HOME
  # is set that way. Elsewhere it is a relative path, so CDPATH is emptied for the cd; with
  # that, ./C: is the same folder there, and no test can tell the two apart.
  dir=$home_as_set
  [[ $dir == /* || $dir =~ ^[A-Za-z]: ]] || dir=./$dir
  if ! real=$(CDPATH= cd "$dir" 2> /dev/null && pwd -P); then
    [ -e "$dir" ] && cannot_resolve "not a folder this user can enter" # or a symlink to one
    [ -L "$dir" ] && cannot_resolve "a dangling symlink or a symlink loop"
    up=$dir
    until [ -e "$up" ] || [ -L "$up" ] || [ "$up" = / ] || [ "$up" = . ]; do
      up=$(dirname "$up")
    done
    (cd "$up" 2> /dev/null) ||
      cannot_resolve "something above it is not a folder this user can search"
  fi
fi
# Squeezed only now: the match forms want it, the cd above does not.
squeeze "$home_as_set"
home=$REPLY

# A path of one segment, such as /root, /app or C:\app, is the same on every machine and names
# nobody, so it is not looked for; nor is what / or a drive alone (C:\) leave. Each form is
# judged by itself: a one-segment HOME may resolve to a longer path that does name someone.
names_someone() { # <path>: true when it has more than one segment, a drive aside
  local path=$1
  [[ $path =~ ^[A-Za-z]: ]] && path=${path:2}
  [[ ${path:1} == */* || ${path:1} == *"$bs"* ]]
}
# The squeezed form decides for both: //root as set is still one segment.
names_someone "$home" || { home=""; home_as_set=""; }
names_someone "$real" || real=""
names_someone "$profile" || { profile=""; profile_as_set=""; }

# USERPROFILE also with forward slashes, and with the doubled backslashes a TOML or JSON
# string needs.
replace_all "$profile" "$bs" /
forward=$REPLY
replace_all "$profile" "$bs" "$bs$bs"
homes=""
for h in "$home" "$home_as_set" "$real" "$profile" "$profile_as_set" "$forward" "$REPLY"; do
  homes+="$h"$'\n'
done
[ -n "$home$real$profile" ] ||
  echo "⚠  [home-path] HOME and USERPROFILE name no home directory (unset, /, a drive alone," \
    "or one segment such as /root), so nothing is checked." >&2

# ENVIRON, not awk -v, which would read the backslashes of a Windows path as escapes. LC_ALL=C,
# as a diff is bytes, not text: tolower() then folds only A-Z (a Turkish locale folds I to a
# dotless i), and bytes that are not UTF-8 neither derail gawk nor stop BSD awk.
# A home counts wherever it starts, as in -I/home/al/include or /mnt/backup/home/al, but only
# up to the end of a segment: /home/al does not match /home/alice, though a full stop that
# ends a sentence still ends the path. ASCII case is ignored, as Windows paths are.
# File headers are the ---/+++ lines between "diff --git" (or a merge's "diff --combined" or
# "diff --cc") and the first hunk; after that, "+++" is an added line starting with "++".
hits=$(HOMES="$homes" LC_ALL=C awk '
  # name is what the character after h must not be; lead, when set, the one before it.
  function names_home(line, h, name, lead,    s, p, q, at) {
    s = line
    at = 0 # where s starts in line, less one
    while ((p = index(s, h)) > 0) {
      at += p
      # In a plain path, a full stop then a name character continues the name (".bak");
      # otherwise it ends a sentence (". " or "...") or starts a git range ("..main"). The
      # dashed spelling has a - for each full stop in the name, so there one starts an extension.
      q = p + length(h)
      if (lead == "" && substr(s, q, 1) == ".") q++
      # A match at the start of the line has nothing before it; awks differ on what
      # substr(line, 0, 1) returns (gawk gives the first character of the line).
      if (substr(s, q, 1) !~ name &&
          (lead == "" || at == 1 || substr(line, at - 1, 1) !~ lead)) return 1
      s = substr(s, p + 1)
    }
    return 0
  }
  # Consume the original name once, so no candidate can match an inserted placeholder.
  # Literal matching also keeps regexp characters in a home harmless.
  function redact(file,    out, at, size, i, h, p) {
    while (1) {
      at = 0
      for (i = 1; i <= 2 * n; i++) {
        h = i <= n ? home[i] : dashed[i - n]
        p = h == "" ? 0 : index(tolower(file), h)
        if (p > 0 && (at == 0 || p < at)) { at = p; size = length(h) }
      }
      if (at == 0) return out file
      out = out substr(file, 1, at - 1) "<home>"
      file = substr(file, at + size)
    }
  }
  BEGIN {
    n = split(tolower(ENVIRON["HOMES"]), home, "\n")
    # Claude Code names a project folder after its working directory with each UTF-16 unit but
    # an ASCII letter or digit turned into -, as in -home-al-repo, so that spelling counts too.
    # A character outside the Basic Multilingual Plane is two units, so it gets two.
    # Bytes here: dropping UTF-8 continuation bytes leaves one byte per character, and the lead
    # byte of a 4-byte character, the only kind outside that plane, becomes two -.
    # Such a name starts a path or follows a separator, and a - ends a segment of it.
    # Use every plain spelling, including as-set homes whose doubled separators survive.
    for (i = 1; i <= n; i++) {
      dashed[i] = home[i]
      gsub(/[\200-\277]/, "", dashed[i])
      gsub(/[\360-\367]/, "--", dashed[i])
      gsub(/[^A-Za-z0-9]/, "-", dashed[i])
      dashed[i] = tolower(dashed[i])
    }
  }
  /^commit [0-9a-f]+$/ { commit = substr($2, 1, 12) " "; next }
  /^diff --(git|combined|cc) / { header = 1; next }
  # git ends a name that holds a space with a tab.
  header && /^\+\+\+ / {
    file = substr($0, 5); sub(/^b\//, "", file); sub(/\t$/, "", file)
    file = redact(file)
    next
  }
  # "@@ -a,b +c,d @@", or for a merge one @ and one -a,b per parent more: c is the number of
  # the first line the hunk leaves in the file.
  /^@@/ {
    header = 0
    match($0, /^@+/); parents = RLENGTH - 1
    match($0, /\+[0-9]+/); number = substr($0, RSTART + 1, RLENGTH - 1) + 0
    next
  }
  # diff.suppressBlankEmpty writes a blank context line with no mark at all, and context
  # reaches a -U0 diff through diff.interHunkContext or GIT_DIFF_OPTS, so such a line must
  # still advance the count. (An added or removed blank line keeps its mark.) No !header here:
  # the blank lines git log writes around each "commit" line, and any a file header might hold,
  # are all above a hunk, where @@ sets the count outright.
  /^$/ { number++; next }
  # A line starts with one mark per parent: "-" for a line the result drops, "+" for one that
  # parent lacks. Only a line every parent lacks is new here; one a parent has was checked in
  # that parent, or is published already.
  !header && /^[-+ ]/ {
    mark = substr($0, 1, parents)
    if (mark ~ /-/) next
    if (mark ~ / /) { number++; next }
    # The \/ some JSON writers put for / is read as /, so one rule covers both spellings; so
    # is \\/, that escape inside another string. A run of them is read as one /, as HOME is
    # squeezed.
    line = tolower(substr($0, parents + 1))
    gsub(/(\\*\/)+/, "/", line)
    hit = 0
    for (i = 1; i <= n && !hit; i++)
      hit = home[i] != "" && names_home(line, home[i], "[a-z0-9_~-]", "")
    for (i = 1; i <= n && !hit; i++)
      hit = dashed[i] != "" && names_home(line, dashed[i], "[a-z0-9]", "[a-z0-9]")
    if (hit) printf "     %s%s:%d\n", commit, file, number
    number++
  }
' <<< "$diff") || { echo "❌ [home-path] cannot scan the diff." >&2; exit 1; }

[ -z "$hits" ] && exit 0
echo "❌ [home-path] these added lines name your home directory, which would be published:" >&2
printf '%s\n' "$hits" >&2
echo "   Use a relative path, ~, or a placeholder such as <repo>," \
  "then rewrite the commit that adds it." >&2
exit 1
