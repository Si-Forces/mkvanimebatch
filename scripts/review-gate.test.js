// The review-gate ledger reads the commits a push sends, past local git replace refs and grafts.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { tmp, sh, gitRepo, copyScripts, hasJj } from "./test-support/home-path.js";

// base is on origin/main; hidden changes f.txt and tip reverts it, so with tip grafted onto
// base the walk sees only tip, whose diff from base is empty, and hidden is never checked.
const fixture = (name) => {
  const { dir, git, commit } = gitRepo(name);
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("base\n");
  const remote = path.join(tmp, `${name}.git`);
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const hidden = commit("unreviewed\n");
  const tip = commit("base\n");
  const gate = (...args) => sh(dir, "bash", ["scripts/review-gate.sh", ...args]);
  const record = (from) => {
    const r = gate("record-range", from, tip, "fixture review");
    assert.equal(r.status, 0, r.out);
    return r.out;
  };
  record(hidden); // only tip, so a refusal can only name hidden
  return { dir, git, commit, gate, record, base, hidden, tip };
};
const refusesHidden = (r, hidden) => {
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, new RegExp(`${hidden.slice(0, 7)}\\w* has no recorded review`));
  assert.doesNotMatch(r.out, /info\/grafts is deprecated/, "a readable graft file makes git print a deprecation hint");
};
const hides = {
  "git replace --graft": ({ git, tip, base }) => git("replace", "--graft", tip, base),
  "info/grafts": ({ dir, tip, base }) => fs.writeFileSync(path.join(dir, ".git", "info", "grafts"), `${tip} ${base}\n`),
};
const replaceGraft = hides["git replace --graft"];

for (const [how, hide] of Object.entries(hides)) {
  test(`verify-push refuses a commit hidden by ${how}`, () => {
    const f = fixture(`verify-${how.replace(/\W+/g, "-")}`);
    hide(f);
    refusesHidden(f.gate("verify-push", "0".repeat(40), f.tip), f.hidden);
  });
}

test("pre-push refuses a commit hidden by a replace ref", () => {
  const f = fixture("pre-push-replace");
  copyScripts(f.dir, ["scripts/hooks/pre-push", "scripts/hooks/hidden-changes.sh"]);
  f.git("config", "core.hooksPath", "scripts/hooks");
  replaceGraft(f);
  refusesHidden(sh(f.dir, "git", ["push", "-q", "origin", `${f.tip}:refs/heads/topic`], { SKIP_SLOW_GATES: "1" }), f.hidden);
});

test("jj gpush refuses a commit hidden by a replace ref", { skip: !hasJj && "jj is not installed" }, () => {
  const f = fixture("jj-replace");
  const config = path.join(tmp, "jj-replace.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const jj = (...args) => sh(f.dir, "jj", ["--color", "never", ...args], { JJ_CONFIG: config });
  for (const args of [["git", "init", "--colocate"], ["bookmark", "create", "topic", "-r", `commit_id(${f.tip})`]]) {
    const r = jj(...args);
    assert.equal(r.status, 0, r.out);
  }
  replaceGraft(f);
  refusesHidden(sh(f.dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "topic"], { JJ_CONFIG: config, SKIP_SLOW_GATES: "1" }), f.hidden);
});

test("a commit's patch id ignores a replace ref on its parent", () => {
  const f = fixture("patch-id");
  f.record(f.base);
  // Replace hidden with a commit on base whose tree is unlike hidden's and tip's; a scratch
  // commit on main supplies that tree.
  const tree = f.git("rev-parse", `${f.commit("other\n", "g.txt")}^{tree}`);
  f.git("replace", f.hidden, f.git("commit-tree", tree, "-p", f.base, "-m", "other"));
  const r = f.gate("verify-push", "0".repeat(40), f.tip);
  assert.equal(r.status, 0, r.out);
});

test("record-range records the stored commits of a grafted range", () => {
  const f = fixture("record-grafted");
  replaceGraft(f);
  assert.match(f.record(f.base), /recorded review for 2 commit\(s\)/);
  f.git("replace", "-d", f.tip);
  const pushed = f.gate("verify-push", "0".repeat(40), f.tip);
  assert.equal(pushed.status, 0, pushed.out);
});
