import { after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("../hooks/home-path.sh", import.meta.url));
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
// Isolate child temporary files alongside the fixtures. An undefined override unsets a variable.
const envFor = (env) => {
  const e = { PATH: process.env.PATH, HOME: home, TMPDIR: tmp, TMP: tmp, TEMP: tmp, ...GIT_ENV, ...env };
  for (const k of Object.keys(e)) if (e[k] === undefined) delete e[k];
  return e;
};
const run = (diff, env = {}, { args = [], cwd = tmp } = {}) => {
  const r = spawnSync("bash", [script, ...args], { input: diff, cwd, encoding: "utf8", env: envFor(env) });
  assert.ifError(r.error);
  return { status: r.status, stderr: r.stderr };
};
// Runs a command in a directory; returns { status, out } with stdout and stderr together.
const sh = (cwd, cmd, args, env = {}) => {
  const r = spawnSync(cmd, args, { cwd, encoding: "utf8", env: envFor(env) });
  assert.ifError(r.error);
  return { status: r.status, out: `${r.stdout}${r.stderr}` };
};
// A fresh git repo; git(...) asserts success and returns stdout.
// hostile: git settings that would each hide an added line from a plain `git diff` or
// `git log -p`: a textconv driver that prints nothing, forced colour, an external diff that
// prints nothing (`git diff` only), no diff for the first commit (`git log` only), no a/ b/
// prefixes, which would report a file under a folder named b/ without it, and diff.relative,
// which would drop each file outside the folder git runs in.
// commit(content, file) writes file (f.txt by default), commits everything, returns the sha.
const gitRepo = (name, { hostile = false, objectFormat } = {}) => {
  const dir = path.join(tmp, name);
  fs.mkdirSync(dir);
  const git = (...a) => {
    const r = sh(dir, "git", a);
    assert.equal(r.status, 0, r.out);
    return r.out.trim();
  };
  git("init", "-q", "-b", "main", ...(objectFormat ? [`--object-format=${objectFormat}`] : []));
  if (hostile) {
    fs.writeFileSync(path.join(dir, ".gitattributes"), "*.txt diff=hide\n");
    git("config", "diff.hide.textconv", "sed d");
    git("config", "diff.external", "true");
    git("config", "color.ui", "always");
    git("config", "log.showRoot", "false");
    git("config", "diff.noprefix", "true");
    git("config", "diff.relative", "true");
  }
  const commit = (content, file = "f.txt") => {
    fs.writeFileSync(path.join(dir, file), content);
    git("add", "-A");
    git("commit", "-q", "-m", "c");
    return git("rev-parse", "HEAD");
  };
  return { dir, git, commit };
};
// A refusal for naming the home, not for some other failure.
const hit = (r, message) => {
  assert.equal(r.status, 1, message);
  assert.match(r.stderr, /name your home directory/, message);
  assert.doesNotMatch(r.stderr, /cannot scan the diff/, message);
};
const diff = (file, ...lines) => `diff --git a/${file} b/${file}\n--- a/${file}\n+++ b/${file}\n@@ -1 +1 @@\n${lines.join("\n")}\n`;
const publishedArgs = (tip, old = "0", remote = "origin") => ["--published", old, tip, remote];

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

const hasJj = spawnSync("jj", ["--version"]).status === 0;

export { script, tmp, home, envFor, run, sh, gitRepo, hit, diff, publishedArgs, copyScripts, SKIP, hasJj };
