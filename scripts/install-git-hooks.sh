#!/usr/bin/env bash
# Installs the tracked git hooks into .git/hooks, preserving any unrelated hooks.
# Run once per clone:  scripts/install-git-hooks.sh
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
install_one pre-push

# Chain the slow gates ahead of the review-gate check rather than replacing the hook.
if [ ! -f "$dst/pre-push-slow-gates" ]; then
  install_one pre-push-slow-gates
fi

cat > "$dst/pre-push" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
root=$(git rev-parse --show-toplevel)
"$root/.git/hooks/pre-push-slow-gates"
exec "$root/scripts/hooks/pre-push" "$@"
WRAPPER
chmod +x "$dst/pre-push"

cat >> "$root/.gitignore" <<'IGN'

# Local review-gate ledger (per-clone, not shared)
.review-ledger.jsonl
IGN

echo ""
echo "Done. Gates now enforced:"
echo "  pre-commit : cargo fmt --check + clippy -D warnings (whole workspace), node --check on staged UI"
echo "  pre-push   : cargo test --workspace, node --test, then the review-gate ledger check"
echo ""
echo "Review a range with the code-review skill (Standards + Spec, parallel"
echo "sub-agents), then record it before pushing:"
echo "  scripts/review-gate.sh record \"Standards+Spec over origin/main..HEAD, findings fixed\""
echo ""
echo "Check what is still unreviewed:  scripts/review-gate.sh status"