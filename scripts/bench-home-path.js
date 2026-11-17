// Manual, reproducible memory evidence for the home-path scanner; not a CI timing gate.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scanner = fileURLToPath(new URL("./hooks/home-path.sh", import.meta.url));
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "home-path-bench-"));
const home = path.join(tmp, "tester");
fs.mkdirSync(home);
const env = {
  PATH: process.env.PATH, HOME: home, LC_ALL: "C", GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1",
};
const command = (tool, args, options = {}) => {
  const r = spawnSync(tool, args, { cwd: tmp, env, encoding: "utf8", ...options });
  assert.ifError(r.error);
  assert.equal(r.status, 0, r.stderr);
  return r.stdout?.trim();
};
const measure = (mode, workload, sizeMiB, cwd, args, input, details) => {
  const metrics = path.join(tmp, "time.txt");
  const r = spawnSync("/usr/bin/time", ["-q", "-f", "%M\t%e\t%x", "-o", metrics, "bash", scanner, ...args], {
    cwd, env, stdio: [input, "ignore", "ignore"],
  });
  assert.ifError(r.error);
  assert.equal(r.status, workload === "hits" ? 1 : 0);
  const [rssKiB, seconds, exit] = fs.readFileSync(metrics, "utf8").trim().split("\t").map(Number);
  assert.ok(rssKiB > 0 && seconds >= 0 && exit === r.status);
  return { mode, workload, sizeMiB, ...details, rssKiB, seconds, exit };
};
const line = (workload, version = "") => (version + (workload === "hits" ? `${home}/x` : "clean")).padEnd(126, " ") + "\n";
const writeStream = (file, workload, sizeMiB) => {
  const count = sizeMiB * 1024 * 1024 / 128;
  const record = "+" + line(workload);
  assert.equal(Buffer.byteLength(record), 128);
  const fd = fs.openSync(file, "w");
  try {
    fs.writeSync(fd, `diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -0,0 +1,${count} @@\n`);
    const chunk = record.repeat(8192);
    for (let i = 0; i < count; i += 8192) fs.writeSync(fd, chunk);
  } finally { fs.closeSync(fd); }
  return { records: count, inputBytes: fs.statSync(file).size };
};
const history = (dir, workload, sizeMiB) => {
  fs.mkdirSync(dir);
  command("git", ["init", "-q", "-b", "main"], { cwd: dir });
  const file = path.join(tmp, "import.txt");
  const fd = fs.openSync(file, "w");
  const commits = sizeMiB * 1024 * 1024 / 65536;
  try {
    for (const [i, version] of ["a", "b"].entries()) {
      const blob = line(workload, version).repeat(256);
      fs.writeSync(fd, `blob\nmark :${i + 1}\ndata ${Buffer.byteLength(blob)}\n${blob}\n`);
    }
    for (let i = 0; i < commits; i++) {
      fs.writeSync(fd, `commit refs/heads/history\nmark :${i + 3}\ncommitter bench <bench@example.com> ${1700000000 + i} +0000\ndata 6\nbench\n`);
      if (i) fs.writeSync(fd, `from :${i + 2}\n`);
      fs.writeSync(fd, `M 100644 :${i % 2 + 1} f.txt\n\n`);
    }
  } finally { fs.closeSync(fd); }
  const input = fs.openSync(file, "r");
  try { command("git", ["fast-import", "--quiet"], { cwd: dir, stdio: [input, "ignore", "pipe"] }); }
  finally { fs.closeSync(input); fs.unlinkSync(file); }
  const tip = command("git", ["rev-parse", "refs/heads/history"], { cwd: dir });
  const patch = path.join(tmp, "history.diff");
  const output = fs.openSync(patch, "w");
  try {
    command("git", ["--no-replace-objects", "log", "-p", "--root", "-c", "-U0", "--text", "--no-color", "--no-relative",
      "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/", "--format=commit %H", tip, "--not"],
    { cwd: dir, stdio: ["ignore", output, "pipe"] });
  } finally { fs.closeSync(output); }
  const patchBytes = fs.statSync(patch).size;
  fs.unlinkSync(patch);
  return { tip, commits, patchBytes };
};

const binaryMerge = (dir, workload, sizeMiB) => {
  fs.mkdirSync(dir);
  const git = (...args) => command("git", args, { cwd: dir });
  git("init", "-q", "-b", "main"); git("config", "user.name", "bench"); git("config", "user.email", "bench@example.com");
  fs.writeFileSync(path.join(dir, ".gitattributes"), "f.txt -diff\n");
  const file = path.join(dir, "f.txt"), inherited = `${home}/inherited\n`;
  const chunk = "clean".padEnd(127, " ") + "\n";
  const count = sizeMiB * 1024 * 1024 / 128;
  const write = moved => {
    const fd = fs.openSync(file, "w");
    try {
      if (!moved) fs.writeSync(fd, inherited);
      for (let n = 0; n < count; n += 8192) fs.writeSync(fd, chunk.repeat(8192));
      if (moved) fs.writeSync(fd, inherited);
    } finally { fs.closeSync(fd); }
  };
  write(false); git("add", "."); git("commit", "-qm", "base"); const base = git("rev-parse", "HEAD");
  fs.appendFileSync(file, "left\n"); git("commit", "-qam", "left"); const left = git("rev-parse", "HEAD");
  git("checkout", "-q", "-B", "other", base);
  fs.appendFileSync(file, "right\n"); git("commit", "-qam", "right"); const right = git("rev-parse", "HEAD");
  write(true);
  const candidates = workload === "hits" ? 16 : 1024;
  for (let n = 0; n < candidates; n++) fs.appendFileSync(file,
    workload === "hits" ? `${home}/novel-${n}\n` : inherited);
  git("add", "f.txt");
  const tip = git("commit-tree", git("write-tree"), "-p", left, "-p", right, "-m", "resolution");
  return { base, tip, candidates, inputBytes: fs.statSync(file).size };
};

try {
  const environment = {
    os: fs.readFileSync("/etc/os-release", "utf8").split("\n").find(s => s.startsWith("PRETTY_NAME="))?.slice(12).replaceAll('"', ""),
    kernel: os.release(), arch: os.arch(), node: process.version,
    bash: command("bash", ["--version"]).split("\n")[0],
    awk: command("awk", ["--version"]).split("\n")[0],
    time: command("/usr/bin/time", ["--version"]).split("\n")[0],
    git: command("git", ["--version"]),
    scannerSha256: createHash("sha256").update(fs.readFileSync(scanner)).digest("hex"),
    adapterSha256: createHash("sha256").update(fs.readFileSync(new URL("./home-path-input.js", import.meta.url))).digest("hex"),
  };
  const samples = [];
  for (const workload of ["clean", "hits"]) {
    for (const sizeMiB of [10, 80]) {
      const file = path.join(tmp, "stdin.diff");
      const details = writeStream(file, workload, sizeMiB);
      const input = fs.openSync(file, "r");
      try { samples.push(measure("stdin", workload, sizeMiB, tmp, [], input, details)); }
      finally { fs.closeSync(input); fs.unlinkSync(file); }
      const dir = path.join(tmp, `${workload}-${sizeMiB}`);
      const { tip, ...historyDetails } = history(dir, workload, sizeMiB);
      samples.push(measure("published", workload, sizeMiB, dir, ["--published", "0", tip], "ignore", historyDetails));
      const binaryDir = path.join(tmp, `binary-${workload}-${sizeMiB}`);
      const { base, tip: binaryTip, ...binaryDetails } = binaryMerge(binaryDir, workload, sizeMiB);
      samples.push(measure("binary-merge", workload, sizeMiB, binaryDir,
        ["--published", base, binaryTip], "ignore", binaryDetails));
    }
  }
  process.stdout.write(JSON.stringify({ environment, samples }, null, 2) + "\n");
} finally { fs.rmSync(tmp, { recursive: true, force: true }); }
