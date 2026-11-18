import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { script, home, tmp, sh, run, gitRepo, publishedArgs, copyScripts, SKIP, hasJj, envFor } from "./test-support/home-path.js";

test("text input checks every draft line without interpreting diff headers or scissors", () => {
  assert.equal(run("ordinary text\n", {}, { args: ["--text", "message"] }).status, 0);
  for (const text of [`# ${home}/private\n`, `diff --git\n${home}/private\n`,
    `# ------------------------ >8 ------------------------\n${home}/private\n`]) {
    const r = run(text, {}, { args: ["--text", "message"] });
    assert.equal(r.status, 1);
    assert.match(r.stderr, /message:[12]/);
    assert.ok(!r.stderr.includes(home), r.stderr);
  }
});

test("each scan warns once when neither environment variable names a home", () => {
  const { dir, commit } = gitRepo("single-identity-warning");
  const base = commit("clean\n");
  for (const args of [[], ["--text", "message"], ["--records"], ["--probe"], ["--staged"],
    publishedArgs(base), ["--between", base, base], ["--ref"]]) {
    const r = run(args[0] === "--ref" ? "refs/heads/clean" : "", { HOME: undefined, USERPROFILE: undefined },
      { cwd: dir, args });
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.stderr.match(/name no home directory/g)?.length, 1, args.join(" ") + r.stderr);
  }
});

test("published messages include intermediate and nested tag annotations over known commits", () => {
  const { dir, git, commit } = gitRepo("published-messages");
  const base = commit("clean\n");
  git("commit", "--allow-empty", "-qm", `message ${home}/private`, "--no-verify");
  const tip = git("rev-parse", "HEAD");
  const message = run("", {}, { cwd: dir, args: publishedArgs(tip, base) });
  assert.equal(message.status, 1);
  assert.match(message.stderr, /message:1/);
  assert.ok(!message.stderr.includes(home));
  git("tag", "-a", "inner", base, "-m", `tag ${home}/private`);
  git("tag", "-a", "outer", "inner", "-m", "clean outer");
  const tagged = run("", {}, { cwd: dir, args: publishedArgs(git("rev-parse", "outer"), base) });
  assert.equal(tagged.status, 1);
  assert.match(tagged.stderr, /tag-message:1/);
  assert.ok(!tagged.stderr.includes(home));
  const boundary = run("", {}, { cwd: dir, args: publishedArgs(git("rev-parse", "outer"), git("rev-parse", "inner")) });
  assert.equal(boundary.status, 0, boundary.stderr);
  git("tag", "-a", "new-outer", "inner", "-m", "another clean outer");
  assert.equal(run("", {}, { cwd: dir, args: publishedArgs(git("rev-parse", "new-outer"), git("rev-parse", "outer")) }).status, 0);
});

test("message and annotation reads ignore replacements and grafts and reject malformed records safely", () => {
  const { dir, git, commit } = gitRepo("metadata-originals");
  const base = commit("clean\n");
  git("commit", "--allow-empty", "-qm", `${home}/private`); const bad = git("rev-parse", "HEAD");
  git("commit", "--allow-empty", "-qm", "clean tip"); const tip = git("rev-parse", "HEAD");
  git("replace", bad, base);
  assert.match(run("", {}, { cwd: dir, args: publishedArgs(tip, base) }).stderr, /message:1/);
  git("replace", "-d", bad);
  fs.writeFileSync(path.join(dir, ".git/info/grafts"), `${tip} ${base}\n`);
  assert.match(run("", {}, { cwd: dir, args: publishedArgs(tip, base) }).stderr, /message:1/);
  fs.unlinkSync(path.join(dir, ".git/info/grafts"));
  git("tag", "-a", "bad", base, "-m", `${home}/private`);
  git("tag", "-a", "clean", base, "-m", "clean");
  git("replace", git("rev-parse", "bad"), git("rev-parse", "clean"));
  assert.match(run("", {}, { cwd: dir, args: publishedArgs(git("rev-parse", "bad"), base) }).stderr, /tag-message:1/);
  for (const body of [`${home}/not-a-record\n`, 'message 0 1 "\\000" ""\n', 'unknown 0 1 "" ""\n']) {
    const r = run(body, {}, { args: ["--records"] }); assert.equal(r.status, 1); assert.ok(!r.stderr.includes(home));
  }
});

test("filename input catches empty additions and pure renames outside a relative cwd", () => {
  const { dir, git, commit } = gitRepo("new-filenames", { hostile: true });
  const base = commit("clean\n");
  const name = path.join(home.slice(1), "empty").split(path.sep).join("/");
  fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
  fs.writeFileSync(path.join(dir, name), "");
  git("add", ".");
  fs.mkdirSync(path.join(dir, "sub"));
  const staged = run("", {}, { cwd: path.join(dir, "sub"), args: ["--staged"] });
  assert.equal(staged.status, 1);
  assert.match(staged.stderr, /:filename/);
  assert.ok(!staged.stderr.includes(home));
  git("commit", "-qm", "empty name", "--no-verify");
  const published = run("", {}, { cwd: path.join(dir, "sub"), args: publishedArgs(git("rev-parse", "HEAD"), base) });
  assert.equal(published.status, 1);
  assert.match(published.stderr, /:filename/);
  git("rm", name); git("commit", "-qm", "delete", "--no-verify");
  assert.equal(run("", {}, { cwd: dir, args: publishedArgs(git("rev-parse", "HEAD"), git("rev-parse", "HEAD~")) }).status, 0);
  fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
  git("mv", "f.txt", name);
  assert.match(run("", {}, { cwd: dir, args: ["--staged"] }).stderr, /:filename/);
});

test("ref input reports safe full destination names and refuses noncommit targets", () => {
  const dirty = `refs/heads/${home.slice(1)}/topic`;
  const refused = run(dirty, {}, { args: ["--ref"] });
  assert.equal(refused.status, 1);
  assert.match(refused.stderr, /:ref/);
  assert.ok(!refused.stderr.includes(home));
  assert.equal(run("refs/heads/clean", {}, { args: ["--ref"] }).status, 0);
  const { dir, git, commit } = gitRepo("noncommit-targets");
  commit("clean\n");
  for (const target of [git("rev-parse", "HEAD^{tree}"), git("rev-parse", "HEAD:f.txt")]) {
    assert.notEqual(run("", {}, { cwd: dir, args: publishedArgs(target) }).status, 0);
  }
});

test("binary merge checks accept moved parent lines and refuse new lines under hostile diff settings", () => {
  for (const kind of ["attribute", "nul"]) {
    const { dir, git, commit } = gitRepo(`binary-merge-${kind}`, { hostile: true });
    fs.writeFileSync(path.join(dir, ".gitattributes"), kind === "attribute" ? "*.txt -diff\n" : "*.txt diff=hide\n");
    const tail = Array.from({ length: 30 }, (_, n) => `tail-${n}`).join("\n");
    const inherited = `${home}/inherited`;
    const prefix = kind === "nul" ? "binary\0prefix" : "prefix";
    const before = `${prefix}\n${inherited}\n${tail}\n`;
    const base = commit(before);
    const a = commit("a\n" + before);
    git("checkout", "-q", "-B", "other", base);
    const b = commit("b\n" + before);
    const merge = text => {
      fs.writeFileSync(path.join(dir, "f.txt"), text); git("add", "f.txt");
      return git("commit-tree", git("write-tree"), "-p", a, "-p", b, "-m", "resolution");
    };
    const moved = merge(`a\nb\n${prefix}\n${tail}\n${inherited}\n`);
    assert.equal(run("", {}, { cwd: dir, args: publishedArgs(moved, base) }).status, 0);
    const novel = merge(`a\nb\n${prefix}\n${tail}\n${inherited}\n${home}/novel\n`);
    const refused = run("", {}, { cwd: dir, args: publishedArgs(novel, base) });
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /f\.txt"?:[0-9]+/);
    assert.ok(!refused.stderr.includes(home));
    assert.equal(run("", {}, { cwd: dir, args: ["--between", base, a] }).status, 0);
    assert.equal(run("", {}, { cwd: dir, args: ["--between", base, novel] }).status, 1);
  }
});

test("binary parent reads stop and reap after all distinct candidates match, and needed read failures refuse", {
  skip: process.platform === "win32" && "reader cleanup fixture uses POSIX signals",
}, () => {
  const { dir, git, commit } = gitRepo("early-parent-read");
  fs.writeFileSync(path.join(dir, ".gitattributes"), "*.txt -diff\n");
  const first = `${home}/first`, second = `${home}/second`;
  const tail = Array.from({ length: 30 }, (_, n) => `tail-${n}`).join("\n");
  const before = `${first}\n${second}\n${tail}\n`;
  const base = commit(before), left = commit("left\n" + before);
  const leftBlob = git("rev-parse", "HEAD:f.txt");
  git("checkout", "-q", "-B", "other", base);
  const right = commit("right\n" + before), rightBlob = git("rev-parse", "HEAD:f.txt");
  fs.writeFileSync(path.join(dir, "f.txt"), `left\nright\n${tail}\n${first}\n${first}\n${second}\n`);
  git("add", "f.txt");
  const merged = git("commit-tree", git("write-tree"), "-p", left, "-p", right, "-m", "moved lines");
  const realGit = sh(tmp, "bash", ["-c", "command -v git"]).out.trim();
  const bin = path.join(tmp, "early-parent-git"); fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "git"), [
    '#!/usr/bin/env node',
    'const fs = require("node:fs"), {spawnSync} = require("node:child_process");',
    'const args = process.argv.slice(2), env = process.env;',
    'if (args[1] !== "cat-file" || args[2] !== "blob") {',
    '  const r = spawnSync(env.REAL_GIT, args, {stdio:"inherit"}); process.exit(r.status ?? 1);',
    '}',
    'const blob = args[3]; fs.appendFileSync(env.PARENT_READS, blob + "\\n");',
    'process.on("SIGTERM", () => { fs.writeFileSync(env.PARENT_STOPPED, blob); process.exit(0); });',
    'if (blob === env.LEFT_BLOB) {',
    '  fs.writeSync(1, env.FIRST_LINE + "\\n");',
    '  if (env.SCENARIO === "failure") process.exit(67);',
    '  if (env.SCENARIO === "later") process.exit(0);',
    '  fs.writeSync(1, env.SECOND_LINE + "\\n");',
    '} else fs.writeSync(1, env.SECOND_LINE + "\\n");',
    // A successful scan must stop this reader without waiting for its tail or the deadline.
    'setTimeout(() => process.exit(68), 8000);',
  ].join("\n"));
  fs.chmodSync(path.join(bin, "git"), 0o755);
  for (const scenario of ["complete", "later", "failure"]) {
    const reads = path.join(tmp, `parent-reads-${scenario}`), stopped = path.join(tmp, `parent-stopped-${scenario}`);
    const r = run("", { PATH: bin + path.delimiter + process.env.PATH, REAL_GIT: realGit,
      LEFT_BLOB: leftBlob, FIRST_LINE: first, SECOND_LINE: second, SCENARIO: scenario,
      PARENT_READS: reads, PARENT_STOPPED: stopped }, { cwd: dir, args: publishedArgs(merged, base) });
    assert.equal(r.status, scenario === "failure" ? 1 : 0, r.stderr);
    assert.deepEqual(fs.readFileSync(reads, "utf8").trim().split("\n"),
      scenario === "later" ? [leftBlob, rightBlob] : [leftBlob]);
    if (scenario === "failure") {
      assert.match(r.stderr, /cannot inspect publication input safely/);
      assert.equal(fs.existsSync(stopped), false);
    } else assert.equal(fs.readFileSync(stopped, "utf8"), scenario === "later" ? rightBlob : leftBlob);
    assert.ok(!r.stderr.includes(home), r.stderr);
  }
});

test("commit hook scans the whole draft before any raw AI-trailer diagnostic", () => {
  const { dir, git, commit } = gitRepo("hook-message"); commit("clean\n");
  git("config", "core.hooksPath", path.dirname(script));
  for (const cleanup of ["strip", "whitespace", "verbatim", "scissors"]) {
    const body = path.join(dir, "body");
    fs.writeFileSync(body, `subject\n\n# ------------------------ >8 ------------------------\n# ${home}/private\n\nCo-authored-by: Claude <${home}/private> <noreply@anthropic.com>\n`);
    const r = sh(dir, "git", ["commit", "--allow-empty", "-F", body, `--cleanup=${cleanup}`]);
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /message:[0-9]+/);
    assert.ok(!r.out.includes(home), r.out);
    assert.doesNotMatch(r.out, /Co-authored-by:/);
  }
});

test("real editor and verbose drafts retain conservative whole-input checking", () => {
  const { dir, git, commit } = gitRepo("edited-drafts");
  commit(`${home}/old\nclean\n`); git("config", "core.hooksPath", path.dirname(script));
  fs.appendFileSync(path.join(dir, "f.txt"), "clean addition\n"); git("add", "f.txt");
  const editor = path.join(tmp, "draft-editor");
  fs.writeFileSync(editor, '#!/bin/sh\n[ "$ADD_COMMENT" != 1 ] || printf "\\n# %s/comment\\n" "$HOME" >> "$1"\n'); fs.chmodSync(editor, 0o755);
  const editorCommand = "'" + editor.replaceAll("\\", "/").replaceAll("'", "'\\''") + "'";
  for (const verbose of [false, true]) {
    const args = ["commit", "--edit", "-m", "clean"];
    if (verbose) args.push("--verbose");
    const r = sh(dir, "git", args, { GIT_EDITOR: editorCommand, ADD_COMMENT: verbose ? "0" : "1" });
    assert.equal(r.status, 1, r.out); assert.match(r.out, /message:[0-9]+/); assert.ok(!r.out.includes(home));
  }
});

const skipCases = [{}, { SKIP_REVIEW_GATE: "1" }, { SKIP_SLOW_GATES: "1" }, SKIP];
test("real Git push checks destination names and noncommit targets before each SKIP setting", () => {
  const { dir, git, commit } = gitRepo("git-ref-publication");
  const base = commit("clean\n");
  const remote = path.join(tmp, "git-ref-publication.git");
  git("init", "-q", "--bare", remote); git("remote", "add", "origin", remote); git("push", "-q", "origin", "main");
  git("config", "core.hooksPath", path.dirname(script));
  const bad = `refs/heads/${home.slice(1)}/topic`;
  const blob = git("rev-parse", "HEAD:f.txt"), tree = git("rev-parse", "HEAD^{tree}");
  git("tag", "-a", "blob-inner", blob, "-m", "inner"); git("tag", "-a", "blob-outer", "blob-inner", "-m", "outer");
  for (const env of skipCases) {
    const r = sh(dir, "git", ["push", "-q", "origin", `HEAD:${bad}`], env);
    assert.equal(r.status, 1, r.out); assert.match(r.out, /:ref/); assert.ok(!r.out.includes(home), r.out);
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", bad]).status, 0);
    for (const [target, name] of [[blob, "blob"], [tree, "tree"], ["blob-outer", "nested"]]) {
      const refused = sh(dir, "git", ["push", "-q", "origin", `${target}:refs/tags/${name}`], env);
      assert.equal(refused.status, 1, refused.out);
      assert.match(refused.out, /cannot inspect publication input safely/);
      assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", `refs/tags/${name}`]).status, 0);
    }
  }
  git("branch", home.slice(1) + "/local", base);
  assert.equal(sh(dir, "git", ["push", "-q", "origin", `${home.slice(1)}/local:refs/heads/clean`], SKIP).status, 0);
  git("push", "--no-verify", "-q", "origin", `HEAD:${bad}`);
  commit("clean update\n");
  for (const env of skipCases) {
    assert.equal(sh(dir, "git", ["push", "-q", "origin", `HEAD:${bad}`], env).status, 1);
    assert.equal(git("--git-dir", remote, "rev-parse", bad), base);
  }
  assert.equal(sh(dir, "git", ["push", "-q", "origin", `:${bad}`]).status, 0);
  git("tag", "-a", "private-inner", base, "-m", `${home}/annotation`);
  git("tag", "-a", "private-outer", "private-inner", "-m", "outer");
  for (const env of skipCases) {
    const r = sh(dir, "git", ["push", "-q", "origin", "private-outer:refs/tags/annotation"], env);
    assert.equal(r.status, 1, r.out); assert.match(r.out, /tag-message:1/); assert.ok(!r.out.includes(home));
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/tags/annotation"]).status, 0);
  }
});

test("real jj gate reads binary snapshots and gpush checks full destination names before SKIP", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-ref-publication");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean\n");
  const remote = path.join(tmp, "jj-ref-publication.git");
  git("init", "-q", "--bare", remote); git("remote", "add", "origin", remote); git("push", "-q", "origin", "main");
  const tip = commit("clean advance\n");
  const config = path.join(tmp, "jj-ref-publication.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config };
  const jj = (...args) => { const r = sh(dir, "jj", ["--color", "never", ...args], env); assert.equal(r.status, 0, r.out); return r.out.trim(); };
  jj("git", "init", "--colocate");
  const bad = home.slice(1) + "/topic";
  jj("bookmark", "create", bad, "-r", `commit_id(${tip})`);
  for (const skips of skipCases) {
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", bad], { ...env, ...skips });
    assert.equal(r.status, 1, r.out); assert.match(r.out, /:ref/); assert.ok(!r.out.includes(home), r.out);
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", `refs/heads/${bad}`]).status, 0);
  }
  jj("tag", "set", bad, "-r", `commit_id(${tip})`);
  for (const skips of skipCases) {
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--tag", bad], { ...env, ...skips });
    assert.equal(r.status, 1, r.out); assert.match(r.out, /:ref/); assert.ok(!r.out.includes(home));
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", `refs/tags/${bad}`]).status, 0);
  }
  git("push", "--no-verify", "-q", "origin", `${base}:refs/heads/${bad}`);
  jj("git", "fetch"); jj("bookmark", "track", bad, "--remote", "origin");
  for (const skips of skipCases) {
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", bad], { ...env, ...skips });
    assert.equal(r.status, 1, r.out); assert.match(r.out, /:ref/);
    assert.equal(git("--git-dir", remote, "rev-parse", `refs/heads/${bad}`), base);
  }
  fs.writeFileSync(path.join(dir, "binary.dat"), `prefix\0${home}/private\n`);
  const gate = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], env);
  assert.equal(gate.status, 1, gate.out); assert.match(gate.out, /binary\.dat/); assert.ok(!gate.out.includes(home), gate.out);
  jj("commit", "-m", "binary intermediate");
  fs.unlinkSync(path.join(dir, "binary.dat"));
  assert.equal(sh(dir, "bash", ["scripts/jj-gate.sh", "check"], env).status, 0);
  jj("describe", "-m", "removed binary"); jj("bookmark", "create", "history", "-r", "@");
  const history = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "history"], { ...env, ...SKIP });
  assert.equal(history.status, 1, history.out); assert.match(history.out, /binary\.dat/);
  jj("bookmark", "create", "clean", "-r", `commit_id(${tip})`);
  const publish = () => sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "clean"], { ...env, ...SKIP });
  assert.equal(publish().status, 0);
  jj("tag", "set", "clean-tag", "-r", `commit_id(${tip})`);
  const publishTag = () => sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--tag", "clean-tag"], { ...env, ...SKIP });
  assert.equal(publishTag().status, 0);
  jj("new", `commit_id(${tip})`); jj("describe", "-m", "clean update");
  fs.writeFileSync(path.join(dir, "f.txt"), "clean update\n");
  const updated = jj("log", "-r", "@", "--no-graph", "-T", "commit_id");
  jj("bookmark", "set", "clean", "-r", `commit_id(${updated})`);
  assert.equal(publish().status, 0);
  assert.equal(git("--git-dir", remote, "rev-parse", "refs/heads/clean"), updated);
  jj("tag", "set", "--allow-move", "clean-tag", "-r", `commit_id(${updated})`);
  assert.equal(publishTag().status, 0);
  assert.equal(git("--git-dir", remote, "rev-parse", "refs/tags/clean-tag"), updated);
  jj("bookmark", "delete", "clean");
  jj("tag", "delete", "clean-tag");
  assert.equal(sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--deleted"], env).status, 0);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/heads/clean"]).status, 0);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/tags/clean-tag"]).status, 0);
});

test("jj message checking reads original object bytes independently of description encoding", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-raw-message");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg"]);
  const base = commit("clean\n"); git("update-ref", "refs/remotes/origin/main", base);
  const profile = "/home/café";
  const raw = `tree ${git("rev-parse", "HEAD^{tree}")}\nparent ${base}\nauthor t <t@example.com> 1700000000 +0000\ncommitter t <t@example.com> 1700000000 +0000\nencoding ISO-8859-1\n\n${profile}/private\n`;
  const object = spawnSync("git", ["hash-object", "-t", "commit", "-w", "--stdin"], { cwd: dir, env: envFor({}), input: raw, encoding: "utf8" });
  assert.equal(object.status, 0, object.stderr); git("reset", "--hard", object.stdout.trim());
  const config = path.join(tmp, "jj-raw-message.toml"); fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config, USERPROFILE: profile };
  assert.equal(sh(dir, "jj", ["git", "init", "--colocate"], env).status, 0);
  const r = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], env);
  assert.equal(r.status, 1); assert.match(r.out, /message:1/); assert.ok(!r.out.includes(profile));
});

test("binary merge classification covers later parents, octopus merges, and hostile ordering", () => {
  const { dir, git, commit } = gitRepo("mixed-binary-merge");
  fs.writeFileSync(path.join(dir, "z.txt"), "base\n");
  const base = commit("base\n");
  fs.writeFileSync(path.join(dir, "z.txt"), "left\n"); const a = commit("left\n");
  git("checkout", "-q", "-B", "right", base);
  fs.writeFileSync(path.join(dir, "z.txt"), "right\0\n"); const b = commit("right\0\n");
  git("checkout", "-q", "-B", "third", base);
  fs.writeFileSync(path.join(dir, "z.txt"), "third\n"); const c = commit("third\n");
  fs.writeFileSync(path.join(dir, "order"), "z.txt\nf.txt\n"); git("config", "diff.orderFile", path.join(dir, "order"));
  for (const parents of [[a, b], [a, b, c]]) {
    fs.writeFileSync(path.join(dir, "f.txt"), `${home}/mixed\n`); fs.writeFileSync(path.join(dir, "z.txt"), "clean resolution\n"); git("add", "f.txt", "z.txt");
    const merge = git("commit-tree", git("write-tree"), ...parents.flatMap(p => ["-p", p]), "-m", "mixed");
    const r = run("", {}, { cwd: dir, args: publishedArgs(merge, base) });
    assert.equal(r.status, 1, r.stderr); assert.match(r.stderr, /f\.txt/); assert.ok(!r.stderr.includes(home));
  }
});

test("home-named gitlinks remain visible despite ignoreSubmodules and gitlink replacements are readable", () => {
  const { dir, git, commit } = gitRepo("gitlink-paths"); const base = commit("base\n");
  const name = home.slice(1) + "/module";
  git("config", "diff.ignoreSubmodules", "all");
  git("update-index", "--add", "--cacheinfo", `160000,${base},${name}`);
  assert.match(run("", {}, { cwd: dir, args: ["--staged"] }).stderr, /:filename/);
  const tip = git("commit-tree", git("write-tree"), "-p", base, "-m", "gitlink");
  assert.match(run("", {}, { cwd: dir, args: publishedArgs(tip, base) }).stderr, /:filename/);
  git("reset", "-q", "--hard", base);
  git("update-index", "--add", "--cacheinfo", `160000,${base},module`);
  const a = git("commit-tree", git("write-tree"), "-p", base, "-m", "module a");
  const b = git("commit-tree", git("write-tree"), "-p", base, "-m", "module b");
  git("update-index", "--force-remove", "module");
  fs.writeFileSync(path.join(dir, "module"), "clean\0binary\n"); git("add", "module");
  const merged = git("commit-tree", git("write-tree"), "-p", a, "-p", b, "-m", "regular file");
  assert.equal(run("", {}, { cwd: dir, args: publishedArgs(merged, a) }).status, 0);
});

test("Git and jj stop before diagnostics that could echo rejected source text or filenames", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("private-diagnostics");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg"]);
  const base = commit("clean\n"); git("update-ref", "refs/remotes/origin/main", base);
  git("config", "core.hooksPath", path.dirname(script));
  const name = "scripts/" + home.slice(1) + "/broken.js";
  fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true }); fs.writeFileSync(path.join(dir, name), `const = '${home}/private';\n`); git("add", name);
  const commitResult = sh(dir, "git", ["commit", "-qm", "private source"]);
  assert.equal(commitResult.status, 1); assert.match(commitResult.out, /name your home directory/); assert.ok(!commitResult.out.includes(home), commitResult.out);
  const config = path.join(tmp, "private-diagnostics.toml"); fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  assert.equal(sh(dir, "jj", ["git", "init", "--colocate"], { JJ_CONFIG: config }).status, 0);
  const checked = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], { JJ_CONFIG: config });
  assert.equal(checked.status, 1); assert.match(checked.out, /name your home directory/); assert.ok(!checked.out.includes(home), checked.out);
});

test("jj refuses unresolved binary conflicts before deriving a Git snapshot delta", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("binary-conflict");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg"]);
  const base = commit("base\0\n"); const a = commit("left\0\n");
  git("checkout", "-q", "-B", "right", base); const b = commit("right\0\n");
  git("update-ref", "refs/remotes/origin/main", base);
  const config = path.join(tmp, "binary-conflict.toml"); fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config };
  assert.equal(sh(dir, "jj", ["git", "init", "--colocate"], env).status, 0);
  assert.equal(sh(dir, "jj", ["new", `commit_id(${a})`, `commit_id(${b})`], env).status, 0);
  const r = sh(dir, "bash", ["scripts/jj-gate.sh", "check"], env);
  assert.equal(r.status, 1); assert.match(r.out, /unresolved conflicts/); assert.ok(!r.out.includes(home));
});

test("binary candidates flush at both line and byte bounds before their diff producer closes", {
  skip: process.platform === "win32" && "streaming fixture uses a POSIX Git wrapper",
}, () => {
  const realGit = sh(tmp, "bash", ["-c", "command -v git"]).out.trim();
  const bin = path.join(tmp, "streaming-git"); fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "git"), [
    '#!/usr/bin/env node',
    'const fs = require("node:fs"); const {spawnSync} = require("node:child_process");',
    'const args = process.argv.slice(2);',
    'if (args[1] === "cat-file" && args[2] === "blob") fs.writeFileSync(process.env.PARENT_SEEN, "yes");',
    'const r = spawnSync(process.env.REAL_GIT, args, {stdio:"inherit"});',
    'if (r.status !== 0) process.exit(r.status ?? 1);',
    'if (args[1] !== "diff") process.exit(0);',
    // Keep the diff open until a batch causes a parent-content lookup.
    'const deadline = Date.now() + 8000;',
    'function check(){ if(fs.existsSync(process.env.PARENT_SEEN)) process.exit(0);',
    'if(Date.now() > deadline) process.exit(67); setTimeout(check, 10); } check();',
  ].join("\n"));
  fs.chmodSync(path.join(bin, "git"), 0o755);
  for (const bound of ["lines", "bytes"]) {
    const { dir, git, commit } = gitRepo(`streaming-${bound}`);
    fs.writeFileSync(path.join(dir, ".gitattributes"), "*.txt -diff\n");
    const base = commit("base\n"); const left = commit("left\n");
    git("checkout", "-q", "-B", "other", base); const right = commit("right\n");
    const text = bound === "lines"
      ? Array.from({ length: 64 }, (_, n) => `${home}/new-${n}\n`).join("")
      : `${home}/new `.padEnd(65536, "x") + "\n";
    fs.writeFileSync(path.join(dir, "f.txt"), text); git("add", "f.txt");
    const merged = git("commit-tree", git("write-tree"), "-p", left, "-p", right, "-m", "resolution");
    const marker = path.join(tmp, `parent-seen-${bound}`);
    const r = run("", { PATH: bin + path.delimiter + process.env.PATH, REAL_GIT: realGit, PARENT_SEEN: marker },
      { cwd: dir, args: publishedArgs(merged, base) });
    assert.equal(r.status, 1, r.stderr); assert.match(r.stderr, /name your home directory/);
    assert.equal(fs.existsSync(marker), true, `${bound} bound must flush before diff EOF`);
    assert.ok(!r.stderr.includes(home));
  }
});

test("published signed history ignores signature display settings", () => {
  const { dir, git, commit } = gitRepo("signed-history");
  const base = commit("clean\n");
  const verifier = path.join(dir, "fake-gpg");
  fs.writeFileSync(verifier, '#!/bin/sh\ncat > /dev/null\necho "signature display fixture" >&2\nexit 1\n');
  fs.chmodSync(verifier, 0o755);
  git("config", "gpg.program", verifier);
  git("config", "log.showSignature", "true");
  // A stored signature header exercises display even when verification fails.
  const original = git("cat-file", "commit", base);
  const signed = original.replace("\n\n", "\ngpgsig -----BEGIN PGP SIGNATURE-----\n fixture\n -----END PGP SIGNATURE-----\n\n") + "\n";
  const object = spawnSync("git", ["hash-object", "-t", "commit", "-w", "--stdin"], {
    cwd: dir, input: signed, encoding: "utf8", env: envFor({}),
  });
  assert.ifError(object.error); assert.equal(object.status, 0, object.stderr);
  const tip = object.stdout.trim();
  assert.match(git("log", "-1", "--format=@%H %P", tip), /signature display fixture/);
  const r = run("", {}, { cwd: dir, args: publishedArgs(tip, base) });
  assert.equal(r.status, 0, r.stderr);
});

test("metadata adapter runs through a symlinked checkout directory", () => {
  const { dir, git, commit } = gitRepo("symlinked-checkout");
  commit("clean\n");
  copyScripts(dir, ["scripts/hooks/home-path.sh"]);
  const alias = path.join(tmp, "checkout-alias");
  fs.symlinkSync(dir, alias, "junction");
  const linkedScript = path.join(alias, "scripts/hooks/home-path.sh");
  const ref = spawnSync("bash", [linkedScript, "--ref"], {
    cwd: dir, input: `refs/heads/${home.slice(1)}/topic`, encoding: "utf8", env: envFor({}),
  });
  assert.ifError(ref.error); assert.equal(ref.status, 1, ref.stderr);
  assert.match(ref.stderr, /:ref/); assert.ok(!ref.stderr.includes(home));
  const file = path.join(dir, home.slice(1), "empty");
  fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, ""); git("add", ".");
  const staged = sh(dir, "bash", [linkedScript, "--staged"]);
  assert.equal(staged.status, 1, staged.out); assert.match(staged.out, /:filename/);
  assert.ok(!staged.out.includes(home));
});

test("metadata entrypoint resolution errors refuse without exposing invocation paths", () => {
  const source = `process.argv[1] = ${JSON.stringify(path.join(home, "missing-entry"))}; await import(${JSON.stringify(new URL("./home-path-input.js", import.meta.url).href)});`;
  const r = sh(tmp, process.execPath, ["--input-type=module", "-e", source]);
  assert.equal(r.status, 1, r.out); assert.match(r.out, /cannot inspect publication input safely/);
  assert.ok(!r.out.includes(home));
});

test("--staged, --published and --between allow 100% pure renames under diff.renames=false while refusing edited renames and home-bearing names", () => {
  const { dir, git, commit } = gitRepo("between-rename", { hostile: true });
  fs.appendFileSync(path.join(dir, ".gitattributes"), "*.bin -diff\n");
  git("config", "diff.renames", "false");
  const contextLines = Array.from({ length: 25 }, (_, i) => `clean-line-${i}`).join("\n");
  fs.writeFileSync(path.join(dir, "a.txt"), `${home}/existing\n${contextLines}\n`);
  fs.writeFileSync(path.join(dir, "a.bin"), `x\0y\n${home}/existing-bin\n`);
  const base = commit("seed\n");
  git("mv", "a.txt", "b.txt");
  git("mv", "a.bin", "b.bin");
  assert.deepEqual(run("", {}, { cwd: dir, args: ["--staged"] }), { status: 0, stderr: "" });
  git("commit", "-qm", "pure rename");
  const renamed = git("rev-parse", "HEAD");
  const startup = path.join(tmp, "between-graft-check.bash");
  fs.writeFileSync(startup, [
    'git() {',
    '  if [ "${1:-}" = --no-replace-objects ] && [ "${2:-}" = diff ]; then',
    '    [ "${GIT_GRAFT_FILE:-}" = /dev/null ] || return 1',
    '  fi',
    '  command git "$@"',
    '}',
    '',
  ].join("\n"));
  assert.deepEqual(run("", { BASH_ENV: startup }, { cwd: dir, args: ["--between", base, renamed] }), { status: 0, stderr: "" });
  assert.deepEqual(run("", {}, { cwd: dir, args: publishedArgs(renamed, base) }), { status: 0, stderr: "" });

  // An edited rename (~96% similar, keeping the home-bearing line) must refuse in every mode.
  git("mv", "b.txt", "c.txt");
  fs.appendFileSync(path.join(dir, "c.txt"), "edited\n");
  git("add", "c.txt");
  assert.equal(run("", {}, { cwd: dir, args: ["--staged"] }).status, 1);
  git("commit", "-qm", "edited rename");
  const editedRename = git("rev-parse", "HEAD");
  assert.equal(run("", {}, { cwd: dir, args: ["--between", renamed, editedRename] }).status, 1);
  assert.equal(run("", {}, { cwd: dir, args: publishedArgs(editedRename, renamed) }).status, 1);

  const dirtyName = path.join(home.slice(1), "renamed.txt");
  fs.mkdirSync(path.dirname(path.join(dir, dirtyName)), { recursive: true });
  git("mv", "c.txt", dirtyName);
  git("commit", "-qm", "rename to home path");
  const badName = git("rev-parse", "HEAD");
  const refused = run("", {}, { cwd: dir, args: ["--between", base, badName] });
  assert.equal(refused.status, 1, refused.stderr);
  assert.match(refused.stderr, /:filename/);
  assert.ok(!refused.stderr.includes(home), refused.stderr);
});

test("published commits refuse a mergetag header containing the home directory", () => {
  const { dir, git, commit } = gitRepo("mergetag-header");
  const base = commit("clean\n");
  const tree = git("rev-parse", "HEAD^{tree}");
  const raw = [
    `tree ${tree}`,
    `parent ${base}`,
    "author t <t@example.com> 1700000000 +0000",
    "committer t <t@example.com> 1700000000 +0000",
    `mergetag object ${base}`,
    " type commit",
    " tag signed-v1",
    " tagger t <t@example.com> 1700000000 +0000",
    " ",
    ` ${home}/from-mergetag`,
    " -----BEGIN PGP SIGNATURE-----",
    " fixture",
    " -----END PGP SIGNATURE-----",
    "",
    "clean merge body",
    "",
  ].join("\n");
  const object = spawnSync("git", ["hash-object", "-t", "commit", "-w", "--stdin"], {
    cwd: dir, input: raw, encoding: "utf8", env: envFor({}),
  });
  assert.ifError(object.error);
  assert.equal(object.status, 0, object.stderr);
  const tip = object.stdout.trim();
  const r = run("", {}, { cwd: dir, args: publishedArgs(tip, base) });
  assert.equal(r.status, 1, r.stderr);
  assert.match(r.stderr, new RegExp(`${tip.slice(0, 12)} header:10\\n`));
  assert.ok(!r.stderr.includes(home), r.stderr);
});

test("jj gpush decodes quoted non-ASCII destination names and fails closed on undecodable escapes", { skip: !hasJj }, () => {
  const cafeHome = path.join(tmp, "café");
  fs.mkdirSync(cafeHome, { recursive: true });
  const { dir, git, commit } = gitRepo("jj-quoted-names");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  commit("clean\n");
  const remote = path.join(tmp, "jj-quoted-names.git");
  git("init", "-q", "--bare", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const tip = commit("clean advance\n");
  const config = path.join(tmp, "jj-quoted-names.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config, HOME: cafeHome };
  const jj = (...args) => { const r = sh(dir, "jj", ["--color", "never", ...args], env); assert.equal(r.status, 0, r.out); return r.out.trim(); };
  jj("git", "init", "--colocate");
  const bad = cafeHome.slice(1) + "/topic";
  jj("bookmark", "create", `"${bad}"`, "-r", `commit_id(${tip})`);
  const bmRefused = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", `"${bad}"`], { ...env, ...SKIP });
  assert.equal(bmRefused.status, 1, bmRefused.out);
  assert.match(bmRefused.out, /:ref/);
  assert.ok(!bmRefused.out.includes(cafeHome), bmRefused.out);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", `refs/heads/${bad}`]).status, 0);

  jj("tag", "set", `"${bad}"`, "-r", `commit_id(${tip})`);
  const tagRefused = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--tag", `"${bad}"`], { ...env, ...SKIP });
  assert.equal(tagRefused.status, 1, tagRefused.out);
  assert.match(tagRefused.out, /:ref/);
  assert.ok(!tagRefused.out.includes(cafeHome), tagRefused.out);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", `refs/tags/${bad}`]).status, 0);

  const realJj = sh(dir, "bash", ["-c", "command -v jj"]).out.trim();
  const bin = path.join(tmp, "jj-escape-bin");
  fs.mkdirSync(bin);
  const marker = path.join(tmp, "jj-escape-pushed");
  fs.writeFileSync(path.join(bin, "jj"), [
    '#!/usr/bin/env bash',
    'case " $* " in',
    '  *" git push --dry-run "*) printf "%s\\n" "$PLAN"; exit 0 ;;',
    '  *" git push "*) touch "$PUSH_MARKER"; exit 99 ;;',
    'esac',
    'exec "$REAL_JJ" "$@"',
    '',
  ].join("\n"));
  fs.chmodSync(path.join(bin, "jj"), 0o755);
  for (const badQuoted of ['"bad\\u1234"', '"nul\\x00"', '"nul\\0"', '"mid"quote"', '"unterminated\\"']) {
    const plan = `Changes to push to origin:\n  bookmark: ${badQuoted} [add to ${tip.slice(0, 12)}]`;
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push"], {
      ...env, PATH: bin + path.delimiter + process.env.PATH, PLAN: plan, PUSH_MARKER: marker, REAL_JJ: realJj,
    });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /cannot decode destination name in the push plan safely/);
    assert.equal(fs.existsSync(marker), false);
  }
});

test("jj-gate decode_jj_name decodes quote and backslash escapes and fails closed on control or unknown escapes", () => {
  const gateScript = path.join(path.dirname(script), "..", "jj-gate.sh");
  const decode = (raw) => spawnSync("bash", [gateScript, "--decode-name", raw], { cwd: tmp, encoding: "utf8", env: envFor({}) });
  for (const [raw, expected] of [
    ["plain/topic", "plain/topic"],
    ['"tmp/x/café/topic"', "tmp/x/café/topic"],
    ['"x\\"y\\\\z"', 'x"y\\z'],
    ['"\\"quoted\\""', '"quoted"'],
  ]) {
    const r = decode(raw);
    assert.equal(r.status, 0, `${raw}: ${r.stderr}`);
    assert.equal(r.stdout, expected, raw);
  }
  for (const invalid of [
    "",
    '""',
    '"a\\tb"',
    '"a\\rb"',
    '"a\\nb"',
    '"a\\eb"',
    '"\\x41"',
    '"bad\\u1234"',
    '"bad\\z"',
    '"nul\\x00"',
    '"nul\\0"',
    '"mid"quote"',
    '"trailing\\"',
    '"unclosed\\',
    'unquoted"quote',
  ]) {
    const r = decode(invalid);
    assert.equal(r.status, 1, `${invalid} must fail closed`);
  }
});

test("jj gpush checks annotated tag objects, refuses git.sign-on-push, decodes quoted remotes, and pushes quoted bookmark names", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-tag-sign-remote");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, "jj-tag-sign-remote.git");
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "café", remote);
  git("push", "-q", "café", "main");
  const leak = commit(`${home}/public-on-cafe\n`);
  git("push", "--no-verify", "-q", "café", `${leak}:refs/heads/main`);
  git("fetch", "-q", "café");
  const tip = commit("clean advance\n");
  const config = path.join(tmp, "jj-tag-sign-remote.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@café"\n');
  const env = { JJ_CONFIG: config };
  const jj = (...args) => { const r = sh(dir, "jj", ["--color", "never", ...args], env); assert.equal(r.status, 0, r.out); return r.out.trim(); };
  jj("git", "init", "--colocate");

  // 1. Annotated tag with home path in tagger header must refuse and leave remote absent.
  sh(dir, "git", ["tag", "-a", "bad-tag", "-m", "clean tag message", tip], { GIT_COMMITTER_NAME: `${home}/tagger` });
  jj("log", "-r", "@", "--no-graph", "-T", "''");
  const badTag = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--remote", '"café"', "--tag", "bad-tag"], { ...env, ...SKIP });
  assert.equal(badTag.status, 1, badTag.out);
  assert.match(badTag.out, /header:[0-9]+/);
  assert.ok(!badTag.out.includes(home), badTag.out);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/tags/bad-tag"]).status, 0);

  // Clean annotated tag passes and reaches the remote.
  git("tag", "-a", "good-tag", "-m", "clean tag message", tip);
  jj("log", "-r", "@", "--no-graph", "-T", "''");
  const goodTag = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--remote", '"café"', "--tag", "good-tag"], { ...env, ...SKIP });
  assert.equal(goodTag.status, 0, goodTag.out);
  assert.equal(git("--git-dir", remote, "rev-parse", "refs/tags/good-tag"), git("rev-parse", "refs/tags/good-tag"));

  // 7 + 9. Quoted remote "café" supplies trusted baseline refs/remotes/café/main, and real bookmark x"y pushes cleanly.
  jj("bookmark", "create", '"x\\"y"', "-r", `commit_id(${tip})`);
  const quotePush = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--remote", '"café"', "-b", '"x\\"y"'], { ...env, ...SKIP });
  assert.equal(quotePush.status, 0, quotePush.out);
  assert.equal(git("--git-dir", remote, "rev-parse", 'refs/heads/x"y'), tip);

  // 2. git.sign-on-push=true refuses before pushing.
  jj("bookmark", "create", "unsigned", "-r", `commit_id(${tip})`);
  jj("config", "set", "--repo", "git.sign-on-push", "true");
  const signedRefused = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--remote", '"café"', "-b", "unsigned"], { ...env, ...SKIP });
  assert.equal(signedRefused.status, 1, signedRefused.out);
  assert.match(signedRefused.out, /git\.sign-on-push/);
  assert.match(signedRefused.out, /jj sign/);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/heads/unsigned"]).status, 0);
});

test("jj refuses to push when an annotated tag object is replaced while the gates run", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-tag-swapped");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, "jj-tag-swapped.git");
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const tip = commit("clean tip\n");
  git("tag", "-a", "swapped-tag", "-m", "clean tag message", tip);
  assert.equal(sh(dir, "bash", ["scripts/review-gate.sh", "record-range", base, tip, "fixture review"]).status, 0);
  const config = path.join(tmp, "jj-tag-swapped.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config };
  assert.equal(sh(dir, "jj", ["git", "init", "--colocate"], env).status, 0);

  const bin = path.join(tmp, "jj-tag-swapped-bin");
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "cargo"), '#!/usr/bin/env bash\ngit tag -fa swapped-tag -m "$HOME/swapped" "$TIP"\n');
  fs.writeFileSync(path.join(bin, "node"), '#!/usr/bin/env bash\nif [ "$1" = --test ]; then exit 0; fi\nexec "$REAL_NODE" "$@"\n');
  for (const file of ["cargo", "node"]) fs.chmodSync(path.join(bin, file), 0o755);

  const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--tag", "swapped-tag"], {
    ...env, PATH: bin + path.delimiter + process.env.PATH, REAL_NODE: process.execPath, TIP: tip,
  });
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /the push plan changed while the gates ran/);
  assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/tags/swapped-tag"]).status, 0);
});

test("jj refuses command-line --config and --config-file overrides on gpush", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-cli-config");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  commit("clean base\n");
  const remote = path.join(tmp, "jj-cli-config.git");
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const tip = commit("clean tip\n");
  const config = path.join(tmp, "jj-cli-config.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const overrideToml = path.join(tmp, "jj-cli-override.toml");
  fs.writeFileSync(overrideToml, "[git]\nsign-on-push = true\n");
  const env = { JJ_CONFIG: config };
  const jj = (...args) => { const r = sh(dir, "jj", ["--color", "never", ...args], env); assert.equal(r.status, 0, r.out); };
  jj("git", "init", "--colocate");
  jj("bookmark", "create", "topic", "-r", `commit_id(${tip})`);

  for (const flags of [
    ["--config", "git.sign-on-push=true"],
    ["--config=git.sign-on-push=true"],
    ["--config-file", overrideToml],
    [`--config-file=${overrideToml}`],
  ]) {
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", ...flags, "-b", "topic"], { ...env, ...SKIP });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /config overrides on the command line are not allowed/);
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/heads/topic"]).status, 0);
  }
  for (const flags of [
    ["-R", tmp],
    [`-R${tmp}`],
    ["--repository", tmp],
    [`--repository=${tmp}`],
    ["--at-operation", "@"],
    ["--at-operation=@"],
    ["--at-op", "@"],
    ["--at-op=@"],
  ]) {
    const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", ...flags, "-b", "topic"], { ...env, ...SKIP });
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /repository and operation overrides on the command line are not allowed/);
    assert.notEqual(sh(dir, "git", ["--git-dir", remote, "show-ref", "--verify", "refs/heads/topic"]).status, 0);
  }
});

test("jj refuses when a tag ref does not peel to the planned commit", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-tag-peel-mismatch");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, "jj-tag-peel-mismatch.git");
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const tip = commit("clean tip\n");
  git("tag", "-a", "mismatch-tag", "-m", "clean tag", base);
  const config = path.join(tmp, "jj-tag-peel-mismatch.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n');
  const env = { JJ_CONFIG: config };
  assert.equal(sh(dir, "jj", ["git", "init", "--colocate"], env).status, 0);

  const realJj = sh(dir, "bash", ["-c", "command -v jj"]).out.trim();
  const bin = path.join(tmp, "jj-tag-mismatch-bin");
  fs.mkdirSync(bin);
  const marker = path.join(tmp, "jj-tag-mismatch-pushed");
  fs.writeFileSync(path.join(bin, "jj"), [
    '#!/usr/bin/env bash',
    'case " $* " in',
    '  *" git push --dry-run "*) printf "%s\\n" "$PLAN"; exit 0 ;;',
    '  *" git push "*) touch "$PUSH_MARKER"; exit 99 ;;',
    'esac',
    'exec "$REAL_JJ" "$@"',
    '',
  ].join("\n"));
  fs.chmodSync(path.join(bin, "jj"), 0o755);
  const plan = `Changes to push to origin:\n  tag: mismatch-tag [add to ${tip.slice(0, 12)}]`;
  const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "--tag", "mismatch-tag"], {
    ...env, ...SKIP, PATH: bin + path.delimiter + process.env.PATH, PLAN: plan, PUSH_MARKER: marker, REAL_JJ: realJj,
  });
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /tag object does not match the planned commit/);
  assert.equal(fs.existsSync(marker), false);
});

test("jj pins git.sign-on-push=false for the push even when JJ_CONFIG enables it during the gates", { skip: !hasJj }, () => {
  const { dir, git, commit } = gitRepo("jj-pin-sign-off");
  copyScripts(dir, ["scripts/jj-gate.sh", "scripts/hooks/home-path.sh", "scripts/hooks/commit-msg", "scripts/review-gate.sh"]);
  const base = commit("clean base\n");
  const remote = path.join(tmp, "jj-pin-sign-off.git");
  git("init", "-q", "--bare", "-b", "main", remote);
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "main");
  const tip = commit("clean tip\n");
  assert.equal(sh(dir, "bash", ["scripts/review-gate.sh", "record-range", base, tip, "fixture review"]).status, 0);
  const config = path.join(tmp, "jj-pin-sign-off.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n[revset-aliases]\n"trunk()"="main@origin"\n[signing]\nbehavior = "keep"\nbackend = "ssh"\n');
  const env = { JJ_CONFIG: config };
  const jj = (...args) => { const r = sh(dir, "jj", ["--color", "never", ...args], env); assert.equal(r.status, 0, r.out); };
  jj("git", "init", "--colocate");
  jj("bookmark", "create", "topic", "-r", `commit_id(${tip})`);

  const bin = path.join(tmp, "jj-pin-sign-off-bin");
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "cargo"), '#!/usr/bin/env bash\nprintf "\\n[git]\\nsign-on-push = true\\n" >> "$JJ_CONFIG"\n');
  fs.writeFileSync(path.join(bin, "node"), '#!/usr/bin/env bash\nif [ "$1" = --test ]; then exit 0; fi\nexec "$REAL_NODE" "$@"\n');
  for (const file of ["cargo", "node"]) fs.chmodSync(path.join(bin, file), 0o755);

  const r = sh(dir, "bash", ["scripts/jj-gate.sh", "push", "-b", "topic"], {
    ...env, PATH: bin + path.delimiter + process.env.PATH, REAL_NODE: process.execPath,
  });
  assert.equal(r.status, 0, r.out);
  assert.equal(git("--git-dir", remote, "rev-parse", "refs/heads/topic"), tip);
  assert.doesNotMatch(git("--git-dir", remote, "cat-file", "commit", "refs/heads/topic"), /^gpgsig /m);
});
