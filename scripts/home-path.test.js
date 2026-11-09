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
// With textconv, a driver that shows nothing for *.txt, which must not hide a leak.
const gitRepo = (name, { textconv = false } = {}) => {
  const dir = path.join(tmp, name);
  fs.mkdirSync(dir);
  const git = (...a) => {
    const r = sh(dir, "git", a);
    assert.equal(r.status, 0, r.out);
    return r.out.trim();
  };
  git("init", "-q", "-b", "main");
  if (textconv) {
    fs.writeFileSync(path.join(dir, ".gitattributes"), "*.txt diff=hide\n");
    git("config", "diff.hide.textconv", "sed d");
  }
  return { dir, git };
};
const diff = (file, ...lines) => `diff --git a/${file} b/${file}\n--- a/${file}\n+++ b/${file}\n@@ -1 +1 @@\n${lines.join("\n")}\n`;

test("refuses an added line naming HOME, with the file and the line", () => {
  const r = run(diff("a.js", `+const REPO = '${home}/My Project/repo'`));
  assert.equal(r.status, 1);
  assert.match(r.stderr, /name your home directory/);
  assert.ok(r.stderr.includes(`a.js: const REPO = '${home}/My Project/repo'`), r.stderr);
  // HOME and its resolved form are the same path here: one line per hit, not one per form.
  assert.equal(r.stderr.split("a.js:").length - 1, 1, r.stderr);
});

test("allows generic home paths, removed lines and context", () => {
  assert.equal(run(diff("t.rs", '+assert_eq!(p("/home/user/show"), "/home/u/App.AppImage");')).status, 0);
  assert.equal(run(diff("a.js", `-const REPO = '${home}/repo'`, `+const REPO = A.repo`)).status, 0);
  assert.equal(run(diff("a.js", ` const REPO = '${home}/repo'`)).status, 0);
  assert.equal(run("").status, 0);
});

test("matches HOME only as whole path segments, in any case", () => {
  assert.equal(run(diff("a.rs", `+mod x; // ${home}z/lib.rs`)).status, 0, "a longer name");
  assert.equal(run(diff("a.rs", `+let p = "src${home}";`)).status, 1, "inside another path");
  assert.equal(run(diff("a.sh", `+gcc -I${home}/include`)).status, 1, "after a flag");
  assert.equal(run(diff("a.sh", `+cp /mnt/backup${home}/x .`)).status, 1, "under a mount point");
  // A one-segment home, like /root, must start a path.
  assert.equal(run(diff("a.rs", "+mod x; // src/no-such-root/lib.rs"), { HOME: "/no-such-root" }).status, 0, "one segment inside a path");
  assert.equal(run(diff("a.sh", "+cd /no-such-root/x"), { HOME: "/no-such-root" }).status, 1, "one segment starting a path");
  assert.equal(run(diff("a.md", `+[doc](file://${home}/doc.md)`)).status, 1, "after a file URL");
  assert.equal(run(diff("a.md", `+see ${home}`)).status, 1, "at the end of a line");
  assert.equal(run(diff("a.md", `+cloned into ${home}.`)).status, 1, "at the end of a sentence");
  assert.equal(run(diff("a.md", `+cloned into ${home}...`)).status, 1, "before an ellipsis");
  assert.equal(run(diff("a.md", `+cp ${home}.bak/x .`)).status, 0, "a sibling with a suffix");
  assert.equal(run(diff("a.md", `+see ${home.toUpperCase()}/x`)).status, 1, "in another case");
});

test("names the commit in git log output", () => {
  const sha = "0123456789abcdef0123456789abcdef01234567";
  const r = run(`commit ${sha}\n${diff(".codex/x.toml", `+see ${home}/Downloads/doc.md`)}`);
  assert.equal(r.status, 1);
  assert.ok(r.stderr.includes("0123456789ab .codex/x.toml: see "), r.stderr);
});

test("a +++ or --- line inside a hunk is content, not a file header", () => {
  const r = run(`${diff("a.sql", "-- old")}+++ ${home}/notes\n`);
  assert.equal(r.status, 1);
  assert.ok(r.stderr.includes(`a.sql: ++ ${home}/notes`), r.stderr);
});

test("shows a long line only around the match", () => {
  const r = run(diff("min.js", `+${"a".repeat(200)} ${home}/x ${"b".repeat(200)}`));
  assert.equal(r.status, 1);
  assert.match(r.stderr, new RegExp(`min\\.js: \\.\\.\\.a{39} ${home.replaceAll("/", "\\/")}/x b+\\.\\.\\.`));
});

test("checks USERPROFILE with backslashes and with forward slashes", () => {
  const env = { USERPROFILE: "C:\\Users\\tester" };
  assert.equal(run(diff("a.ps1", "+cd C:\\Users\\tester\\repo"), env).status, 1);
  assert.equal(run(diff("a.ps1", "+cd c:\\users\\tester\\repo"), env).status, 1);
  assert.equal(run(diff("a.js", "+'C:/Users/tester/repo'"), env).status, 1);
  const escaped = diff("a.toml", '+path = "C:\\\\Users\\\\tester\\\\repo"');
  assert.equal(run(escaped, env).status, 1, "escaped in a string");
  // bash before 4.3 keeps the quotes of a quoted replacement, as macOS's bash 3.2 does.
  assert.equal(run(escaped, { ...env, BASH_COMPAT: "42" }).status, 1, "escaped, on an old bash");
  assert.equal(run(diff("a.bat", "+cl -IC:\\Users\\tester\\inc"), env).status, 1, "after a flag");
  assert.equal(run(diff("a.js", "+'C:/Users/other/repo'"), env).status, 0);
});

test("ignores trailing separators on USERPROFILE and HOME", () => {
  const line = diff("a.ps1", "+cd C:\\Users\\tester\\repo");
  for (const p of ["C:\\Users\\tester\\", "C:/Users/tester/", "C:\\Users\\tester\\\\"]) {
    assert.equal(run(line, { USERPROFILE: p }).status, 1, p);
  }
  // A drive alone names nobody, as a HOME of / does not.
  assert.equal(run(diff("a.ps1", "+cd C:\\tools"), { USERPROFILE: "C:\\" }).status, 0);
  assert.equal(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester//" }).status, 1);
});

test("checks the \\/ a JSON writer may put for /", () => {
  const escaped = `${home}/x`.replaceAll("/", "\\/");
  assert.equal(run(diff("a.json", `+{"p":"${escaped}"}`)).status, 1);
  const env = { USERPROFILE: "C:\\Users\\tester" };
  assert.equal(run(diff("a.json", '+{"p":"C:\\/Users\\/tester\\/x"}'), env).status, 1, "USERPROFILE");
});

test("refuses a HOME it cannot resolve, and warns when no home is set", () => {
  const dangling = path.join(tmp, "dangling");
  fs.symlinkSync(path.join(tmp, "nowhere"), dangling);
  const loop = path.join(tmp, "loop");
  fs.symlinkSync(path.join(tmp, "loop2"), loop);
  fs.symlinkSync(loop, path.join(tmp, "loop2"));
  for (const h of [dangling, loop]) {
    const r = run(diff("a.txt", "+clean"), { HOME: h });
    assert.equal(r.status, 1, h);
    assert.match(r.stderr, /cannot resolve HOME.*symlink/);
  }
  const file = path.join(tmp, "a-file");
  fs.writeFileSync(file, "");
  assert.match(run(diff("a.txt", "+clean"), { HOME: file }).stderr, /not a folder/);
  if (process.getuid?.() !== 0) { // root enters any directory
    const locked = path.join(tmp, "locked");
    fs.mkdirSync(locked, { mode: 0o000 });
    try {
      assert.equal(run(diff("a.txt", "+clean"), { HOME: locked }).status, 1, "no permission");
      // Under a folder that cannot be searched, a missing home cannot be told from a hidden one.
      const r = run(diff("a.txt", "+clean"), { HOME: path.join(locked, "tester") });
      assert.equal(r.status, 1, "under no permission");
      assert.match(r.stderr, /cannot be searched/);
    } finally {
      fs.chmodSync(locked, 0o700); // so the cleanup can remove it
    }
  }
  const r = run(diff("a.txt", "+clean"), { HOME: undefined });
  assert.equal(r.status, 0);
  assert.match(r.stderr, /neither HOME nor USERPROFILE is set/);
});

test("checks the resolved form of a symlinked HOME", () => {
  const link = path.join(tmp, "link");
  fs.symlinkSync(home, link);
  assert.equal(run(diff("a.js", `+'${home}/repo'`), { HOME: link }).status, 1);
});

test("a HOME of / checks nothing, and a trailing slash on HOME is ignored", () => {
  assert.equal(run(diff("a.sh", "+cd / && ls"), { HOME: "/" }).status, 0);
  // Before bash 5.3, `cd ""` stays in the current directory; the script must not take that
  // for the home.
  const work = path.join(tmp, "work");
  fs.mkdirSync(work);
  assert.equal(run(diff("a.sh", `+ls ${work}/x`), { HOME: "/" }, { cwd: work }).status, 0);
  assert.equal(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester/" }).status, 1);
});

test("--published checks each commit no remote-tracking ref holds, past any textconv", () => {
  const { dir: repo, git } = gitRepo("repo", { textconv: true });
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
  assert.equal(r.status, 1);
  assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt: ${home}/secret`), r.stderr);
  // Already public: not this push's to refuse.
  git("update-ref", "refs/remotes/origin/main", leak);
  assert.equal(published().status, 0);
  // A sha git cannot read fails closed, and so does a missing one.
  assert.equal(run("", {}, { args: ["--published", "0".repeat(40)], cwd: repo }).status, 1);
  assert.match(run("", {}, { args: ["--published"], cwd: repo }).stderr, /usage/);
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
  const { dir, git } = gitRepo("hooked", { textconv: true });
  git("config", "core.hooksPath", path.dirname(script));
  copyScripts(dir, ["scripts/hooks/home-path.sh"]); // pre-push calls it from the repo root
  sh(dir, "git", ["init", "-q", "--bare", path.join(tmp, "hooked.git")]);
  git("remote", "add", "origin", path.join(tmp, "hooked.git"));
  const commit = (content, ...flags) => {
    fs.writeFileSync(path.join(dir, "f.txt"), content);
    git("add", "f.txt", ".gitattributes");
    return sh(dir, "git", ["commit", "-q", "-m", "c", ...flags]); // may fail, unlike git()
  };

  assert.equal(commit("clean\n").status, 0);
  const leak = commit(`${home}/secret\n`);
  assert.equal(leak.status, 1, leak.out);
  assert.match(leak.out, /name your home directory/);
  assert.equal(sh(dir, "git", ["push", "-q", "origin", "main"], SKIP).status, 0, "a clean push");

  // Added then removed is still published; the bypasses do not skip the check.
  assert.equal(commit(`${home}/secret\n`, "--no-verify").status, 0);
  assert.equal(commit("clean again\n").status, 0);
  const push = sh(dir, "git", ["push", "-q", "origin", "main"], SKIP);
  assert.notEqual(push.status, 0, push.out);
  assert.match(push.out, /name your home directory/);
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
