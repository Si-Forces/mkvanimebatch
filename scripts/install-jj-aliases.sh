#!/usr/bin/env bash
# Per-clone jj setup: the `jj gate` / `jj gpush` aliases (scripts/jj-gate.sh) and the working-copy
# settings jj needs here. Run once per clone, after `jj git init --colocate`.
#
# jj never runs git hooks, so scripts/install-git-hooks.sh does nothing for `jj commit` or
# `jj git push`; the aliases carry the same gates. Config goes to the repo-level jj config
# (`jj config path --repo`), which lives outside the tree and is not tracked.
set -euo pipefail

command -v jj > /dev/null || { echo "jj is required." >&2; exit 1; }
root=$(jj workspace root)

# The gates run these; a missing or non-executable one would only fail later, mid-gate.
for f in scripts/jj-gate.sh scripts/hooks/commit-msg scripts/review-gate.sh; do
  if [ ! -x "$root/$f" ]; then
    echo "$f is missing or not executable; refusing to set the aliases." >&2
    echo "Run: chmod +x $f  (or jj restore $f if it was deleted)" >&2
    exit 1
  fi
done

# `jj util exec` runs from the current directory, so the alias resolves the script from the
# workspace root: aliases.<name> = ["util", "exec", "--", "bash", "-c",
#   'exec "$(jj workspace root)/scripts/jj-gate.sh" <subcommand> "$@"', "jj-gate"]
set_alias() { # <alias name> <jj-gate.sh subcommand>
  jj config set --repo "aliases.$1" \
    "[\"util\", \"exec\", \"--\", \"bash\", \"-c\", \"exec \\\"\$(jj workspace root)/scripts/jj-gate.sh\\\" $2 \\\"\$@\\\"\", \"jj-gate\"]"
}
set_alias gate check
set_alias gpush push

# .gitattributes normalizes CRLF to LF; git applies it on its own, jj only when asked ("input":
# CRLF to LF on check-in only; "input-output" would write CRLF into every checked-out file), and
# otherwise shows every CRLF file (a Windows copy, a Tauri-generated schema) as modified.
jj config set --repo working-copy.eol-conversion input

cat <<'MSG'
jj repo config set (aliases, and working-copy.eol-conversion = input):
  jj gate           commit-msg trailer ban over trunk()..@, then cargo fmt/clippy when Rust or
                    Cargo files changed, node --check on changed UI modules. Run before `jj new`.
  jj gpush [args]   the trailer ban, the review-gate ledger and the test suites for what
                    `jj git push [args]` would push, then the push. Use it instead of
                    `jj git push`.

Bypass: SKIP_REVIEW_GATE=1 / SKIP_SLOW_GATES=1 jj gpush, or plain `jj git push`.
`git push` still fires scripts/hooks/pre-push (core.hooksPath stays set).

Record a review before pushing (HEAD is @- in a colocated repo):
  scripts/review-gate.sh record-range <base> HEAD "Standards+Spec over <base>..HEAD, findings fixed"
MSG
