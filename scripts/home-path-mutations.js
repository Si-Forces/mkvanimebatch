// Manual guard audit. GNU timeout bounds deliberately damaged loops and their children.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scripts = path.dirname(fileURLToPath(import.meta.url));
const all = JSON.parse(fs.readFileSync(path.join(scripts, "test-support/home-path-mutations.json"), "utf8"));
const cases = all.filter(item => item.id.includes(process.argv[2] ?? ""));
assert.ok(cases.length, "No matching mutation cases");
const sources = new Map(cases.map(item => [item.file, fs.readFileSync(path.join(scripts, item.file), "utf8")]));
const output = fs.mkdtempSync(path.join(os.tmpdir(), "home-path-mutations-"));
process.stdout.write(`Results: ${output}\n`);
for (const item of cases) {
  assert.equal(sources.get(item.file).split(item.from).length, 2, `${item.id}: replacement must match exactly once`);
  assert.ok(item.expected === "caught" || (item.expected === "survived" && item.reason), item.id);
  const titles = item.tests.flatMap(file => [...fs.readFileSync(path.join(scripts, file), "utf8")
    .matchAll(/^test\(("(?:[^"\\]|\\.)*")/gm)].map(match => JSON.parse(match[1])));
  assert.ok(titles.some(title => new RegExp(item.pattern).test(title)), `${item.id}: no test title matches`);
  const preflight = path.join(output, `${item.id}.sh`);
  fs.writeFileSync(preflight, sources.get(item.file).replace(item.from, () => item.to));
  const syntax = spawnSync("bash", ["-n", preflight], { encoding: "utf8" });
  assert.ifError(syntax.error);
  assert.equal(syntax.status, 0, `${item.id}: invalid syntax is not a useful mutant: ${syntax.stderr}`);
}
const execute = (args, cwd, seconds) => {
  const r = spawnSync("timeout", ["-k", "2s", `${seconds}s`, process.execPath, "--test", "--test-reporter=tap", ...args], {
    cwd, encoding: "utf8", maxBuffer: 16 * 1024 * 1024,
  });
  assert.ifError(r.error);
  return r;
};
const files = [...new Set(cases.flatMap(item => item.tests))];
const baseline = execute(files.map(file => path.join(scripts, file)), path.dirname(scripts), 90);
fs.writeFileSync(path.join(output, "baseline.log"), baseline.stdout + baseline.stderr);
assert.equal(baseline.status, 0, `Baseline failed: ${output}/baseline.log`);
const results = [];
for (const item of cases) {
  const dir = path.join(output, item.id);
  fs.mkdirSync(dir);
  fs.cpSync(scripts, path.join(dir, "scripts"), { recursive: true });
  const target = path.join(dir, "scripts", item.file);
  fs.writeFileSync(target, sources.get(item.file).replace(item.from, () => item.to));
  const syntax = spawnSync("bash", ["-n", target], { encoding: "utf8" });
  assert.ifError(syntax.error);
  assert.equal(syntax.status, 0, `${item.id}: invalid syntax is not a useful mutant: ${syntax.stderr}`);
  const run = execute([`--test-name-pattern=${item.pattern}`, ...item.tests.map(file => path.join(dir, "scripts", file))], dir, 20);
  fs.writeFileSync(path.join(output, `${item.id}.log`), run.stdout + run.stderr);
  const timeout = run.status === 124 || run.status === 137;
  const caught = timeout || (run.status === 1 && /# fail [1-9]/.test(run.stdout));
  const activeTests = Number(run.stdout.match(/# tests (\d+)/)?.[1]) - Number(run.stdout.match(/# skipped (\d+)/)?.[1] ?? 0);
  assert.ok(timeout || activeTests > 0, `${item.id}: no active test matched`);
  assert.ok(caught || run.status === 0, `${item.id}: unexpected test-runner exit ${run.status}`);
  const status = caught ? (timeout ? "caught-timeout" : "caught") : "survived";
  results.push({ id: item.id, file: item.file, pattern: item.pattern, status, expected: item.expected, reason: item.reason });
  fs.writeFileSync(path.join(output, "results.json"), JSON.stringify({
    sources: Object.fromEntries([...sources].map(([file, text]) => [file, createHash("sha256").update(text).digest("hex")])),
    results,
  }, null, 2) + "\n");
  process.stdout.write(`${item.id}: ${status}${item.reason ? " — " + item.reason : ""}\n`);
}
const unexpected = results.filter(item => (item.status === "survived") !== (item.expected === "survived"));
process.stdout.write(JSON.stringify({
  total: results.length, assertions: results.filter(item => item.status === "caught").length,
  timeouts: results.filter(item => item.status === "caught-timeout").length,
  justifiedSurvivors: results.filter(item => item.status === "survived" && item.expected === "survived").length,
  unexpected: unexpected.map(item => item.id),
}) + "\n");
assert.equal(unexpected.length, 0, `Unresolved mutation outcomes: ${unexpected.map(item => item.id).join(", ")}`);
