import test, { after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./release-body.sh", import.meta.url));
const realNotes = fileURLToPath(new URL("../docs/release-notes.md", import.meta.url));

const run = (args, env = {}) =>
  spawnSync("bash", [script, ...args], { encoding: "utf8", env: { ...process.env, ...env } });

// Every fixture lives under one temp dir, removed when the suite ends.
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "release-body-"));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

// Writes fixture notes to a fresh dir and returns the file path.
const fixture = text => {
  const dir = fs.mkdtempSync(path.join(tmp, "notes-"));
  const file = path.join(dir, "release-notes.md");
  fs.writeFileSync(file, text);
  return file;
};

const NOTES = [
  "# Release Notes",
  "",
  "## 2.0.0",
  "",
  "Intro text.",
  "",
  "### Downloads ",
  "",
  "| a | b |",
  "",
  "### Features",
  "",
  "- one",
  "",
  "### Known issues",
  "",
  "- two",
  "",
  "## 1.0.0+ci.1",
  "",
  "### Features",
  "",
  "- plus build",
  "",
  "## 1.0.0",
  "",
  "### Downloads",
  "",
  "Only downloads here.",
  "",
].join("\n");

test("notes drops the intro and Downloads, even with a trailing space, and promotes ###", () => {
  const r = run(["notes", "2.0.0", fixture(NOTES)]);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout, "## Features\n\n- one\n\n## Known issues\n\n- two\n");
});

test("notes reports a missing section", () => {
  const r = run(["notes", "9.9.9", fixture(NOTES)]);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /::error::.*has no '## 9\.9\.9' section/);
});

test("notes reports a section with nothing but Downloads as empty, not missing", () => {
  const r = run(["notes", "1.0.0", fixture(NOTES)]);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /::error::'## 1\.0\.0' in .* has no notes/);
  assert.doesNotMatch(r.stderr, /has no '## .*' section/);
});

test("notes matches the version literally, so + and . are not patterns", () => {
  const file = fixture(NOTES);
  const plus = run(["notes", "1.0.0+ci.1", file]);
  assert.equal(plus.status, 0, plus.stderr);
  assert.equal(plus.stdout, "## Features\n\n- plus build\n");
  // "1.0.0" must not pick up the "1.0.0+ci.1" section, and "1x0.0" must not match "1.0.0".
  assert.match(run(["notes", "1.0.0", file]).stderr, /has no notes/);
  assert.match(run(["notes", "1x0x0", file]).stderr, /has no '## 1x0x0' section/);
});

// The release fails on a tag whose version has no notes, so a bump without them fails here first.
test("the real release notes have a usable section for the workspace version", () => {
  const cargo = fs.readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8");
  const version = cargo.match(/^version = "([^"]+)"/m)?.[1];
  assert.ok(version, "no workspace version in Cargo.toml");
  const r = run(["notes", version, realNotes]);
  assert.equal(r.status, 0, r.stderr);
});

const runtimeEnv = { WEBKIT: "2.52.6-w", JSC: "2.52.6-j", GTK: "3.24.41-g" };
const bundles = [
  "MKV_Anime_Batch_2.0.0_amd64.AppImage",
  "MKV_Anime_Batch_2.0.0_amd64.deb",
  "MKV_Anime_Batch-2.0.0-1.x86_64.rpm",
];
const bundleDir = names => {
  const dir = fs.mkdtempSync(path.join(tmp, "bundles-"));
  for (const n of names) fs.writeFileSync(path.join(dir, n), "");
  return dir;
};

test("page names the version's Linux bundles and the runtime versions", () => {
  const r = run(["page", "2.0.0", bundleDir(bundles), fixture(NOTES)], runtimeEnv);
  assert.equal(r.status, 0, r.stderr);
  for (const n of bundles) assert.ok(r.stdout.includes(`| \`${n}\` |`), n);
  assert.ok(r.stdout.includes("| `libwebkit2gtk-4.1.so.0` | `2.52.6-w` |"));
  assert.ok(r.stdout.includes("| `libjavascriptcoregtk-4.1.so.0` | `2.52.6-j` |"));
  assert.ok(r.stdout.includes("| `libgtk-3.so.0` | `3.24.41-g` |"));
  assert.ok(r.stdout.includes("\n## Features\n"));
});

test("page fails when a bundle is missing", () => {
  const r = run(["page", "2.0.0", bundleDir(bundles.slice(0, 2)), fixture(NOTES)], runtimeEnv);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /::error::expected bundle .*\.rpm is missing/);
});

// check runs against a fixture repo: a copy of this script under scripts/, a Cargo.toml at
// `version`, notes for it, and an origin that is the repo itself, so ls-remote sees its tags.
const git = (cwd, ...args) => {
  const r = spawnSync("git", args, {
    cwd,
    encoding: "utf8",
    env: {
      ...process.env,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_AUTHOR_NAME: "t",
      GIT_AUTHOR_EMAIL: "t@example.com",
      GIT_COMMITTER_NAME: "t",
      GIT_COMMITTER_EMAIL: "t@example.com",
    },
  });
  assert.equal(r.status, 0, r.stderr);
  return r.stdout.trim();
};
const repo = (version, notes = `## ${version}\n\n### Fixes\n\n- x\n`) => {
  const dir = fs.mkdtempSync(path.join(tmp, "repo-"));
  fs.mkdirSync(path.join(dir, "scripts"));
  fs.mkdirSync(path.join(dir, "docs"));
  fs.copyFileSync(script, path.join(dir, "scripts", "release-body.sh"));
  fs.writeFileSync(path.join(dir, "Cargo.toml"), `[workspace.package]\nversion = "${version}"\n`);
  fs.writeFileSync(path.join(dir, "docs", "release-notes.md"), notes);
  git(dir, "init", "-q", "-b", "main");
  git(dir, "remote", "add", "origin", dir);
  git(dir, "add", ".");
  git(dir, "commit", "-q", "-m", "a");
  const tagged = git(dir, "rev-parse", "HEAD");
  git(dir, "commit", "-q", "--allow-empty", "-m", "b");
  return { dir, tagged, head: git(dir, "rev-parse", "HEAD") };
};
const check = (dir, tag, sha) =>
  spawnSync("bash", [path.join(dir, "scripts", "release-body.sh"), "check", tag, sha], {
    encoding: "utf8",
  });

for (const kind of ["lightweight", "annotated"]) {
  test(`check passes only at the commit an existing ${kind} tag peels to`, () => {
    const { dir, tagged, head } = repo("2.0.0");
    git(dir, "tag", ...(kind === "annotated" ? ["-a", "-m", "v2.0.0"] : []), "v2.0.0", tagged);
    const ok = check(dir, "v2.0.0", tagged);
    assert.equal(ok.status, 0, ok.stderr);
    assert.equal(ok.stdout, "prerelease=false\n");
    const bad = check(dir, "v2.0.0", head);
    assert.notEqual(bad.status, 0);
    assert.match(bad.stderr, new RegExp(`::error::tag v2\\.0\\.0 points at ${tagged}, not at the built commit ${head}`));
  });
}

test("check passes when the tag does not exist yet", () => {
  const { dir, head } = repo("2.0.0");
  const r = check(dir, "v2.0.0", head);
  assert.equal(r.status, 0, r.stderr);
});

test("check derives prerelease from a '-' in the version, ignoring build metadata", () => {
  for (const [version, pre] of [["1.2.0", false], ["1.2.0-rc.1", true], ["1.2.0+build", false], ["1.2.0+build-7", false]]) {
    const { dir, head } = repo(version);
    const r = check(dir, `v${version}`, head);
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.stdout, `prerelease=${pre}\n`, version);
  }
});

test("check refuses a malformed tag before reading anything", () => {
  const { dir, head } = repo("2.0.0");
  for (const tag of ["2.0.0", "v2.0.0\n::warning::x", "v"]) {
    const r = check(dir, tag, head);
    assert.notEqual(r.status, 0);
    assert.equal(r.stderr, "::error::the release tag is not of the form v<version>\n");
  }
});

test("check refuses a tag that differs from the workspace version", () => {
  const { dir, head } = repo("2.0.0");
  fs.appendFileSync(path.join(dir, "docs", "release-notes.md"), "\n## 2.0.1\n\n### Fixes\n\n- y\n");
  const r = check(dir, "v2.0.1", head);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /::error::tag v2\.0\.1 does not match the workspace version 2\.0\.0/);
});

// The notes check is what stops a release with no page before either build starts.
for (const [what, notes, error] of [
  ["no section", "## 1.9.0\n\n### Fixes\n\n- x\n", /::error::docs\/release-notes\.md has no '## 2\.0\.0' section/],
  ["only Downloads", "## 2.0.0\n\nIntro.\n\n### Downloads\n\n| a |\n", /::error::'## 2\.0\.0' in docs\/release-notes\.md has no notes/],
]) {
  test(`check refuses a version whose notes have ${what}`, () => {
    const { dir, head } = repo("2.0.0", notes);
    const r = check(dir, "v2.0.0", head);
    assert.notEqual(r.status, 0);
    assert.match(r.stderr, error);
  });
}

test("check fails closed when origin cannot be listed", () => {
  const { dir, head } = repo("2.0.0");
  git(dir, "remote", "set-url", "origin", path.join(dir, "missing"));
  const r = check(dir, "v2.0.0", head);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /::error::could not list tag v2\.0\.0 on origin/);
});
