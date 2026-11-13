// Real Git hooks and jj gates, with isolated homes and repositories.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { script, tmp, home, envFor, run, sh, gitRepo, hit, copyScripts, SKIP, hasJj } from "./test-support/home-path.js";

test("pre-commit refuses a staged leak, and pre-push one in any pushed commit", () => {
  const { dir, git } = gitRepo("hooked", { hostile: true });
  git("config", "core.hooksPath", path.dirname(script));
  sh(dir, "git", ["init", "-q", "--bare", path.join(tmp, "hooked.git")]);
  git("remote", "add", "origin", path.join(tmp, "hooked.git"));
  fs.mkdirSync(path.join(dir, "b"));
  const commit = (content, ...flags) => {
    fs.writeFileSync(path.join(dir, "b", "f.txt"), content);
    git("add", "b/f.txt", ".gitattributes");
    return sh(dir, "git", ["commit", "-q", "-m", "c", ...flags]); // may fail, unlike git()
  };

  assert.equal(commit("clean\n").status, 0);
  const leak = commit(`${home}/secret\n`);
  assert.equal(leak.status, 1, leak.out);
  assert.match(leak.out, /name your home directory[^]*\n {5}b\/f\.txt:1\n/);
  assert.equal(sh(dir, "git", ["push", "-q", "origin", "main"], SKIP).status, 0, "a clean push");

  // Added then removed is still published; the bypasses do not skip the check.
  assert.equal(commit(`${home}/secret\n`, "--no-verify").status, 0);
  assert.equal(commit("clean again\n").status, 0);
  const push = sh(dir, "git", ["push", "-q", "origin", "main"], SKIP);
  assert.notEqual(push.status, 0, push.out);
  assert.match(push.out, /name your home directory/);
});

test("pre-commit diffs from the HEAD the commit extends, not a replacement", () => {
  const { dir, git } = gitRepo("replaced-head");
  git("config", "core.hooksPath", path.dirname(script));
  const commit = (content, ...flags) => {
    fs.writeFileSync(path.join(dir, "f.txt"), content);
    git("add", "f.txt");
    return sh(dir, "git", ["commit", "-q", "-m", "c", ...flags]);
  };
  assert.equal(commit("clean\n").status, 0);
  // A stand-in for HEAD whose tree already holds the line.
  assert.equal(commit(`${home}/secret\n`, "--no-verify").status, 0);
  const standIn = git("rev-parse", "HEAD");
  git("reset", "-q", "--hard", "HEAD~");
  git("replace", "HEAD", standIn);
  const r = commit(`${home}/secret\n`);
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /name your home directory/);
});

test("pre-commit checks a staged leak before a dirty Rust tree stops it", () => {
  const { dir, git } = gitRepo("dirty");
  git("config", "core.hooksPath", path.dirname(script));
  fs.writeFileSync(path.join(dir, "a.rs"), "fn a() {}\n");
  git("add", "a.rs");
  git("commit", "-q", "--no-verify", "-m", "base"); // no cargo workspace to check here
  fs.writeFileSync(path.join(dir, "b.rs"), `// ${home}/b.rs\n`);
  git("add", "b.rs");
  fs.writeFileSync(path.join(dir, "a.rs"), "fn a() { }\n"); // unstaged, so the hook stops before cargo
  const r = sh(dir, "git", ["commit", "-q", "-m", "c"]);
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /name your home directory[^]*Rust\/Cargo files differ from the index/);
});

test("pre-push checks every ref and forwards its destination", () => {
  const { dir, git, commit } = gitRepo("two-refs");
  const base = commit("base\n");
  git("update-ref", "refs/remotes/origin/main", base);
  const clean = commit("clean\n");
  git("checkout", "-q", "-b", "topic", base);
  const leak = commit(`${home}/secret\n`);
  // The lines git writes to the hook, one per pushed ref.
  const refs = (...shas) => shas.map((sha, i) => `refs/heads/b${i} ${sha} refs/heads/b${i} ${"0".repeat(40)}\n`).join("");
  const prePush = (stdin) => {
    const r = spawnSync("bash", [path.join(path.dirname(script), "pre-push"), "origin", "url"], { input: stdin, cwd: dir, encoding: "utf8", env: envFor(SKIP) });
    return { status: r.status, stderr: r.stderr };
  };
  assert.equal(prePush(refs(clean)).status, 0, "a clean ref");
  hit(prePush(refs(clean, leak)), "the second ref");
  git("remote", "add", "origin", path.join(tmp, "two-refs.git"));
  git("update-ref", "refs/remotes/origin/main", leak);
  const afterAccepted = commit("clean after accepted history\n");
  assert.deepEqual(prePush(refs(afterAccepted)), { status: 0, stderr: "" }, "a new ref trusts the destination's main");
});

test("real pre-push uses each destination and its old tip instead of origin or unrelated refs", () => {
  const { dir, git, commit } = gitRepo("destination-wiring");
  const base = commit("clean base\n");
  for (const remote of ["origin", "private"]) {
    const target = path.join(tmp, `destination-${remote}.git`);
    git("init", "-q", "--bare", "-b", "main", target);
    git("remote", "add", remote, target);
  }
  git("push", "-q", "private", `${base}:refs/heads/main`);
  const leak = commit(`${home}/secret\n`);
  const accepted = commit("clean again\n");
  git("push", "-q", "origin", `${accepted}:refs/heads/main`);
  git("push", "-q", "private", `${accepted}:refs/heads/already`);
  git("remote", "add", "public-alias", path.join(tmp, "destination-origin.git"));
  git("fetch", "-q", "public-alias");
  const tip = commit("clean new commit\n");
  git("config", "core.hooksPath", path.dirname(script));
  for (const destination of ["private", path.join(tmp, "destination-private.git")]) {
    const pushed = sh(dir, "git", ["push", "-q", destination, `${tip}:refs/heads/topic`], SKIP);
    assert.equal(pushed.status, 1, pushed.out);
    assert.match(pushed.out, new RegExp(`${leak.slice(0, 12)} f\\.txt:1\\n`));
  }
  for (const [destination, ref] of [["public-alias", "topic"], ["private", "already"]]) {
    const pushed = sh(dir, "git", ["push", "-q", destination, `${tip}:refs/heads/${ref}`], SKIP);
    assert.equal(pushed.status, 0, pushed.out);
  }
});

test("jj-gate.sh check and push (jj gate, jj gpush) refuse a leak", { skip: !hasJj && "jj is not installed" }, () => {
  const { dir, git } = gitRepo("jj");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  // An old leak already on main, which trunk removes after the stack below forks: the check
  // diffs from the fork point, so the stack does not count it as re-added.
  fs.writeFileSync(path.join(dir, "old.txt"), `${home}/old\n`);
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  sh(dir, "git", ["init", "-q", "--bare", path.join(tmp, "jj.git")]);
  git("remote", "add", "origin", path.join(tmp, "jj.git"));
  git("rm", "-q", "old.txt");
  git("commit", "-q", "-m", "trunk drops the old leak");
  git("push", "-q", "origin", "main");
  git("reset", "-q", "--hard", "HEAD~"); // the stack below starts from the fork point, not trunk
  const config = path.join(tmp, "jj.toml");
  fs.writeFileSync(config, '[user]\nname = "t"\nemail = "t@example.com"\n[revset-aliases]\n"trunk()" = "main@origin"\n');
  const jj = (...a) => sh(dir, "jj", a, { JJ_CONFIG: config });
  const gate = (...a) => sh(dir, "bash", ["scripts/jj-gate.sh", ...a], { JJ_CONFIG: config, ...SKIP });
  assert.equal(jj("git", "init", "--colocate").status, 0);

  fs.writeFileSync(path.join(dir, "f.txt"), `${home}/secret\n`);
  fs.writeFileSync(path.join(dir, "scripts/broken.js"), "const = ;\n");
  const check = gate("check");
  assert.equal(check.status, 1, check.out);
  // The refusals name only the stack's own leak: old.txt is trunk's, and gone from it.
  assert.match(check.out, /name your home directory[^]*\n {5}f\.txt:1\n/);
  assert.doesNotMatch(check.out, /old\.txt/);
  assert.match(check.out, /node --check failed: scripts\/broken\.js/);
  fs.unlinkSync(path.join(dir, "scripts/broken.js"));
  // All parsed jj output must ignore forced colour, including ids and push plans.
  const colour = path.join(tmp, "jj-colour.toml");
  fs.writeFileSync(colour, `${fs.readFileSync(config, "utf8")}[ui]\ncolor = "always"\n`);
  const coloured = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], { JJ_CONFIG: colour, ...SKIP });
  assert.match(coloured.out, /name your home directory[^]*\n {5}f\.txt:1\n/);
  fs.writeFileSync(path.join(dir, "f.txt"), "clean\n");
  assert.equal(gate("check").status, 0, "a clean stack");
  const cleanColoured = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], { JJ_CONFIG: colour, ...SKIP });
  assert.equal(cleanColoured.status, 0, cleanColoured.out);

  fs.writeFileSync(path.join(dir, "f.txt"), `${home}/secret\n`);
  assert.equal(jj("commit", "-m", "leak").status, 0);
  const leak = jj("log", "--no-graph", "-r", "@-", "-T", "commit_id").out.trim();
  fs.writeFileSync(path.join(dir, "f.txt"), "clean\n");
  assert.equal(jj("commit", "-m", "fix").status, 0);
  assert.equal(jj("bookmark", "create", "topic", "-r", "@-").status, 0);
  // Fetching the branch to origin/topic must not hide it from a later publication scan.
  const tip = jj("log", "--no-graph", "-r", "@-", "-T", "commit_id").out.trim();
  git("update-ref", "refs/remotes/origin/cloud-topic", tip);
  const push = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "topic"], { JJ_CONFIG: colour, ...SKIP });
  assert.equal(push.status, 1, push.out);
  // Per commit: the leak's own commit, though the pushed tip removes it again.
  assert.match(push.out, new RegExp(`name your home directory[^]*\\n {5}${leak.slice(0, 12)} f\\.txt:1\\n`));
  assert.doesNotMatch(push.out, /old\.txt/);

  const privateRepo = path.join(tmp, "jj-private.git");
  assert.equal(sh(dir, "git", ["init", "-q", "--bare", privateRepo]).status, 0);
  git("remote", "add", "private", privateRepo);
  git("push", "-q", "private", `${tip}:refs/heads/main`);
  git("remote", "add", "before-origin", privateRepo);
  git("fetch", "-q", "before-origin");
  for (const [remotes, deletionOnly] of [
    [["origin", "private"], false],
    [["before-origin", "origin"], false],
    [["origin", "private"], true],
  ]) {
    if (deletionOnly) {
      git("push", "-q", "private", `${tip}:refs/heads/topic`, `${tip}:refs/heads/delete-me`);
      assert.equal(jj("bookmark", "track", "topic", "--remote", "private").status, 0);
      assert.equal(jj("bookmark", "track", "topic", "--remote", "origin").status, 0);
      assert.equal(jj("bookmark", "set", "delete-me", "-r", tip).status, 0);
      assert.equal(jj("bookmark", "track", "delete-me", "--remote", "private").status, 0);
      assert.equal(jj("bookmark", "delete", "delete-me").status, 0);
    }
    const args = [...remotes.flatMap(remote => ["--remote", remote]), "-b", "topic"];
    if (deletionOnly) args.push("-b", "delete-me");
    const plan = jj("git", "push", "--dry-run", ...args);
    assert.equal(plan.status, 0, plan.out);
    for (const remote of remotes) assert.ok(plan.out.includes(`Changes to push to ${remote}:`), plan.out);
    if (deletionOnly) assert.match(plan.out, /Changes to push to private:\n  bookmark: delete-me \[delete from [^\n]+\]\nDry-run/);
    const multi = gate("push", ...args);
    assert.equal(multi.status, 1, multi.out);
    assert.match(multi.out, new RegExp(`name your home directory[^]*\\n {5}${leak.slice(0, 12)} f\\.txt:1\\n`));
  }
  assert.equal(jj("bookmark", "create", "clean", "-r", "main@origin").status, 0);
  const cleanPush = gate("push", "--remote", "origin", "--remote", "private", "-b", "clean");
  assert.equal(cleanPush.status, 0, cleanPush.out);
  const existingRepo = path.join(tmp, "jj-existing.git");
  assert.equal(sh(dir, "git", ["init", "-q", "--bare", "-b", "main", existingRepo]).status, 0);
  git("remote", "add", "existing", existingRepo);
  git("push", "-q", "existing", `${git("rev-parse", "refs/remotes/origin/main")}:refs/heads/main`, `${leak}:refs/heads/topic`);
  assert.equal(jj("bookmark", "track", "topic", "--remote", "existing").status, 0);
  const existingPush = gate("push", "--remote", "existing", "-b", "topic");
  assert.equal(existingPush.status, 0, existingPush.out);
  assert.equal(git("--git-dir", existingRepo, "rev-parse", "refs/heads/topic"), tip);
});
