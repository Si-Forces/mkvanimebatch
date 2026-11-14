// Exercise the audit command in tiny repositories, including timeout classification and cleanup.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { tmp, sh } from "./test-support/home-path.js";

const options = { skip: process.platform === "win32" && "manual audit fixtures use POSIX executable scripts" };

const fixture = (name, expected, {
  timeout = false, survives = false, brokenBaseline = false, javascript = false,
  skipped = false, runnerExit, manifest = {}, selector = "",
} = {}) => {
  const dir = path.join(tmp, `audit-${name}`);
  const scripts = path.join(dir, "scripts");
  const bin = path.join(dir, "bin");
  fs.mkdirSync(path.join(scripts, "test-support"), { recursive: true });
  fs.mkdirSync(bin);
  fs.copyFileSync(new URL("./home-path-mutations.js", import.meta.url), path.join(scripts, "home-path-mutations.js"));
  const from = javascript ? "process.exit(0);" : brokenBaseline ? "exit 1" : "exit 0";
  const file = javascript ? "guard.js" : "guard.sh";
  fs.writeFileSync(path.join(scripts, file), javascript ? from : `#!/usr/bin/env bash\n${from}\n`);
  fs.writeFileSync(path.join(scripts, "guard.test.js"), `import test from "node:test";\nimport assert from "node:assert/strict";\nimport { spawnSync } from "node:child_process";\nimport { fileURLToPath } from "node:url";\nconsole.log("CHILD_TEMP=" + JSON.stringify(["TMPDIR", "TMP", "TEMP"].map(k => process.env[k])));\ntest("guard bites", { skip: ${skipped} }, () => assert.equal(spawnSync(${javascript ? "process.execPath" : '"bash"'}, [fileURLToPath(new URL("./${file}", import.meta.url))]).status, 0));\n`);
  const item = { id: "fixture", file, from, to: javascript ? "process.exit(1);" : survives ? "# equivalent\nexit 0" : "exit 1", tests: ["guard.test.js"], pattern: "^guard bites$", expected, reason: expected === "survived" ? "Equivalent fixture" : undefined, ...manifest };
  fs.writeFileSync(path.join(scripts, "test-support/home-path-mutations.json"), JSON.stringify([item]));
  // Simulate runner failures without timing assertions or requiring GNU timeout in CI.
  fs.writeFileSync(path.join(bin, "timeout"), '#!/usr/bin/env bash\nif [[ "$*" == *--test-name-pattern=* ]]; then\n  if [[ "$AUDIT_REPORT_TIMEOUT" = 1 ]]; then exit 124; fi\n  if [[ -n "$AUDIT_RUNNER_EXIT" ]]; then printf "# tests 1\\n# skipped 0\\n"; exit "$AUDIT_RUNNER_EXIT"; fi\nfi\nshift 3\nexec "$@"\n');
  fs.chmodSync(path.join(bin, "timeout"), 0o755);
  const r = sh(dir, process.execPath, [path.join(scripts, "home-path-mutations.js"), selector], {
    PATH: bin + path.delimiter + process.env.PATH, AUDIT_REPORT_TIMEOUT: timeout ? "1" : "0", AUDIT_RUNNER_EXIT: runnerExit,
  });
  if (selector) return { r };
  const output = r.out.match(/^Results: (.+)$/m)?.[1];
  assert.ok(output, r.out);
  return { r, output, evidence: path.join(output, "results.json"), caseDir: path.join(output, "fixture") };
};

test("audit accepts exact assertion, timeout and survivor outcomes and removes completed copies", options, () => {
  for (const [name, expected, options] of [
    ["assertion", "caught", {}], ["javascript", "caught", { javascript: true }], ["timeout", "caught-timeout", { timeout: true }],
    ["survivor", "survived", { survives: true }],
  ]) {
    const f = fixture(name, expected, options);
    assert.equal(f.r.status, 0, f.r.out);
    assert.equal(JSON.parse(fs.readFileSync(f.evidence, "utf8")).results[0].status, expected);
    assert.equal(fs.existsSync(f.caseDir), false, "completed copy removed");
    assert.equal(fs.existsSync(path.join(f.output, "fixture.sh")), false, "syntax preflight removed");
    assert.equal(fs.existsSync(path.join(f.output, "fixture.mjs")), false, "JavaScript preflight removed");
    for (const file of ["baseline.log", "fixture.log"]) assert.ok(fs.existsSync(path.join(f.output, file)), file);
    const baselineTemp = path.join(f.output, "baseline-tmp");
    assert.equal(fs.existsSync(baselineTemp), false, "baseline temporary files removed");
    for (const [file, owner] of [["baseline.log", baselineTemp], ["fixture.log", f.caseDir]]) {
      if (file === "fixture.log" && expected === "caught-timeout") continue; // simulated timeout before Node starts
      const match = fs.readFileSync(path.join(f.output, file), "utf8").match(/CHILD_TEMP=(\[[^\n]+\])/);
      assert.ok(match, file);
      assert.deepEqual(JSON.parse(match[1]), [owner, owner, owner], file);
    }
  }
});

test("audit rejects unexpected timeouts and unexpected assertion failures and keeps their copies", options, () => {
  for (const [name, expected, options, actual] of [
    ["unexpected-timeout", "caught", { timeout: true }, "caught-timeout"],
    ["unexpected-assertion", "caught-timeout", {}, "caught"],
  ]) {
    const f = fixture(name, expected, options);
    assert.equal(f.r.status, 1, f.r.out);
    assert.match(f.r.out, /Unresolved mutation outcomes: fixture/);
    assert.equal(JSON.parse(fs.readFileSync(f.evidence, "utf8")).results[0].status, actual);
    assert.equal(fs.existsSync(f.caseDir), true, "unexpected copy retained for diagnosis");
    assert.equal(fs.existsSync(path.join(f.output, "fixture.sh")), false);
  }
});

test("audit refuses an unrecognized expectation or failing unmodified baseline", options, () => {
  const invalid = fixture("invalid-expectation", "unknown");
  assert.equal(invalid.r.status, 1, invalid.r.out);
  assert.equal(fs.existsSync(invalid.evidence), false);
  const baseline = fixture("broken-baseline", "caught", { brokenBaseline: true });
  assert.equal(baseline.r.status, 1, baseline.r.out);
  assert.match(baseline.r.out, /Baseline failed/);
  assert.equal(fs.existsSync(baseline.evidence), false);
});

test("audit refuses no active tests and unexpected runner exits", options, () => {
  for (const [name, opts, message] of [
    ["all-skipped", { skipped: true }, /fixture: no active test matched/],
    ["runner-exit", { runnerExit: "2" }, /fixture: unexpected test-runner exit 2/],
  ]) {
    const f = fixture(name, "survived", opts);
    assert.equal(f.r.status, 1, f.r.out);
    assert.match(f.r.out, message);
    assert.equal(fs.existsSync(f.evidence), false);
    assert.equal(fs.existsSync(f.caseDir), true, "invalid run retained for diagnosis");
  }
});

test("audit refuses a selector with no mutation cases", options, () => {
  const f = fixture("no-cases", "survived", { selector: "missing" });
  assert.equal(f.r.status, 1, f.r.out);
  assert.match(f.r.out, /No matching mutation cases/);
  assert.doesNotMatch(f.r.out, /^Results:/m);
});

test("audit refuses invalid mutation anchors, survivor reasons, test patterns and syntax", options, () => {
  for (const [name, opts, message] of [
    ["no-anchor", { manifest: { from: "missing" } }, /replacement must match exactly once/],
    ["multiple-anchors", { manifest: { from: "i" } }, /replacement must match exactly once/],
    ["no-reason", { survives: true, manifest: { reason: undefined } }, /AssertionError[^]*fixture/],
    ["no-test-title", { manifest: { pattern: "^missing$" } }, /no test title matches/],
    ["invalid-bash", { manifest: { to: "if" } }, /invalid syntax is not a useful mutant/],
    ["invalid-js", { javascript: true, manifest: { to: "const = ;" } }, /invalid syntax is not a useful mutant/],
  ]) {
    const f = fixture(name, "survived", opts);
    assert.equal(f.r.status, 1, f.r.out);
    assert.match(f.r.out, message);
    assert.equal(fs.existsSync(f.evidence), false);
    assert.equal(fs.existsSync(f.caseDir), false, "refused before making a case copy");
  }
});
