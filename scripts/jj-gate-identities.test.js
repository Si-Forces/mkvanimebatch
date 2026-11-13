// Real jj identity boundaries: machine-produced IDs must never resolve as ref names.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { tmp, home, envFor, sh, gitRepo, copyScripts, hasJj, SKIP } from "./test-support/home-path.js";

const options = { skip: !hasJj && "jj is not installed" };
const fixture = (name, { existing = false, trailer = false, intermediate = false } = {}) => {
  const { dir, git, commit } = gitRepo(`identity-${name}`);
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, `identity-${name}.git`);
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  let bad;
  if (trailer) {
    fs.writeFileSync(path.join(dir, "f.txt"), "clean content\n");
    git("add", "f.txt");
    git("commit", "-q", "-m", "fixture\n\nCo-authored-by: Claude <noreply@anthropic.com>");
    bad = git("rev-parse", "HEAD");
  } else bad = commit(`${home}/secret\n`);
  const tip = !trailer || intermediate ? commit("clean again\n") : bad;
  git("branch", "topic", tip);
  commit("working copy is on a different commit\n"); // unrelated gates must not mask a wrong planned tip
  if (existing) git("push", "-q", "origin", `${base}:refs/heads/topic`);
  const config = path.join(tmp, `identity-${name}.toml`);
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const jj = (...args) => {
    const r = sh(dir, "jj", ["--color", "never", ...args], { JJ_CONFIG: config });
    assert.equal(r.status, 0, r.out);
    return r.out.trim();
  };
  jj("git", "init", "--colocate");
  if (existing) jj("bookmark", "track", "topic", "--remote", "origin");
  const gate = (env = {}) => sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "topic"], { JJ_CONFIG: config, ...env });
  const shadow = (kind, id, target) => {
    if (kind === "bookmark") jj("bookmark", "create", id, "-r", `commit_id(${target})`);
    else { git("tag", id, target); jj("log", "-r", "@", "--no-graph", "-T", "''"); }
  };
  const remoteTip = () => sh(dir, "git", ["--git-dir", remote, "rev-parse", "--verify", "refs/heads/topic"]);
  return { dir, git, jj, gate, shadow, remoteTip, config, base, bad, tip };
};

test("jj refuses leaking new and existing refs despite commit-prefix bookmarks and tags", options, () => {
  for (const kind of ["bookmark", "tag"]) for (const existing of [false, true]) {
    const f = fixture(`${kind}-${existing}`, { existing });
    const plan = f.jj("git", "push", "--dry-run", "-b", "topic");
    const id = existing ? plan.match(/move forward from ([0-9a-f]+) to /)?.[1] : plan.match(/add to ([0-9a-f]+)/)?.[1];
    assert.ok(id, plan);
    f.shadow(kind, id, existing ? f.tip : f.base);
    const r = f.gate(); // no SKIP variables: the wrong resolution previously allowed a real push
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /name your home directory/);
    assert.ok(r.out.includes(`${f.bad.slice(0, 12)} f.txt:1\n`), r.out);
    const remote = f.remoteTip();
    if (existing) assert.equal(remote.out.trim(), f.base);
    else assert.notEqual(remote.status, 0, remote.out);
  }
});

test("jj checks the trailer range and each description despite full-ID bookmarks and tags", options, () => {
  for (const kind of ["bookmark", "tag"]) for (const intermediate of [false, true]) {
    const f = fixture(`trailer-${kind}-${intermediate}`, { trailer: true, intermediate });
    f.shadow(kind, f.bad, f.base); // endpoint when false, intermediate description lookup when true
    const r = f.gate(SKIP); // only unrelated ledger/test gates; trailer refusal is unconditional
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /AI co-author|AI agent|names an AI/i);
    assert.ok(r.out.includes(`jj describe 'commit_id(${f.bad})'`), r.out);
    assert.notEqual(f.remoteTip().status, 0);
  }
});

test("jj rejects nonhex plan operands before resolving or pushing them", options, () => {
  const f = fixture("invalid-plan");
  const realJj = sh(f.dir, "bash", ["-c", "command -v jj"]).out.trim();
  const bin = path.join(f.dir, "fake-bin");
  fs.mkdirSync(bin);
  const marker = path.join(f.dir, "pushed");
  fs.writeFileSync(path.join(bin, "jj"), [
    '#!/usr/bin/env bash', 'case " $* " in',
    '*" git push --dry-run "*) cat "$PLAN"; exit 0 ;;',
    '*" git push "*) touch "$PUSH_MARKER"; exit 99 ;;',
    '*" log -r commit_id("*) if [ "${ID_OUTPUT+x}" = x ]; then printf "%s" "$ID_OUTPUT"; exit "${ID_STATUS:-0}"; fi ;;',
    '*" log -r @ --no-graph -T commit_id "*) [ "${FAIL_QUERY:-}" != current ] || exit 1 ;;',
    '*" log -r @- --no-graph -T commit_id "*) [ "${FAIL_QUERY:-}" != parents ] || exit 1 ;;',
    '*" log -r empty() & @ --no-graph -T commit_id "*) [ "${FAIL_QUERY:-}" != empty ] || exit 1 ;;',
    'esac', 'exec "$REAL_JJ" "$@"', '',
  ].join("\n"));
  fs.chmodSync(path.join(bin, "jj"), 0o755);
  const plan = path.join(f.dir, "plan.txt");
  for (const field of ["old", "new"]) for (const invalid of ["", "@", "ROOT()", "dead) | all() | commit_id(dead", "f".repeat(40)]) {
    const old = field === "old" ? invalid : f.base.slice(0, 12);
    const next = field === "new" ? invalid : f.tip.slice(0, 12);
    fs.writeFileSync(plan, `Changes to push to origin:\n  bookmark: topic [move forward from ${old} to ${next}]\n`);
    const r = f.gate({ PATH: bin + path.delimiter + process.env.PATH, PLAN: plan, PUSH_MARKER: marker, REAL_JJ: realJj });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, invalid === "f".repeat(40) ? /cannot resolve/ : /cannot read commit ids in the push plan/);
    assert.equal(fs.existsSync(marker), false);
  }
  fs.writeFileSync(plan, `Changes to push to origin:\n  bookmark: topic [add to ${f.tip.slice(0, 12)}]\n`);
  const gateEnv = { PATH: bin + path.delimiter + process.env.PATH, PLAN: plan, PUSH_MARKER: marker, REAL_JJ: realJj };
  for (const id of ["", ...[39, 41, 63, 65, 80].map(n => "a".repeat(n)), "x" + f.tip, f.tip + "x", f.tip.toUpperCase(), f.tip + "\n" + f.base]) {
    const r = f.gate({ ...gateEnv, ID_OUTPUT: id });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /cannot resolve/);
    assert.equal(fs.existsSync(marker), false);
  }
  const failedLog = f.gate({ ...gateEnv, ID_OUTPUT: f.tip, ID_STATUS: "1" });
  assert.equal(failedLog.status, 1, failedLog.out);
  assert.match(failedLog.out, /cannot resolve/);
  assert.equal(fs.existsSync(marker), false);
  // Accept the other supported full width at this boundary; Git then rejects its absent object.
  const sha256 = f.gate({ ...gateEnv, ID_OUTPUT: "a".repeat(64) });
  assert.equal(sha256.status, 1, sha256.out);
  assert.match(sha256.out, /cannot list the commits to check/);
  assert.equal(fs.existsSync(marker), false);
  fs.writeFileSync(plan, `Changes to push to origin:\n  bookmark: topic [add to ${f.base.slice(0, 12)}]\n`);
  for (const [query, diagnostic] of [
    ["current", /cannot resolve the working-copy commit/],
    ["parents", /cannot read the working-copy parents/],
    ["empty", /cannot check whether the working copy is empty/],
  ]) {
    const r = f.gate({ ...gateEnv, FAIL_QUERY: query });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, diagnostic);
    assert.equal(fs.existsSync(marker), false);
  }
});

test("child temporary files stay inside the home-path fixture directory", () => {
  const env = envFor({});
  for (const key of ["TMPDIR", "TMP", "TEMP"]) assert.equal(env[key], tmp, key);
  const r = sh(tmp, "bash", ["-c", 'p=$(mktemp) || exit; case "$p" in "$TMPDIR"/*) rm -f "$p" ;; *) rm -f "$p"; exit 1 ;; esac']);
  assert.equal(r.status, 0, r.out);
});
