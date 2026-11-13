import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { copyScripts, gitRepo, hasJj, sh, tmp } from "./test-support/home-path.js";

const executables = [
  "scripts/hooks/commit-msg", "scripts/hooks/pre-commit", "scripts/hooks/pre-push",
  "scripts/hooks/home-path.sh", "scripts/review-gate.sh",
];
const helper = "scripts/hooks/hidden-changes.sh";
const fixture = name => {
  const repo = gitRepo(name);
  copyScripts(repo.dir, ["scripts/install-git-hooks.sh", ...executables, helper]);
  return repo;
};
const install = dir => sh(dir, "bash", ["scripts/install-git-hooks.sh"]);

test("installer requires every invoked hook and helper before enabling protection", () => {
  for (const [i, file] of executables.entries()) {
    for (const failure of ["missing", "not-executable"]) {
      const { dir } = fixture(`install-${i}-${failure}`);
      if (failure === "missing") fs.unlinkSync(path.join(dir, file));
      else fs.chmodSync(path.join(dir, file), 0o644);
      const r = install(dir);
      assert.equal(r.status, 1, r.out);
      assert.ok(r.out.includes(`${file} is missing or not executable`), r.out);
      assert.equal(sh(dir, "git", ["config", "--get", "core.hooksPath"]).status, 1);
    }
  }
  const { dir } = fixture("install-missing-sourced-helper");
  fs.unlinkSync(path.join(dir, helper));
  const r = install(dir);
  assert.equal(r.status, 1, r.out);
  assert.ok(r.out.includes(`${helper} is missing`), r.out);
});

test("installer accepts its relative and absolute configuration and reports only real legacy hooks", () => {
  for (const setting of ["unset", "relative", "absolute"]) {
    const { dir, git } = fixture(`install-${setting}`);
    if (setting !== "unset") git("config", "core.hooksPath", setting === "relative" ? "scripts/hooks" : path.join(dir, "scripts/hooks"));
    fs.chmodSync(path.join(dir, helper), 0o644); // sourced helpers need not be executable
    fs.writeFileSync(path.join(dir, ".git/hooks/old-hook"), "legacy\n");
    fs.mkdirSync(path.join(dir, ".git/hooks/directory"));
    const r = install(dir);
    assert.equal(r.status, 0, r.out);
    assert.equal(git("config", "core.hooksPath"), "scripts/hooks");
    assert.match(r.out, /no longer run: old-hook\n/);
    assert.doesNotMatch(r.out, /\.sample|no longer run:.*directory/);
  }
});

test("installer preserves another hooks configuration", () => {
  const { dir, git } = fixture("install-custom");
  git("config", "core.hooksPath", "other-hooks");
  const r = install(dir);
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /refusing to replace/);
  assert.equal(git("config", "core.hooksPath"), "other-hooks");
});

test("jj installer checks every helper before writing aliases", { skip: !hasJj && "jj is not installed" }, () => {
  const config = path.join(tmp, "installer-jj.toml");
  fs.writeFileSync(config, '[user]\nname="t"\nemail="t@example.com"\n');
  const required = ["scripts/jj-gate.sh", "scripts/hooks/commit-msg", "scripts/hooks/home-path.sh", "scripts/review-gate.sh"];
  for (const [i, file] of [...required, null].entries()) {
    for (const failure of file ? ["missing", "not-executable"] : ["success"]) {
      const { dir } = fixture(`jj-install-${i}-${failure}`);
      copyScripts(dir, ["scripts/jj-gate.sh", "scripts/install-jj-aliases.sh"]);
      const jj = (...args) => sh(dir, "jj", args, { JJ_CONFIG: config });
      const initialized = jj("git", "init", "--colocate");
      assert.equal(initialized.status, 0, initialized.out);
      if (failure === "missing") fs.unlinkSync(path.join(dir, file));
      if (failure === "not-executable") fs.chmodSync(path.join(dir, file), 0o644);
      const r = sh(dir, "bash", ["scripts/install-jj-aliases.sh"], { JJ_CONFIG: config });
      if (file) {
        assert.equal(r.status, 1, r.out);
        assert.ok(r.out.includes(`${file} is missing or not executable`), r.out);
        assert.equal(jj("config", "get", "aliases.gate").status, 1);
      } else {
        assert.equal(r.status, 0, r.out);
        for (const alias of ["gate", "gpush"]) assert.match(jj("config", "get", `aliases.${alias}`).out, /scripts\/jj-gate\.sh/);
        assert.equal(jj("config", "get", "working-copy.eol-conversion").out.trim(), "input");
      }
    }
  }
});
