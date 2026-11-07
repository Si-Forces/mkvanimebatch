#!/usr/bin/env bash
# Mutation-tests only the lines changed since <base>, to show that the new tests catch the guards
# they were written for (CODING_STANDARDS.md, "Every guard bites").
#
# Usage: scripts/mutants-diff.sh <base> [cargo-mutants args, e.g. -p mkv-core]
#
# Run it on committed work (under jj: after `jj commit` or `jj squash`, with @ empty), because
# cargo-mutants mutates a copy of the working tree and the diff's line numbers must match it.
# Exit status is cargo-mutants': 0 all mutants caught (or no Rust changed), 2 some missed, 3 some
# timed out, 4 the unmutated tests fail. The script's own refusals are 64 (usage or unknown
# base), 65 (dirty tree) and 69 (cargo-mutants missing). Needs `cargo install cargo-mutants
# --locked`. A run builds the crate from scratch in its copy: about 4 minutes for a small
# mkv-core diff.
#
# The copy and its target dir go to MUTANTS_TMPDIR (default ~/.cache/mkv-mutants), never /tmp:
# /tmp is a RAM-backed tmpfs on the dev laptop. The report goes to MUTANTS_OUT (default
# $MUTANTS_TMPDIR/out), outside the repo, so jj never snapshots it.
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
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
  echo "working tree differs from HEAD; commit first (jj: jj commit, or jj squash with @ empty)" >&2
  exit 65
fi
command -v cargo-mutants >/dev/null || { echo "cargo-mutants missing: cargo install cargo-mutants --locked" >&2; exit 69; }

rust_files=$(git diff --name-only "$base" HEAD -- '*.rs')
if [[ -z $rust_files ]]; then
  echo "no Rust changes since $base; nothing to mutate"
  exit 0
fi

tmp=${MUTANTS_TMPDIR:-$HOME/.cache/mkv-mutants}
out=${MUTANTS_OUT:-$tmp/out}
mkdir -p "$tmp" "$out"
rm -rf "$out/mutants.out"
diff="$out/in.diff"
# Two dots on purpose: <base> may be a bookmark commit that a later squash rewrote, so it is no
# longer an ancestor of HEAD, and a merge-base diff would widen back to the whole change.
git diff --no-ext-diff --no-color --src-prefix=a/ --dst-prefix=b/ "$base" HEAD -- '*.rs' >"$diff"

# One core stays free for the desktop on the 4-core dev laptop.
status=0
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} TMPDIR=$tmp \
  cargo mutants --in-diff "$diff" --output "$out" --no-shuffle "$@" || status=$?
# Reports exist only for a run that tested mutants (0 all caught, 2 missed, 3 timed out).
if [[ $status == 0 || $status == 2 || $status == 3 ]]; then
  for f in missed timeout; do
    if [[ -s "$out/mutants.out/$f.txt" ]]; then
      echo "--- $f:"
      cat "$out/mutants.out/$f.txt"
    fi
  done
fi
exit "$status"
