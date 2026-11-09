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

// Only PATH, HOME and USERPROFILE reach the script.
const run = (diff, env = {}, { args = [], cwd = tmp } = {}) => {
  const r = spawnSync("bash", [script, ...args], {
    input: diff,
    cwd,
    encoding: "utf8",
    env: { PATH: process.env.PATH, HOME: home, ...env },
  });
  return { status: r.status, stderr: r.stderr };
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

test("--published checks each commit no remote-tracking ref holds", () => {
  const repo = path.join(tmp, "repo");
  fs.mkdirSync(repo);
  const git = (...a) => {
    const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...a], { cwd: repo, encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
    return r.stdout.trim();
  };
  const commit = (content) => {
    fs.writeFileSync(path.join(repo, "f.txt"), content);
    git("add", "f.txt");
    git("commit", "-q", "-m", "c");
    return git("rev-parse", "HEAD");
  };
  git("init", "-q");
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
