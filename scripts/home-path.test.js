// scripts/hooks/home-path.sh, fed diffs on stdin (or pointed at a throwaway git repo) with a
// throwaway HOME, so the machine's real home directory never decides a result.
import test, { after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "hooks", "home-path.sh");
const tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "home-path-")));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));
const home = path.join(tmp, "tester");
fs.mkdirSync(home);

// git without the machine's own config, so a global hooksPath or gpgsign cannot flake a test.
const GIT_ENV = {
  GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@example.com",
  GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@example.com",
  GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
};
// Only PATH, HOME, USERPROFILE and the git settings reach the script; an undefined value
// unsets that variable.
const envFor = (env) => {
  const e = { PATH: process.env.PATH, HOME: home, ...GIT_ENV, ...env };
  for (const k of Object.keys(e)) if (e[k] === undefined) delete e[k];
  return e;
};
const run = (diff, env = {}, { args = [], cwd = tmp } = {}) => {
  const r = spawnSync("bash", [script, ...args], { input: diff, cwd, encoding: "utf8", env: envFor(env) });
  return { status: r.status, stderr: r.stderr };
};
// Runs a command in a directory; returns { status, out } with stdout and stderr together.
const sh = (cwd, cmd, args, env = {}) => {
  const r = spawnSync(cmd, args, { cwd, encoding: "utf8", env: envFor(env) });
  return { status: r.status, out: `${r.stdout}${r.stderr}${r.error ?? ""}` };
};
// A fresh git repo; git(...) asserts success and returns stdout.
// hostile: git settings that would each hide an added line from a plain `git diff` or
// `git log -p`: a textconv driver that prints nothing, forced colour, an external diff that
// prints nothing (`git diff` only), no diff for the first commit (`git log` only), and no
// a/ b/ prefixes, which would report a file under a folder named b/ without it.
const gitRepo = (name, { hostile = false } = {}) => {
  const dir = path.join(tmp, name);
  fs.mkdirSync(dir);
  const git = (...a) => {
    const r = sh(dir, "git", a);
    assert.equal(r.status, 0, r.out);
    return r.out.trim();
  };
  git("init", "-q", "-b", "main");
  if (hostile) {
    fs.writeFileSync(path.join(dir, ".gitattributes"), "*.txt diff=hide\n");
    git("config", "diff.hide.textconv", "sed d");
    git("config", "diff.external", "true");
    git("config", "color.ui", "always");
    git("config", "log.showRoot", "false");
    git("config", "diff.noprefix", "true");
  }
  return { dir, git };
};
// A refusal for naming the home, not for some other failure.
const hit = (r, message) => {
  assert.equal(r.status, 1, message);
  assert.match(r.stderr, /name your home directory/, message);
};
const diff = (file, ...lines) => `diff --git a/${file} b/${file}\n--- a/${file}\n+++ b/${file}\n@@ -1 +1 @@\n${lines.join("\n")}\n`;

test("refuses an added line naming HOME, with the file and line number but not the text", () => {
  const r = run(diff("a.js", `+const REPO = '${home}/My Project/repo'`));
  hit(r);
  // HOME and its resolved form are the same path here: one line per hit, not one per form.
  assert.equal(r.stderr.split("a.js:1\n").length - 1, 1, r.stderr);
  // The report may end up in a PR description, which nothing checks.
  assert.ok(!r.stderr.includes(home) && !r.stderr.includes("My Project"), r.stderr);
});

test("allows generic home paths, removed lines and context", () => {
  assert.equal(run(diff("t.rs", '+assert_eq!(p("/home/user/show"), "/home/u/App.AppImage");')).status, 0);
  assert.equal(run(diff("a.js", `-const REPO = '${home}/repo'`, `+const REPO = A.repo`)).status, 0);
  assert.equal(run(diff("a.js", ` const REPO = '${home}/repo'`)).status, 0);
  assert.equal(run("").status, 0);
});

test("matches HOME only as whole path segments, in any case", () => {
  assert.equal(run(diff("a.rs", `+mod x; // ${home}z/lib.rs`)).status, 0, "a longer name");
  hit(run(diff("a.rs", `+let p = "src${home}";`)), "inside another path");
  hit(run(diff("a.sh", `+gcc -I${home}/include`)), "after a flag");
  hit(run(diff("a.sh", `+cp /mnt/backup${home}/x .`)), "under a mount point");
  hit(run(diff("a.sh", `+cp ${home}z/a ${home}/b`)), "after an occurrence that does not count");
  hit(run(diff("a.md", `+[doc](file://${home}/doc.md)`)), "after a file URL");
  hit(run(diff("a.md", `+see ${home}`)), "at the end of a line");
  hit(run(diff("a.md", `+cloned into ${home}.`)), "at the end of a sentence");
  hit(run(diff("a.md", `+cloned into ${home}...`)), "before an ellipsis");
  hit(run(diff("a.md", `+see ${home}...more`)), "before an ellipsis and more text");
  hit(run(diff("a.sh", `+git diff ${home}..main`)), "in a git range");
  assert.equal(run(diff("a.md", `+cp ${home}.bak/x .`)).status, 0, "a sibling with a suffix");
  hit(run(diff("a.md", `+see ${home.toUpperCase()}/x`)), "in another case");
});

test("names the commit in git log output", () => {
  const sha = "0123456789abcdef0123456789abcdef01234567";
  const r = run(`commit ${sha}\n${diff(".codex/x.toml", `+see ${home}/Downloads/doc.md`)}`);
  hit(r);
  assert.ok(r.stderr.includes("0123456789ab .codex/x.toml:1\n"), r.stderr);
  // git writes "+++ b/<name><tab>" for a name that holds a space.
  const spaced = run(`diff --git a/my file.md b/my file.md\n--- a/my file.md\t\n+++ b/my file.md\t\n@@ -0,0 +1 @@\n+${home}/x\n`);
  assert.ok(spaced.stderr.includes("     my file.md:1\n"), spaced.stderr);
});

test("a file header is not content, even before the first hunk", () => {
  // Repo-relative names are not read, so a fixture folder shaped like the home passes.
  const named = `a${home}/x`;
  const deleted = `diff --git ${named} b/y\ndeleted file mode 100644\n--- ${named}\n+++ /dev/null\n@@ -1 +0,0 @@\n-clean\n`;
  assert.deepEqual(run(deleted), { status: 0, stderr: "" });
});

test("a +++ or --- line inside a hunk is content, not a file header", () => {
  const r = run(`${diff("a.sql", "-- old")}+++ ${home}/notes\n`);
  hit(r);
  assert.ok(r.stderr.includes("a.sql:1\n"), r.stderr);
});

test("numbers a line by its place in the new file", () => {
  const r = run(
    "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n" +
      `@@ -1,2 +1,3 @@\n context\n+clean\n+${home}/x\n` +
      `@@ -10 +20,2 @@\n-old\n+clean\n+${home}/y\n`,
  );
  hit(r);
  assert.ok(r.stderr.includes("a.md:3\n") && r.stderr.includes("a.md:21\n"), r.stderr);
});

test("checks USERPROFILE with backslashes and with forward slashes", () => {
  const env = { USERPROFILE: "C:\\Users\\tester" };
  hit(run(diff("a.ps1", "+cd C:\\Users\\tester\\repo"), env));
  hit(run(diff("a.ps1", "+cd c:\\users\\tester\\repo"), env));
  hit(run(diff("a.js", "+'C:/Users/tester/repo'"), env));
  const escaped = diff("a.toml", '+path = "C:\\\\Users\\\\tester\\\\repo"');
  hit(run(escaped, env), "escaped in a string");
  // bash before 4.3 keeps the quotes of a quoted replacement, as macOS's bash 3.2 does.
  hit(run(escaped, { ...env, BASH_COMPAT: "42" }), "escaped, on an old bash");
  hit(run(diff("a.bat", "+cl -IC:\\Users\\tester\\inc"), env), "after a flag");
  assert.equal(run(diff("a.js", "+'C:/Users/other/repo'"), env).status, 0);
});

test("ignores trailing separators on USERPROFILE and HOME", () => {
  const line = diff("a.ps1", "+cd C:\\Users\\tester\\repo");
  for (const p of ["C:\\Users\\tester\\", "C:/Users/tester/", "C:\\Users\\tester\\\\"]) {
    hit(run(line, { USERPROFILE: p }), p);
  }
  // A drive alone names nobody, as a HOME of / does not.
  assert.equal(run(diff("a.ps1", "+cd C:\\tools"), { USERPROFILE: "C:\\" }).status, 0);
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester//" }));
});

test("a doubled separator inside HOME or USERPROFILE counts squeezed and as set", () => {
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home//tester" }));
  hit(run(diff("a.sh", "+ls /no-such-home//tester/x"), { HOME: "/no-such-home//tester/" }), "as set");
  const profile = { USERPROFILE: "C:\\Users\\\\tester\\" };
  hit(run(diff("a.ps1", "+cd C:\\Users\\tester\\repo"), profile));
  hit(run(diff("a.ps1", "+cd C:\\Users\\\\tester\\repo"), profile), "USERPROFILE as set");
});

test("checks the \\/ a JSON writer may put for /", () => {
  const escaped = `${home}/x`.replaceAll("/", "\\/");
  hit(run(diff("a.json", `+{"p":"${escaped}"}`)));
  const env = { USERPROFILE: "C:\\Users\\tester" };
  hit(run(diff("a.json", '+{"p":"C:\\/Users\\/tester\\/x"}'), env), "USERPROFILE");
  // That escape inside another string doubles the backslash.
  const twice = `${home}/x`.replaceAll("/", "\\\\/");
  hit(run(diff("a.js", `+const s = "{\\"p\\":\\"${twice}\\"}";`)), "escaped twice");
});

test("a home of one segment names nobody, so it is not looked for", () => {
  const lines = ["+cd /no-such-root/x", "+cd //no-such-root/x", "+cd C:\\app\\x", "+cd C:\\\\app\\x"];
  for (const env of [{ HOME: "/no-such-root" }, { HOME: "//no-such-root/", USERPROFILE: "C:\\app" }, { HOME: "/", USERPROFILE: "C:\\\\app" }]) {
    const r = run(diff("a.sh", ...lines), env);
    assert.equal(r.status, 0);
    assert.match(r.stderr, /name no home directory/);
  }
  // Each form by itself: a one-segment HOME that resolves to a longer path is looked for
  // there. A one-segment folder sits directly under /, where only root can make one, so a
  // relative HOME stands in for it.
  fs.symlinkSync(home, path.join(tmp, "short"));
  const r = run(diff("a.js", `+'${home}/repo'`), { HOME: "short" });
  hit(r);
  assert.doesNotMatch(r.stderr, /name no home directory/);
  // And the other way: a longer HOME that resolves to one segment is looked for only as written.
  const top = ["/tmp", "/usr", "/var"].find((d) => fs.existsSync(d) && fs.realpathSync(d) === d);
  assert.ok(top, "a folder directly under / that is not a symlink");
  fs.symlinkSync(top, path.join(tmp, "to-top"));
  assert.deepEqual(run(diff("a.sh", `+ls ${top}/x`), { HOME: path.join(tmp, "to-top") }), { status: 0, stderr: "" });
});

test("refuses a HOME it cannot resolve, and warns when there is no home to look for", () => {
  const dangling = path.join(tmp, "dangling");
  fs.symlinkSync(path.join(tmp, "nowhere"), dangling);
  const loop = path.join(tmp, "loop");
  fs.symlinkSync(path.join(tmp, "loop2"), loop);
  fs.symlinkSync(loop, path.join(tmp, "loop2"));
  for (const h of [dangling, loop, "dangling"]) { // the last one relative: one segment
    const r = run(diff("a.txt", "+clean"), { HOME: h });
    assert.equal(r.status, 1, h);
    assert.match(r.stderr, /cannot resolve HOME.*symlink/);
  }
  const file = path.join(tmp, "a-file");
  fs.writeFileSync(file, "");
  assert.match(run(diff("a.txt", "+clean"), { HOME: file }).stderr, /HOME: not a folder this user can enter/);
  if (process.getuid?.() !== 0) { // root enters any directory
    const locked = path.join(tmp, "locked");
    fs.mkdirSync(locked, { mode: 0o000 });
    try {
      assert.match(run(diff("a.txt", "+clean"), { HOME: locked }).stderr, /HOME: not a folder this user can enter/, "no permission");
      // Under a folder that cannot be searched, a missing home cannot be told from a hidden one.
      const r = run(diff("a.txt", "+clean"), { HOME: path.join(locked, "tester") });
      assert.equal(r.status, 1, "under no permission");
      assert.match(r.stderr, /above it is not a folder/);
    } finally {
      fs.chmodSync(locked, 0o700); // so the cleanup can remove it
    }
  }
  // Under a dangling symlink the same holds; under missing folders the home is just absent.
  const under = run(diff("a.txt", "+clean"), { HOME: path.join(dangling, "tester") });
  assert.equal(under.status, 1, "under a dangling symlink");
  assert.match(under.stderr, /above it is not a folder/);
  assert.ok(!under.stderr.includes(tmp), "no message names the home it could not resolve");
  const absent = run(diff("a.txt", "+clean"), { HOME: "/no-such-home/tester" });
  assert.deepEqual(absent, { status: 0, stderr: "" }, "two missing levels");
  for (const env of [{ HOME: undefined }, { HOME: "/" }, { HOME: "/", USERPROFILE: "C:\\" }]) {
    const r = run(diff("a.txt", "+clean"), env);
    assert.equal(r.status, 0);
    assert.match(r.stderr, /name no home directory/);
  }
  // USERPROFILE alone is a home to look for.
  const windows = run(diff("a.txt", "+clean"), { HOME: "/", USERPROFILE: "C:\\Users\\tester" });
  assert.deepEqual(windows, { status: 0, stderr: "" });
});

test("checks the resolved form of a symlinked HOME", () => {
  const link = path.join(tmp, "link");
  fs.symlinkSync(home, link);
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: link }));
  // A relative HOME is resolved from the current folder, not from CDPATH.
  fs.mkdirSync(path.join(tmp, "elsewhere", "tester"), { recursive: true });
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: "tester", CDPATH: path.join(tmp, "elsewhere") }), "CDPATH");
  // Nor is a HOME of - read as cd's "the previous folder".
  fs.symlinkSync(home, path.join(tmp, "-"));
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: "-" }), "a HOME of -");
});

test("the locale changes neither the case folding nor the parse", () => {
  // A Turkish locale folds I to a dotless i. Where it is not installed, awk falls back to C.
  const turkish = { HOME: "/no-such-home/TESTERI", LC_ALL: "tr_TR.UTF-8" };
  hit(run(diff("a.sh", "+ls /no-such-home/testeri/x"), turkish), "Turkish");
  // A diff is bytes: a line that is not UTF-8 must not derail the lines after it.
  const bytes = Buffer.concat([Buffer.from(diff("a.bin", "+clean")), Buffer.from("+"), Buffer.from([0xff, 0xfe]), Buffer.from(` ${home}/x\n`)]);
  const r = run(bytes, { LC_ALL: "C.UTF-8" });
  hit(r);
  assert.ok(r.stderr.includes("     a.bin:2\n") && !r.stderr.includes("warning"), r.stderr);
});

test("a HOME of / checks nothing, and a trailing slash on HOME is ignored", () => {
  assert.equal(run(diff("a.sh", "+cd / && ls"), { HOME: "/" }).status, 0);
  // Before bash 5.3, `cd ""` stays in the current directory; the script must not take that
  // for the home.
  const work = path.join(tmp, "work");
  fs.mkdirSync(work);
  assert.equal(run(diff("a.sh", `+ls ${work}/x`), { HOME: "/" }, { cwd: work }).status, 0);
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester/" }));
});

test("--published checks each commit no remote-tracking ref holds, past git settings that hide lines", () => {
  const { dir: repo, git } = gitRepo("repo", { hostile: true });
  const commit = (content) => {
    fs.writeFileSync(path.join(repo, "f.txt"), content);
    git("add", "-A");
    git("commit", "-q", "-m", "c");
    return git("rev-parse", "HEAD");
  };
  const base = commit("base\n");
  const leak = commit(`${home}/secret\n`);
  const fix = commit("clean\n");
  const published = (env = {}) => run("", env, { args: ["--published", fix], cwd: repo });

  // The leak is in history even though the tip removed it.
  git("update-ref", "refs/remotes/origin/main", base);
  const r = published();
  hit(r);
  assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt:1\n`), r.stderr);
  // Already public: not this push's to refuse.
  git("update-ref", "refs/remotes/origin/main", leak);
  assert.equal(published().status, 0);
  // With nothing published yet, the first commit counts too.
  git("update-ref", "-d", "refs/remotes/origin/main");
  git("checkout", "-q", "--orphan", "fresh");
  const first = commit(`${home}/secret\n`);
  const fresh = run("", {}, { args: ["--published", first], cwd: repo });
  hit(fresh);
  assert.ok(fresh.stderr.includes(`${first.slice(0, 12)} f.txt:1\n`), fresh.stderr);
  // A sha git cannot read fails closed, with only the script's own line: git's error could
  // name a path.
  const unknown = "0".repeat(40);
  assert.deepEqual(run("", {}, { args: ["--published", unknown], cwd: repo }), {
    status: 1, stderr: `❌ [home-path] cannot list the commits in ${unknown} to check them (git log ${unknown} says why).\n`,
  });
  // A ref or path, whose text git's own error would print, gets the usage line, and so does
  // a short id, a missing one, an extra argument, or a sha without --published, which would
  // otherwise wait on stdin.
  const usage = { status: 1, stderr: "❌ [home-path] usage: home-path.sh [--published <sha>]\n" };
  for (const args of [["--published", `${home}/bad-ref`], ["--published", fix.slice(0, 12)], ["--published"], ["--published", fix, "x"], [fix]]) {
    assert.deepEqual(run("", {}, { args, cwd: repo }), usage, args.join(" "));
  }
});

test("--published checks what a merge resolution adds, and nothing a parent already has", () => {
  const { dir: repo, git } = gitRepo("merge", { hostile: true });
  fs.mkdirSync(path.join(repo, "b")); // a folder named like git's b/ prefix, to keep in the report
  const commit = (content) => {
    fs.writeFileSync(path.join(repo, "b", "f.txt"), content);
    git("add", "-A");
    git("commit", "-q", "-m", "c");
    return git("rev-parse", "HEAD");
  };
  commit("1\n2\n3\n4\n5\n");
  git("checkout", "-q", "-b", "side");
  // Published already, so not this push's to refuse, though the merge brings it in.
  git("update-ref", "refs/remotes/origin/main", commit(`1\n${home}/old\n3\nside\n5\n`));
  git("checkout", "-q", "main");
  commit("1\n2\n3\n4\nmain\n");
  sh(repo, "git", ["merge", "-q", "--no-commit", "side"]); // conflicts, which the resolution settles
  // Line 4 replaces a line from each parent, so the hunk's dropped lines come before it.
  fs.writeFileSync(path.join(repo, "b", "f.txt"), `1\n${home}/old\n3\n${home}/new\nmain\n`);
  git("add", "-A");
  git("commit", "-q", "-m", "merge");
  const merge = git("rev-parse", "HEAD");
  const r = run("", {}, { args: ["--published", merge], cwd: repo });
  hit(r);
  assert.equal(r.stderr.split("\n").filter((l) => l.startsWith("     ")).join("\n"), `     ${merge.slice(0, 12)} b/f.txt:4`);
});

// The wiring: each gate runs the check, and before its SKIP_* bypasses. The repos get copies of
// the scripts the gates call by repo-relative path.
const scriptsDir = path.dirname(path.dirname(script));
const copyScripts = (dir, files) => {
  for (const f of files) {
    fs.mkdirSync(path.join(dir, path.dirname(f)), { recursive: true });
    fs.copyFileSync(path.join(scriptsDir, "..", f), path.join(dir, f));
    fs.chmodSync(path.join(dir, f), 0o755);
  }
};
const SKIP = { SKIP_REVIEW_GATE: "1", SKIP_SLOW_GATES: "1" };

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

test("pre-push checks every pushed ref, not only the first", () => {
  const { dir, git } = gitRepo("two-refs");
  const commit = (content) => {
    fs.writeFileSync(path.join(dir, "f.txt"), content);
    git("add", "f.txt");
    git("commit", "-q", "-m", "c");
    return git("rev-parse", "HEAD");
  };
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
});

const hasJj = spawnSync("jj", ["--version"]).status === 0;
test("jj-gate.sh check and push (jj gate, jj gpush) refuse a leak", { skip: !hasJj && "jj is not installed" }, () => {
  const { dir, git } = gitRepo("jj");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  sh(dir, "git", ["init", "-q", "--bare", path.join(tmp, "jj.git")]);
  git("remote", "add", "origin", path.join(tmp, "jj.git"));
  git("push", "-q", "origin", "main");
  const config = path.join(tmp, "jj.toml");
  fs.writeFileSync(config, '[user]\nname = "t"\nemail = "t@example.com"\n[revset-aliases]\n"trunk()" = "main@origin"\n');
  const jj = (...a) => sh(dir, "jj", a, { JJ_CONFIG: config });
  const gate = (...a) => sh(dir, "bash", ["scripts/jj-gate.sh", ...a], { JJ_CONFIG: config, ...SKIP });
  assert.equal(jj("git", "init", "--colocate").status, 0);

  fs.writeFileSync(path.join(dir, "f.txt"), `${home}/secret\n`);
  const check = gate("check");
  assert.equal(check.status, 1, check.out);
  assert.match(check.out, /name your home directory/);
  fs.writeFileSync(path.join(dir, "f.txt"), "clean\n");
  assert.equal(gate("check").status, 0, "a clean stack");

  fs.writeFileSync(path.join(dir, "f.txt"), `${home}/secret\n`);
  assert.equal(jj("commit", "-m", "leak").status, 0);
  fs.writeFileSync(path.join(dir, "f.txt"), "clean\n");
  assert.equal(jj("commit", "-m", "fix").status, 0);
  assert.equal(jj("bookmark", "create", "topic", "-r", "@-").status, 0);
  const push = gate("push", "-b", "topic");
  assert.equal(push.status, 1, push.out);
  assert.match(push.out, /name your home directory/);
});
