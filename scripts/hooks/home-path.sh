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
#   home-path.sh --published <old> <new> [remote]
#                                    checks each commit in old..new, including fetched ones.
#                                    Both ids are full; old=0 means a new ref. For a new ref,
#                                    only that remote's main is a trusted baseline, if known;
#                                    otherwise all reachable history is checked. A merge
#                                    counts for lines its resolution adds, which no parent has.
# Prints the file and line number of each hit, redacting recognized home spellings from the
# file name, and exits 1, else exits 0. Not the line's text: gate output gets pasted into PR
# descriptions, which nothing checks.
# Run by pre-commit, pre-push and scripts/jj-gate.sh; not a hook itself.
set -euo pipefail

usage() { echo "❌ [home-path] usage: home-path.sh [--staged | --published <old> <new> [remote]]" >&2; exit 1; }
# What both git commands pass, so that no git setting hides a line or misnames a file. The
# prefixes are set because diff.noprefix and diff.mnemonicPrefix would change them (a file's
# name is read from its +++ line, so a/ is there only to match b/), and --no-relative because
# diff.relative would drop each file outside the current folder. --text
# prints the lines of a file git takes for binary (by a -diff or binary attribute, or a NUL in
# its first 8000 bytes), where it would otherwise say only that the files differ; a merge's
# combined diff (-c) ignores it, so what a resolution adds to such a file stays unread. git's
# own errors are dropped: they could print a path that holds the home. NUL bytes are removed
# in every mode (--text prints the ones a file holds).
diff_flags=(-U0 --text --no-color --no-relative --no-ext-diff --no-textconv
  --src-prefix=a/ --dst-prefix=b/)
mode=${1:-}
# Keep the array nonempty for Bash 3.2 with nounset; --not alone excludes nothing.
exclude=(--not)
case $mode in
  --published)
    # A full commit id only: a ref or path could hold the home, and git's error would print
    # it. A push sends the original objects, so git log must not read git replace refs or an
    # info/grafts file, which could hide a commit from it. /dev/null supplies no grafts.
    [ $# -ge 3 ] && [ $# -le 4 ] &&
      [[ $2 =~ ^(0|[0-9a-f]{40}|[0-9a-f]{64})$ && $3 =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || usage
    old=$2 new=$3
    if [[ $old =~ ^0+$ ]]; then
      # A fetched topic is still incoming to main. Never exclude every remote-tracking ref.
      # An unknown destination (including a URL) has no trusted baseline: scan from the root.
      old=""
      if [ $# -eq 4 ]; then
        # A remote may fetch privately and push elsewhere. Trust its fetched main only when
        # one push URL equals its fetch URL. Sentinels keep trailing newlines distinguishable.
        fetch_url=$(git remote get-url -- "$4" 2> /dev/null && printf .) || fetch_url=""
        push_urls=$(git remote get-url --push --all -- "$4" 2> /dev/null && printf .) || push_urls=""
        fetch_url=${fetch_url%$'\n.'} push_urls=${push_urls%$'\n.'}
        if [[ -n $fetch_url && $fetch_url != *$'\n'* && $fetch_url == "$push_urls" ]]; then
          old=$(git --no-replace-objects rev-parse --verify --quiet "refs/remotes/$4/main^{commit}" 2> /dev/null) || old=""
        fi
      fi
    fi
    [ -z "$old" ] || exclude=(--not "$old")
    ;;
  --staged)
    [ $# -eq 1 ] || usage
    ;;
  *)
    [ $# -eq 0 ] || usage # a sha without --published would wait on stdin
    ;;
esac

read_diff() {
  case $mode in
    --published)
      GIT_GRAFT_FILE=/dev/null git --no-replace-objects log -p --root -c "${diff_flags[@]}" \
        --format='commit %H' "$new" "${exclude[@]}" 2> /dev/null ||
        { echo "❌ [home-path] cannot list the commits to check (git log says why)." >&2; return 1; } ;;
    --staged)
      # A replace ref on HEAD could stand a tree that already holds the line in for the one
      # the commit extends.
      git --no-replace-objects diff --cached "${diff_flags[@]}" 2> /dev/null ||
        { echo "❌ [home-path] cannot read the staged diff (git diff --cached says why)." >&2; return 1; } ;;
    *) cat || { echo "❌ [home-path] cannot read the diff." >&2; return 1; } ;;
  esac
}

# A line-based diff cannot represent an identity containing a newline. Refuse it as a whole
# instead of splitting it into unrelated, possibly one-segment names.
case ${HOME:-}${USERPROFILE:-} in
  *$'\n'*) echo "❌ [home-path] HOME and USERPROFILE must not contain newlines." >&2; exit 1 ;;
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

# Trailing separators would stop "$HOME/x" from matching. Keep each variable as set and
# squeezed: literal backslashes matter in POSIX homes, and repeated separators survive in
# dashed spellings and filenames. USERPROFILE uses backslashes here; the other forms come
# from that below.
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
  # The sentinel preserves a canonical folder name ending in newline through substitution.
  if real=$(CDPATH= cd "$dir" 2> /dev/null && pwd -P 2> /dev/null && printf .); then
    real=${real%$'\n.'}
    case $real in *$'\n'*) cannot_resolve "the resolved name contains a newline" ;; esac
  else
    [ -e "$dir" ] && cannot_resolve "not a folder this user can enter" # or a symlink to one
    [ -L "$dir" ] && cannot_resolve "a dangling symlink or a symlink loop"
    up=$dir
    until [ -e "$up" ] || [ -L "$up" ] || [ "$up" = / ] || [ "$up" = . ]; do
      case $up in */*) up=${up%/*}; [ -n "$up" ] || up=/ ;; *) up=. ;; esac
    done
    (CDPATH= cd "$up" 2> /dev/null) ||
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

# A POSIX home may contain literal doubled backslashes and repeated forward slashes at once.
# Preserve its backslashes while normalizing slashes, independently of squeeze() above.
home_slashes=$home_as_set
while [[ $home_slashes == *//* ]]; do
  replace_all "$home_slashes" // /
  home_slashes=$REPLY
done

# USERPROFILE also with forward slashes, and with the doubled backslashes a TOML or JSON
# string needs.
replace_all "$profile" "$bs" /
forward=$REPLY
replace_all "$profile" "$bs" "$bs$bs"
doubled=$REPLY
homes=""
for h in "$home" "$home_as_set" "$home_slashes" "$real" "$profile" "$profile_as_set" "$forward" "$doubled"; do
  homes+="$h"$'\n'
done
# Native UTF-16 replacement matches Claude Code, without an awk byte/locale approximation.
# The JSON-doubled spelling is only a literal form, never another project-folder identity.
dashed=$(MSYS_NO_PATHCONV=1 node -e '
const homes = process.argv.slice(1);
if (homes.some(p => p.includes("\ufffd"))) process.exit(1);
process.stdout.write(homes.map(p => p.replace(/[^A-Za-z0-9]/g, "-")).join("\n"));
' -- "$home" "$home_as_set" "$home_slashes" "$real" "$profile" "$profile_as_set" "$forward" 2> /dev/null) ||
  { echo "❌ [home-path] cannot derive home spellings (Node and valid Unicode identities are required)." >&2; exit 1; }
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
if read_diff | LC_ALL=C tr -d '\0' | HOMES="$homes" DASHED="$dashed" LC_ALL=C awk '
  # name is what the character after h must not be; lead, when set, the one before it.
  function names_home(line, h, name, lead,    s, p, q, at) {
    s = line
    at = 0 # where s starts in line, less one
    while ((p = index(s, h)) > 0) {
      at += p
      # In a plain path, a full stop then a name character continues the name (".bak");
      # otherwise it ends a sentence (". " or "...") or starts a git range ("..main"). The
      # dashed spelling has a - for each full stop in the name, so there one starts an extension.
      q = at + length(h)
      # A match at the start of the line has nothing before it; awks differ on what
      # substr(line, 0, 1) returns (gawk gives the first character of the line).
      if (boundary(line, at, q, name, lead)) return 1
      s = substr(s, p + 1)
    }
    return 0
  }
  function boundary(line, start, end, name, lead) {
    if (name == "[a-z0-9_~-]" && substr(line, end, 1) == ".") end++
    return substr(line, end, 1) !~ name &&
      (lead == "" || start == 1 || substr(line, start - 1, 1) !~ lead)
  }
  # End of a literal home at p, allowing repeated or JSON-escaped slashes as in content.
  function home_end(file, p, h,    j, end, c) {
    if (h == "") return 0
    end = p
    for (j = 1; j <= length(h); j++) {
      c = substr(h, j, 1)
      if (c == "/") {
        if (!match(substr(file, end), /^(\\*\/)+/)) return 0
        end += RLENGTH
      } else if (substr(file, end++, 1) != c) return 0
    }
    return end
  }
  # Walk the original name, including hidden bytes: a later match can overlap and extend
  # the hidden span. Inserted placeholders are never scanned, and homes are matched literally.
  function redact(file,    out, lower, p, i, h, end, name, covered_until, hidden) {
    lower = tolower(file)
    for (p = 1; p <= length(file); p++) {
      hidden = p < covered_until
      for (i = 1; i <= n + nd; i++) {
        h = i <= n ? home[i] : dashed[i - n]
        name = i <= n ? "[a-z0-9_~-]" : "[a-z0-9]"
        end = home_end(lower, p, h)
        # No lead check in a report: a dashed overlap can start inside an earlier home.
        if (end > covered_until && boundary(lower, p, end, name, "")) covered_until = end
      }
      if (!hidden) out = out (p < covered_until ? "<home>" : substr(file, p, 1))
    }
    return out
  }
  # Git C-quotes control characters, quotes and backslashes even with core.quotePath=false.
  # Decode its bytes before matching, then quote the safe result again for display.
  function unquote(file,    out, p, c, oct, k, value) {
    if (length(file) < 2 || substr(file, length(file), 1) != "\"") return "<quoted filename>"
    for (p = 2; p < length(file); p++) {
      c = substr(file, p, 1)
      if (c == "\\") {
        if (++p >= length(file)) return "<quoted filename>"
        c = substr(file, p, 1)
        if (c ~ /[0-7]/) {
          oct = substr(file, p, 3); value = 0
          if (oct !~ /^[0-3][0-7][0-7]$/ || oct == "000") return "<quoted filename>"
          for (k = 1; k <= 3; k++) value = 8 * value + substr(oct, k, 1)
          c = sprintf("%c", value); p += 2
        } else if (c in escape) c = escape[c]
        else return "<quoted filename>"
      }
      out = out c
    }
    return out
  }
  function quote(file,    out, p, c) {
    out = "\""
    for (p = 1; p <= length(file); p++) {
      c = substr(file, p, 1)
      if (c == "\\" || c == "\"") out = out "\\" c
      else if (ord[c] < 32 || ord[c] > 126) out = out sprintf("\\%03o", ord[c])
      else out = out c
    }
    return out "\""
  }
  BEGIN {
    n = split(tolower(ENVIRON["HOMES"]), home, "\n")
    nd = split(tolower(ENVIRON["DASHED"]), dashed, "\n")
    escape["a"] = "\a"; escape["b"] = "\b"; escape["t"] = "\t"; escape["n"] = "\n"
    escape["v"] = "\v"; escape["f"] = "\f"; escape["r"] = "\r"
    escape["\\"] = "\\"; escape["\""] = "\""
    for (i = 1; i <= 255; i++) ord[sprintf("%c", i)] = i
  }
  /^commit [0-9a-f]+([ \t]|$)/ { commit = substr($2, 1, 12) " "; in_hunk = 0; next }
  /^diff --(git|combined|cc) / { in_hunk = 0; next }
  # git ends a name that holds a space with a tab.
  !in_hunk && /^\+\+\+ / {
    file = substr($0, 5); sub(/\t$/, "", file)
    quoted = substr(file, 1, 1) == "\""
    if (quoted) file = unquote(file)
    sub(/^b\//, "/", file)
    file = redact(file)
    sub(/^\//, "", file)
    if (quoted) file = quote(file)
    next
  }
  # "@@ -a,b +c,d @@", or for a merge one @ and one -a,b per parent more: c is the number of
  # the first line the hunk leaves in the file.
  /^@@/ {
    in_hunk = 1
    match($0, /^@+/); parents = RLENGTH - 1
    match($0, /\+[0-9]+/); number = substr($0, RSTART + 1, RLENGTH - 1) + 0
    next
  }
  # diff.suppressBlankEmpty writes a blank context line with no mark at all, and context
  # reaches a -U0 diff through diff.interHunkContext or GIT_DIFF_OPTS, so such a line must
  # still advance the count. An added or removed blank line keeps its mark.
  in_hunk && /^$/ { number++; next }
  # A line starts with one mark per parent: "-" for a line the result drops, "+" for one that
  # parent lacks. Only a line every parent lacks is new here; one a parent has was checked in
  # that parent, or is published already.
  in_hunk && /^[-+ ]/ {
    mark = substr($0, 1, parents)
    if (mark ~ /-/) next
    if (mark ~ / /) { number++; next }
    # Keep a slash-squeezed form with literal POSIX backslashes, plus a decoded form where
    # JSON \/ (or \\/ inside another string) becomes /. Decode runs together so escaped
    # or plain repeated slashes also read as one /.
    squeezed = tolower(substr($0, parents + 1))
    gsub(/\/+/, "/", squeezed)
    line = squeezed
    gsub(/(\\*\/)+/, "/", line)
    hit = 0
    for (i = 1; i <= n && !hit; i++)
      hit = home[i] != "" && (names_home(squeezed, home[i], "[a-z0-9_~-]", "") ||
        names_home(line, home[i], "[a-z0-9_~-]", ""))
    # Dashed homes use only [a-z0-9-]; slash decoding changes no match or boundary.
    for (i = 1; i <= nd && !hit; i++)
      hit = dashed[i] != "" && names_home(line, dashed[i], "[a-z0-9]", "[a-z0-9]")
    if (hit) {
      if (!found++) print "❌ [home-path] these added lines name your home directory, which would be published:"
      printf "     %s%s:%d\n", commit, file, number
    }
    number++
  }
  END {
    if (found) {
      print "   Use a relative path, ~, or a placeholder such as <repo>, then rewrite the commit that adds it."
      exit 1
    }
  }
' >&2; then
  exit 0
else
  statuses=("${PIPESTATUS[@]}")
  # awk uses 1 for findings; producer/tr failures and awk errors also refuse.
  if [ "${statuses[1]}" -ne 0 ] || [ "${statuses[2]}" -gt 1 ]; then
    echo "❌ [home-path] cannot scan the diff." >&2
  fi
  exit 1
fi
