// The code-reviewer persona exists once for Claude Code and once for Codex (AGENTS.md, "Agent
// personas"). Their bodies must not drift apart, and the limits each persona's text sets are
// its only guard, so they must stay.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";

const read = (f) => fs.readFileSync(new URL(`../${f}`, import.meta.url), "utf8");
const claude = read(".claude/agents/code-reviewer.md");
const codex = (name) => read(`.codex/agents/${name}.toml`);

// From the persona's heading up to its Composition section.
const shared = (text) => {
  const from = text.indexOf("# Senior Code Reviewer\n");
  const to = text.indexOf("\n## Composition\n");
  assert.ok(from >= 0 && to > from, "the heading and the Composition section are both there");
  return text.slice(from, to);
};

test("the code-reviewer persona is the same for Claude Code and Codex up to Composition", () => {
  assert.equal(shared(codex("code-reviewer")), shared(claude));
});

test("both code-reviewer descriptions say the review does not replace Standards + Spec", () => {
  const caveat = "it does not replace the code-review skill's Standards + Spec review that AGENTS.md requires for every commit";
  assert.ok(claude.match(/^description: .*$/m)[0].includes(caveat));
  assert.ok(codex("code-reviewer").match(/^description = ".*"$/m)[0].includes(caveat));
});

test("each persona keeps its limits", () => {
  const readOnly = "Read-only: do not edit files, commit, or push. Do not run cargo unless";
  assert.ok(shared(claude).includes(readOnly)); // and so the Codex copy, by the first test
  assert.ok(codex("security-auditor").includes(readOnly));
  assert.ok(codex("test-engineer").includes("Run targeted tests only, never `cargo test --workspace` (the parent task owns the one workspace run), and one cargo process at a time"));
});

// No TOML parser without npm, so the shape: comments, three string keys, and instructions in
// one ''' literal string, which a ''' pasted into the text would end early.
test("each Codex persona file keeps the shape Codex reads", () => {
  for (const name of ["code-reviewer", "security-auditor", "test-engineer"]) {
    const m = codex(name).match(/^(?:#.*\n)*name = "([^"\n]+)"\ndescription = "[^"\n]+"\ndeveloper_instructions = '''\n([\s\S]*)'''\n$/);
    assert.ok(m, name);
    assert.equal(m[1], name);
    assert.ok(!m[2].includes("'''"), `${name}: ''' inside the instructions`);
  }
});
