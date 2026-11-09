#!/usr/bin/env bash
# Mutation-tests only the lines changed since <base>, to show that the new tests catch the guards
# they were written for (CODING_STANDARDS.md, "Every guard bites").
#
# Usage: scripts/mutants-diff.sh <base> [cargo-mutants args: one -p <crate> per changed crate]
#
# Run it on committed work (under jj: after `jj commit` or `jj squash`, with @ empty), because
# cargo-mutants mutates a copy of the working tree and the diff's line numbers must match it.
# Pass the branch's fork point as <base> (`git merge-base origin/main HEAD`), or the commit
# before a fix round: the diff is <base> against HEAD, so a <base> that main has moved past also
# mutates main's newer code.
#
# Exit status is cargo-mutants': 0 all mutants caught, or none tested (no Rust changed, or the
# diff changed no line cargo-mutants mutates, such as tests or comments: both say so), 1 a usage
# error (a bad -p, say), 2 some missed, 3 some timed out, 4 the unmutated tests fail, 70 an internal
# error. The script's own refusals are 64 (usage or unknown base), 65 (dirty tree) and 69
# (cargo-mutants missing); a git command that fails stops it with git's own status. Needs
# `cargo install cargo-mutants --locked`. A run builds the crate from scratch in its copy: about
# 4 minutes for a small mkv-core diff.
#
# The copy and its target dir go to MUTANTS_TMPDIR (default ~/.cache/mkv-mutants), never /tmp:
# /tmp is a RAM-backed tmpfs on the dev laptop. TMPDIR, TMP and TEMP all point there, since
# Rust's temp_dir() reads TMPDIR on Unix and TMP/TEMP on Windows. The report goes to MUTANTS_OUT
# (default $MUTANTS_TMPDIR/out), outside the repo, so jj never snapshots it.
set -euo pipefail

if [[ $# -lt 1 || $1 == -* ]]; then
  echo "usage: $0 <base> [cargo-mutants args]" >&2
  exit 64
fi
base=$1
shift

root=$(git rev-parse --show-toplevel)
cd "$root"
git rev-parse --verify --quiet "$base^{commit}" >/dev/null || { echo "unknown base: $base" >&2; exit 64; }
# cargo-mutants copies the working tree, so it must hold exactly HEAD. git status misses edits
# behind an assume-unchanged or skip-worktree bit; the hooks' helper finds those.
source "$root/scripts/hooks/hidden-changes.sh"
# Both are plain assignments so that a failing git stops the script (set -e) rather than reading
# as a clean tree; a substitution inside [[ ]] would not.
hidden=$(hidden_changes)
dirty=$(git status --porcelain --untracked-files=normal)
if [[ -n $dirty || -n $hidden ]]; then
  echo "working tree differs from HEAD; commit first (jj: jj commit, or jj squash with @ empty)" >&2
  [[ -z $hidden ]] || explain_hidden_changes "$hidden"
  exit 65
fi

rust_files=$(git diff --name-only "$base" HEAD -- '*.rs')
if [[ -z $rust_files ]]; then
  echo "no Rust changes since $base; no mutants were tested"
  exit 0
fi
command -v cargo-mutants >/dev/null || { echo "cargo-mutants missing: cargo install cargo-mutants --locked" >&2; exit 69; }

tmp=${MUTANTS_TMPDIR:-$HOME/.cache/mkv-mutants}
out=${MUTANTS_OUT:-$tmp/out}
mkdir -p "$tmp" "$out"
diff="$out/in.diff"
# Two dots on purpose: <base> may be a bookmark commit that a later squash rewrote, so it is no
# longer an ancestor of HEAD, and a merge-base diff would widen back to the whole change.
git diff --no-ext-diff --no-color --src-prefix=a/ --dst-prefix=b/ "$base" HEAD -- '*.rs' >"$diff"

# No jobs override: cargo always reads $CARGO_HOME/config.toml, so the copy's builds keep the
# same `jobs` cap as any other build (2 on the dev laptop).
# The previous report stays: cargo-mutants rotates it to mutants.out.old itself, and --iterate
# reads it to skip the mutants it already caught. The stamp tells this run's report from it.
stamp="$out/run.stamp"
touch "$stamp"
status=0
TMPDIR=$tmp TMP=$tmp TEMP=$tmp cargo mutants --in-diff "$diff" --output "$out" --no-shuffle "$@" || status=$?
# Only a run that tested mutants writes a report (0 all caught, 2 missed, 3 timed out); --list
# and an empty diff exit 0 without one.
if [[ ($status == 0 || $status == 2 || $status == 3) && "$out/mutants.out/outcomes.json" -nt "$stamp" ]]; then
  for f in missed timeout; do
    if [[ -s "$out/mutants.out/$f.txt" ]]; then
      echo "--- $f:"
      cat "$out/mutants.out/$f.txt"
    fi
  done
elif [[ $status == 0 ]]; then
  echo "--- no mutants were tested (the diff has no mutable line, or --list was given)"
fi
exit "$status"
