// .claude/workflows/usual.js runs only inside the Claude Code Workflow runtime, which supplies
// `args`, `agent`, `phase` and `log` and allows a top-level `return`. Nothing else parses it, so
// this suite runs it with stubs: a syntax error or a broken argument guard fails here, not when
// a session first starts the workflow.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";

const source = fs
  .readFileSync(new URL("../.claude/workflows/usual.js", import.meta.url), "utf8")
  .replace(/^export const meta\b/m, "const meta");
const AsyncFunction = (async () => {}).constructor;
const workflow = new AsyncFunction("args", "agent", "phase", "log", source);

const ARGS = {
  mode: "implement",
  issue: "178",
  base: "5a2a0ce",
  bookmark: "fix/x",
  scratch: "/tmp/scratch dir",
  reading: "crates/mkv-core/src/lib.rs",
  tests: "cargo test -p mkv-core --lib",
  final_tests: "cargo test -p mkv-core",
  crates: ["mkv-core"],
  spec: "the design",
};

// Answers each agent by label; returns the prompts and log lines.
async function run(args, answers) {
  const prompts = {};
  const logs = [];
  const result = await workflow(
    args,
    async (prompt, opts) => {
      prompts[opts.label] = prompt;
      return answers[opts.label];
    },
    () => {},
    (line) => logs.push(line),
  );
  return { result, prompts, logs };
}

const DONE = {
  status: "done", skills_loaded: [], fixed: [], rejected: [], commits: [], tests: "",
  fails_without_fix: [], mutants: "", behaviour_changes: "", open_questions: [], summary: "",
};

test("rejects missing, blank and mistyped args", async () => {
  const bad = [
    { reading: undefined },
    { issue: -1 },
    { spec: 0 },
    { reading: "  " },
    { reading: [] },
    { tests: {} },
    { final_tests: null },
    { crates: [] },
    { crates: ["mkv-core", 3] },
    { crates: ["mkv-core; rm -rf ~"] },
    { skills: "slint:slint" },
    { skills: [""] },
    { mode: "fix" },
    { mode: "review" },
    { repo: "" },
    { notes: " " },
    { review_focus: 3 },
    { commit_hint: [] },
    { issue: "abc" },
    { issue: "178; rm -rf ~" },
    { base: "5a2a0ce; rm -rf ~" },
    { base: "-rf" },
    { bookmark: "fix/x $(id)" },
    { mode: "fix", prev: "abc 1234" },
    { mode: "fix", prev: "@-" },
    { base: "main@origin" },
    { base: "~" },
  ];
  for (const patch of bad) {
    await assert.rejects(run({ ...ARGS, ...patch }, {}), /usual workflow:/, JSON.stringify(patch));
  }
});

test("accepts a numeric issue, a repo override and a fix round with prev", async () => {
  for (const ref of ["HEAD~1", "HEAD^2", "origin/main"]) {
    await run({ ...ARGS, base: ref }, {});
  }
  const { prompts, logs } = await run(
    { ...ARGS, issue: 178, mode: "fix", prev: "abc1234", repo: "/src/clone" },
    { fix: { ...DONE, status: "blocked" } },
  );
  assert.match(prompts.fix, /pr-178\.md/);
  assert.match(prompts.fix, /Repo: \/src\/clone, colocated jj\./);
  // A blocked round may already have committed, so the owed review is still named.
  assert.match(logs.at(-1), /record-range 5a2a0ce fix\/x/);
});

test("runs every phase, quotes the log path and ends with the review steps", async () => {
  const { prompts, logs } = await run(ARGS, {
    implement: DONE,
    "re-review": { findings: [{ severity: "minor", file: "a.rs", summary: "s", evidence: "e", suggested_fix: "f" }] },
    "follow-up": { ...DONE, fixed: ["s"] },
    "last-commit review": { findings: [] },
  });
  assert.deepEqual(Object.keys(prompts), ["implement", "re-review", "follow-up", "last-commit review"]);
  assert.match(prompts.implement, /Repo: \/home\/sifaha\/My Project\/mkvanimebatch, main checkout,/);
  assert.match(prompts.implement, /mutants-diff\.sh 5a2a0ce -p mkv-core > "\/tmp\/scratch dir\/mutants-178\.log"/);
  assert.match(prompts["follow-up"], /mutants-diff\.sh <R> -p mkv-core/);
  assert.match(logs.at(-1), /record-range 5a2a0ce fix\/x/);
});

test("logs the review steps when the re-review finds nothing", async () => {
  const { prompts, logs } = await run(ARGS, { implement: DONE, "re-review": { findings: [] } });
  assert.equal(prompts["follow-up"], undefined);
  assert.match(logs.at(-1), /record-range/);
});

const FINDING = { severity: "minor", file: "a.rs", summary: "s", evidence: "e", suggested_fix: "f" };

test("a fix round scopes the mutation check and the re-review to prev", async () => {
  const { prompts } = await run(
    { ...ARGS, mode: "fix", prev: "abc1234" },
    { fix: DONE, "re-review": { findings: [] } },
  );
  assert.match(prompts.fix, /bookmark fix\/x is commit abc1234 on main 5a2a0ce/);
  assert.match(prompts.fix, /mutants-diff\.sh abc1234 -p mkv-core/);
  assert.match(prompts["re-review"], /`git diff abc1234 fix\/x`/);
  assert.match(prompts["re-review"], /whole change `git diff 5a2a0ce fix\/x`/);
});

test("stops with no follow-up when the re-review returns nothing", async () => {
  const { prompts, logs } = await run(ARGS, { implement: DONE });
  assert.equal(prompts["follow-up"], undefined);
  assert.ok(logs.some((l) => /Re-review failed or was skipped/.test(l)), logs.join("\n"));
  assert.match(logs.at(-1), /record-range 5a2a0ce fix\/x/);
});

test("the follow-up pins <R> to the bookmark before its first edit", async () => {
  const { prompts } = await run(ARGS, {
    implement: DONE,
    "re-review": { findings: [FINDING] },
    "follow-up": { ...DONE, status: "blocked" },
  });
  assert.match(prompts["follow-up"], /work is in bookmark fix\/x/);
  assert.match(prompts["follow-up"], /`git rev-parse fix\/x` before your first edit: that commit id is <R>/);
});

test("logs every agent's loaded skills", async () => {
  const { logs } = await run(ARGS, {
    implement: { ...DONE, skills_loaded: ["a:impl"] },
    "re-review": { skills_loaded: ["a:rr"], findings: [FINDING] },
    "follow-up": { ...DONE, skills_loaded: ["a:fu"], fixed: ["s"] },
    "last-commit review": { skills_loaded: [], findings: [] },
  });
  for (const line of [
    "Implement loaded skills: a:impl",
    "Re-review loaded skills: a:rr",
    "Follow-up loaded skills: a:fu",
    "Last-commit review loaded skills: none",
  ]) {
    assert.ok(logs.includes(line), `${line}\n---\n${logs.join("\n")}`);
  }
});

test("flags unresolved last-commit findings before the review steps", async () => {
  const { logs } = await run(ARGS, {
    implement: DONE,
    "re-review": { findings: [FINDING] },
    "follow-up": { ...DONE, fixed: ["s"] },
    "last-commit review": { findings: [FINDING, FINDING] },
  });
  assert.match(logs.at(-2), /Last-commit review: 2 UNRESOLVED findings/);
  assert.match(logs.at(-1), /record-range/);
});

test("a blocked implement says the bookmark may not exist; a blocked fix does not", async () => {
  const blocked = { ...DONE, status: "blocked", skills_loaded: ["a:impl"] };
  const impl = await run(ARGS, { implement: blocked });
  assert.ok(impl.logs.includes("Implement loaded skills: a:impl"), impl.logs.join("\n"));
  assert.match(impl.logs.at(-2), /Check whether bookmark fix\/x exists/);
  const fix = await run({ ...ARGS, mode: "fix", prev: "abc1234" }, { fix: blocked });
  assert.ok(!fix.logs.some((l) => /Check whether bookmark/.test(l)), fix.logs.join("\n"));
});

test("no last-commit review unless the follow-up is done and fixed something", async () => {
  for (const follow of [{ ...DONE, fixed: [] }, { ...DONE, status: "blocked", fixed: ["s"] }]) {
    const { prompts, logs } = await run(ARGS, {
      implement: DONE,
      "re-review": { findings: [FINDING] },
      "follow-up": follow,
    });
    assert.equal(prompts["last-commit review"], undefined, JSON.stringify(follow));
    assert.match(logs.at(-2), /no last-commit review/);
    assert.match(logs.at(-1), /record-range 5a2a0ce fix\/x/);
  }
});
