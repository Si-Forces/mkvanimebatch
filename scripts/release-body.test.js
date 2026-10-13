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
