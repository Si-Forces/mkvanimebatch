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
  ];
  for (const patch of bad) {
    await assert.rejects(run({ ...ARGS, ...patch }, {}), /usual workflow:/, JSON.stringify(patch));
  }
});

test("accepts a numeric issue, a repo override and a fix round with prev", async () => {
  const { prompts, logs } = await run(
    { ...ARGS, issue: 178, mode: "fix", prev: "abc1234", repo: "/src/clone" },
    { fix: { ...DONE, status: "blocked" } },
  );
  assert.match(prompts.fix, /pr-178\.md/);
  assert.match(prompts.fix, /Repo: \/src\/clone,/);
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
  assert.match(prompts.implement, /mutants-diff\.sh 5a2a0ce -p mkv-core > "\/tmp\/scratch dir\/mutants-178\.log"/);
  assert.match(prompts["follow-up"], /mutants-diff\.sh <R> -p mkv-core/);
  assert.match(logs.at(-1), /record-range 5a2a0ce fix\/x/);
});

test("logs the review steps when the re-review finds nothing", async () => {
  const { prompts, logs } = await run(ARGS, { implement: DONE, "re-review": { findings: [] } });
  assert.equal(prompts["follow-up"], undefined);
  assert.match(logs.at(-1), /record-range/);
});
