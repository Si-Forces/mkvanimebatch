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

try {
  const environment = {
    os: fs.readFileSync("/etc/os-release", "utf8").split("\n").find(s => s.startsWith("PRETTY_NAME="))?.slice(12).replaceAll('"', ""),
    kernel: os.release(), arch: os.arch(), node: process.version,
    bash: command("bash", ["--version"]).split("\n")[0],
    awk: command("awk", ["--version"]).split("\n")[0],
    time: command("/usr/bin/time", ["--version"]).split("\n")[0],
    git: command("git", ["--version"]),
    scannerSha256: createHash("sha256").update(fs.readFileSync(scanner)).digest("hex"),
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
    }
  }
  process.stdout.write(JSON.stringify({ environment, samples }, null, 2) + "\n");
} finally { fs.rmSync(tmp, { recursive: true, force: true }); }
