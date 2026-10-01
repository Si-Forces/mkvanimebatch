#!/usr/bin/env bash
# Installs the tracked git hooks into .git/hooks, preserving unrelated hooks.
# Run once per clone:  scripts/install-git-hooks.sh
#
# Idempotent: re-running must not dirty .gitignore or duplicate entries. The first
# version appended the ledger ignore line on every run.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
src="$root/scripts/hooks"
dst="$root/.git/hooks"

install_one() {
  local name="$1"
  [ -f "$src/$name" ] || { echo "missing: scripts/hooks/$name" >&2; exit 1; }
  cp "$src/$name" "$dst/$name"
  chmod +x "$dst/$name"
  echo "  installed .git/hooks/$name"
}

echo "Installing hooks into $dst"
install_one pre-commit
install_one pre-push-slow-gates
# Both push-time hooks read git's ref-update stream on stdin, so the wrapper captures
# it once and replays it to each. Chaining them naively let the first hook drain stdin
# and the second see EOF, silently skipping the review gate.
#
# An empty stream is passed through untouched. Replaying it with `printf '%s\n'` would
# synthesise one blank ref-update line, which the slow-gates hook reads as a push and
# would run the whole test suite for a no-op push.
cat > "$dst/pre-push" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
root=$(git rev-parse --show-toplevel)
updates=$(cat)
if [ -z "${updates//[[:space:]]/}" ]; then
  exit 0
fi
rc=0
printf '%s\n' "$updates" | "$root/.git/hooks/pre-push-slow-gates" "$@" || rc=$?
printf '%s\n' "$updates" | bash "$root/scripts/hooks/pre-push" "$@" || rc=$?
exit "$rc"
WRAPPER
chmod +x "$dst/pre-push"
echo "  installed .git/hooks/pre-push (chains slow gates + review gate)"

# scripts/hooks/pre-push is invoked through `bash` above, so it needs no executable
# bit in the index. scripts/review-gate.sh IS invoked directly and must be executable
# in a fresh clone, so record the mode in git rather than relying on the filesystem.
chmod +x "$root/scripts/review-gate.sh" "$root/scripts/install-git-hooks.sh"
git update-index --chmod=+x scripts/review-gate.sh scripts/install-git-hooks.sh 2>/dev/null || true

# The ledger ignore lives in the committed .gitignore; do not append it here.
if ! grep -qx '.review-ledger.jsonl' "$root/.gitignore"; then
  echo "⚠  .gitignore has no .review-ledger.jsonl entry; add it once by hand." >&2
fi

cat <<'MSG'
Done. Gates now enforced:
  pre-commit : cargo fmt --check + clippy -D warnings (whole workspace), node --check on staged UI
  pre-push   : cargo test --workspace, node --test, then the review-gate ledger check

Review a range with the code-review skill (Standards + Spec, parallel
sub-agents), then record it before pushing:
  scripts/review-gate.sh record-range <base> <head> "Standards+Spec over <base>..<head>, findings fixed"

Check what is still unreviewed:  scripts/review-gate.sh status
MSG