// Real jj push plans: tests on a merge tree cannot vouch for either parent tree.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { tmp, sh, gitRepo, copyScripts, hasJj } from "./test-support/home-path.js";

const options = { skip: !hasJj && "jj is not installed" };
const fixture = (name, { merge, dirty, emptySibling = false }) => {
  const { dir, git, commit } = gitRepo(name);
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, `${name}.git`);
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const config = path.join(tmp, `${name}.toml`);
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const jj = (...args) => {
    const r = sh(dir, "jj", ["--color", "never", ...args], { JJ_CONFIG: config });
    assert.equal(r.status, 0, r.out);
    return r.out.trim();
  };
  jj("git", "init", "--colocate");
  const parents = [];
  for (const side of merge ? ["left", "right"] : ["left"]) {
    jj("new", `commit_id(${base})`);
    if (!emptySibling || side !== "right") fs.writeFileSync(path.join(dir, `${side}.txt`), `${side}\n`);
    jj("commit", "-m", side);
    parents.push(jj("log", "-r", "@-", "--no-graph", "-T", "commit_id"));
  }
  jj("new", ...parents.map(id => `commit_id(${id})`), "-m", "working copy");
  if (dirty) fs.writeFileSync(path.join(dir, "working.txt"), "working copy only\n");
  const current = jj("log", "-r", "@", "--no-graph", "-T", "commit_id");
  assert.equal(jj("log", "-r", "empty() & @", "--no-graph", "-T", "commit_id"), dirty ? "" : current);
  if (merge && !emptySibling) for (const parent of parents) {
    assert.notEqual(git("rev-parse", `${current}^{tree}`), git("rev-parse", `${parent}^{tree}`));
  }
  parents.forEach((id, i) => jj("bookmark", "create", `parent-${i}`, "-r", `commit_id(${id})`));
  jj("bookmark", "create", "working", "-r", `commit_id(${current})`);
  jj("bookmark", "create", "unrelated", "-r", `commit_id(${base})`);
  const recorded = sh(dir, "bash", ["scripts/review-gate.sh", "record-range", base, current, "fixture review"]);
  assert.equal(recorded.status, 0, recorded.out);

  // Observe suite dispatch without launching cargo or recursively running the Node suite.
  const bin = path.join(tmp, `${name}-bin`);
  const log = path.join(tmp, `${name}-checks`);
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "cargo"), '#!/usr/bin/env bash\nprintf "cargo %s\\n" "$*" >> "$GATE_CHECKS"\n');
  fs.writeFileSync(path.join(bin, "node"), '#!/usr/bin/env bash\nif [ "$1" = --test ]; then echo "node tests" >> "$GATE_CHECKS"; exit 0; fi\nexec "$REAL_NODE" "$@"\n');
  for (const file of ["cargo", "node"]) fs.chmodSync(path.join(bin, file), 0o755);
  const checks = () => fs.existsSync(log) ? fs.readFileSync(log, "utf8").trim().split("\n") : [];
  const gate = (...bookmarks) => sh(dir, "bash", ["scripts/jj-gate.sh", "push", ...bookmarks.flatMap(b => ["-b", b])], {
    JJ_CONFIG: config, PATH: bin + path.delimiter + process.env.PATH, GATE_CHECKS: log, REAL_NODE: process.execPath,
  }); // real privacy and review gates, no SKIP variables
  const remoteTip = bookmark => sh(dir, "git", ["--git-dir", remote, "rev-parse", "--verify", `refs/heads/${bookmark}`]);
  return { jj, git, gate, checks, base, parents, current, remoteTip };
};

test("jj tests an empty single parent and refuses a changed working copy", options, () => {
  for (const dirty of [false, true]) {
    const f = fixture(`single-parent-${dirty}`, { merge: false, dirty });
    const r = f.gate("parent-0");
    if (dirty) {
      assert.equal(r.status, 1, r.out);
      assert.match(r.out, /the tests would not run on it/);
      assert.notEqual(f.remoteTip("parent-0").status, 0);
      assert.deepEqual(f.checks(), []);
    } else {
      assert.equal(r.status, 0, r.out);
      assert.equal(f.remoteTip("parent-0").out.trim(), f.parents[0]);
      assert.deepEqual(f.checks(), ["cargo test --workspace", "node tests"]);
    }
  }
});

test("jj refuses either merge parent unless the working-copy merge is also pushed", options, () => {
  for (const dirty of [false, true]) {
    const f = fixture(`merge-parent-${dirty}`, { merge: true, dirty });
    const unrelated = f.gate("unrelated");
    assert.equal(unrelated.status, 0, unrelated.out);
    assert.match(unrelated.out, /local test suites do not cover it/);
    assert.equal(f.remoteTip("unrelated").out.trim(), f.base);
    assert.deepEqual(f.checks(), []);
    for (const bookmarks of [["parent-0"], ["parent-1"], ["parent-0", "parent-1"]]) {
      const r = f.gate(...bookmarks);
      assert.equal(r.status, 1, r.out);
      assert.match(r.out, /the tests would not run on it/);
      for (const name of bookmarks) assert.notEqual(f.remoteTip(name).status, 0);
      assert.deepEqual(f.checks(), []);
    }
    const pushed = f.gate("parent-0", "parent-1", "working");
    assert.equal(pushed.status, 0, pushed.out);
    assert.deepEqual(f.checks(), ["cargo test --workspace", "node tests"]);
    for (const [name, id] of [["parent-0", f.parents[0]], ["parent-1", f.parents[1]], ["working", f.current]]) {
      assert.equal(f.remoteTip(name).out.trim(), id);
    }
  }
});

test("jj's printed parent remedy runs the suites even when a merge tree equals that parent", options, () => {
  const f = fixture("merge-parent-remedy", { merge: true, dirty: false, emptySibling: true });
  assert.equal(f.git("rev-parse", `${f.current}^{tree}`), f.git("rev-parse", `${f.parents[0]}^{tree}`));
  const refused = f.gate("parent-0");
  assert.equal(refused.status, 1, refused.out);
  assert.match(refused.out, /the pushed parent has no working-copy test coverage/);
  assert.doesNotMatch(refused.out, /tree differs/);
  assert.deepEqual(f.checks(), []);
  assert.notEqual(f.remoteTip("parent-0").status, 0);
  const remedy = refused.out.match(/jj new '(commit_id\([0-9a-f]+\))'/)?.[1];
  assert.equal(remedy, `commit_id(${f.parents[0]})`, refused.out);
  f.jj("new", remedy);
  const pushed = f.gate("parent-0");
  assert.equal(pushed.status, 0, pushed.out);
  assert.deepEqual(f.checks(), ["cargo test --workspace", "node tests"]);
  assert.equal(f.remoteTip("parent-0").out.trim(), f.parents[0]);
});
