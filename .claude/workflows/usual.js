export const meta = {
  name: 'usual',
  description: 'The usual issue loop: Opus high implements (or fixes a review round), Sonnet xhigh re-reviews read-only, Opus follow-up and Sonnet last-commit review only when needed; nothing is pushed',
  whenToUse: 'Implementing a settled issue design, or a review round on an unpushed or open PR, in mkvanimebatch; Rust changes only, since both modes require crates and the mutation check. Pass the issue-specific parts as args; see the ARGS comment at the top of the script. Its re-review is internal: the code-review skill and the review-gate record still follow by hand.',
  phases: [
    { title: 'Implement', detail: 'tests first, targeted tests, gate, commit, mutation check', model: 'opus' },
    { title: 'Re-review', detail: 'Standards + Spec, read-only, code-reviewer agent', model: 'sonnet' },
    { title: 'Follow-up', detail: 'only if the re-review reports findings', model: 'opus' },
    { title: 'Last-commit review', detail: 'code-reviewer agent, only if the follow-up fixed something', model: 'sonnet' },
  ],
}

// ARGS (pass a JSON object, not a string). Required unless marked optional; text args are
// non-blank strings (issue may also be a positive integer):
//   mode         'implement' (new commit on a new bookmark) or 'fix' (fold a review round into the bookmark)
//   issue        issue number, e.g. '178'
//   base         the main commit id the bookmark sits on, e.g. '5a2a0ce' (an id, not a ref)
//   bookmark     e.g. 'fix/episode-parser-markers'
//   prev         fix mode only: the bookmark's commit id before this round (the round's review scope)
//   scratch      this session's scratchpad directory (gate and mutants logs go there)
//   reading      the docs, code and tests to read first (paths and symbols)
//   tests        targeted test commands while iterating
//   final_tests  run once at the end, e.g. 'cargo test -p mkv-core'
//   crates       non-empty array of the crate names the diff touches, e.g. ['mkv-core']
//   spec         the settled design, or the verified findings to fix, with expected inputs/outputs
//   review_focus optional: what the reviewer should hand-trace beyond the spec
//   commit_hint  optional: conventional commit subject
//   skills       optional array of extra skills, e.g. ['slint:slint', 'anthropic-skills:domain-modeling']
//   repo         optional checkout the agents work in (default below: the maintainer's main checkout)
//   notes        optional PR-notes directory (default below); the notes file is pr-<issue>.md

const A = args || {}
const TEXT = ['mode', 'issue', 'base', 'bookmark', 'scratch', 'reading', 'tests', 'final_tests', 'spec']
const isText = (v) => typeof v === 'string' && v.trim() !== ''
const isName = (v) => typeof v === 'string' && /^[\w:-]+$/.test(v)
const missing = TEXT.filter((k) => !isText(A[k]) && !(k === 'issue' && Number.isInteger(A[k]) && A[k] > 0))
if (A.mode === 'fix' && !isText(A.prev)) missing.push('prev')
for (const k of ['repo', 'notes', 'review_focus', 'commit_hint']) if (A[k] !== undefined && !isText(A[k])) missing.push(k)
if (missing.length) throw new Error(`usual workflow: missing or blank args ${missing.join(', ')}`)
if (!['implement', 'fix'].includes(A.mode)) throw new Error(`usual workflow: mode must be implement or fix, got ${A.mode}`)
if (!Array.isArray(A.crates) || !A.crates.length || !A.crates.every(isName)) throw new Error('usual workflow: crates must be a non-empty array of crate names')
if (A.skills !== undefined && !(Array.isArray(A.skills) && A.skills.every(isName))) throw new Error('usual workflow: skills must be an array of skill names')
// These go unquoted into the shell commands the agents run, like crates and skills.
if (!/^[1-9]\d*$/.test(String(A.issue))) throw new Error(`usual workflow: issue must be an issue number, got ${A.issue}`)
// Commit ids, not refs: HEAD, a bookmark or origin/main can move during the run, which would
// empty the mutation and review scopes, and jj revsets cannot resolve git forms such as HEAD~1.
const isCommitId = (v) => /^[0-9a-f]{7,40}$/.test(v)
for (const k of ['base', ...(A.mode === 'fix' ? ['prev'] : [])]) if (!isCommitId(A[k])) throw new Error(`usual workflow: ${k} must be a commit id, got ${A[k]}`)
// Word runs joined by single . / or -: no @ (jj's name@remote), no trailing - (jj's parents
// operator), no .. (git refuses it), nothing the shell expands.
if (!/^\w+(?:[.\/-]\w+)*$/.test(A.bookmark)) throw new Error(`usual workflow: bookmark must be a bookmark name, got ${A.bookmark}`)

const REPO = A.repo || '~/My Project/mkvanimebatch'
const NOTES = A.notes || '~/My Project/mkvanimebatch/mkvanimebatch-notes/audit-fixes'
const NOTES_FILE = `${NOTES}/pr-${A.issue}.md`
const BM = A.bookmark
const FIX_MODE = A.mode === 'fix'
const SCOPE_FROM = FIX_MODE ? A.prev : A.base
const PKGS = A.crates.map((c) => `-p ${c}`).join(' ')
const SKILL_LIST = ['anthropic-skills:ponytail', 'anthropic-skills:rust-skills', ...(A.skills || [])]
const SKILLS = `Before your first edit, load these skills with the Skill tool and follow them: ${SKILL_LIST.map((s) => `\`${s}\``).join(', ')}. Report the ones you loaded in \`skills_loaded\`.`
// Single-quotes a path for the shell, so $, " and spaces in it stay literal.
const shQuote = (s) => `'${s.replaceAll("'", `'\\''`)}'`
const FOLD = `\`jj squash --into ${BM}\`, then update its message with \`jj describe -r ${BM} --stdin\` if it no longer matches (start from \`git log -1 --format=%B ${BM}\`)`

const ctx = (state) => `
Issue #${A.issue} (Si-Forces/mkvanimebatch; \`gh issue view ${A.issue}\`, read-only). Repo: ${REPO}${A.repo ? '' : ', main checkout'}, colocated jj.
${state}
jj: \`jj commit --stdin\` does not exist. A message written from a file must not contain jj graph characters such as │ or ○. Conventional commit message with a body that says why. Keep an empty @ on top of the bookmark; check with \`jj log -r '${A.base}:: | @'\`.
Read first: ${A.reading}
Machine (4 cores, 8 GB): while iterating run only ${A.tests}; run \`${A.final_tests}\` once at the end. Never \`cargo test --workspace\` or the bench; Slint and Tauri crates build only inside \`jj gate\`. \`jj gate\`: redirect its output to a log in ${shQuote(A.scratch)} and check the exit status; never pipe it through tail. jj gate's clippy runs on Linux only, so if the diff has #[cfg] code, also run \`cargo clippy ${PKGS} --all-targets --target x86_64-pc-windows-msvc -- -D warnings\` (check only); run \`jj gate\` after the last edit.
Tests never change the process current directory. Never cite review or audit finding numbers in code, comments or messages (#${A.issue} in the commit body is fine). Never push, never open or edit a PR, never write to GitHub. Search with \`rg\`.`

const STATE = FIX_MODE
  ? `State: bookmark ${BM} is commit ${A.prev} on main ${A.base}; @ is an empty change on top. Fold your changes in with ${FOLD}.`
  : `State: @ is an empty change on main@origin ${A.base}. Commit with \`jj commit -m\` (or \`jj describe --stdin\` then \`jj new\`), then \`jj bookmark create ${BM} -r @-\`. Fold any later change into it with ${FOLD}.`

const mutants = (from) => `Mutation check: with the work committed (@ empty), run \`scripts/mutants-diff.sh ${from} ${PKGS} > ${shQuote(`${A.scratch}/mutants-${A.issue}.log`)} 2>&1\` and read the log's tail. Exit 0 = every mutant caught, unless the log says no mutants were tested (the diff changed no mutable Rust line; say so in \`mutants\`); 2 = some missed, 3 = some timed out; any other status means the script or the unmutated tests failed, so read the log and fix that first. For each missed mutant, add a test that kills it, or give a one-line reason it is equivalent or harmless. Commit those tests like any other change (\`cargo fmt --all\`, the targeted tests, \`jj gate\`, fold into ${BM}, @ empty again), then rerun the same command with \`--iterate\` added, which retests only the mutants not yet caught, and confirm each one you added a test for is now caught. Record the counts and every survivor with its resolution in \`mutants\` and in ${NOTES_FILE}'s Evidence section.`

const WORK_SCHEMA = {
  type: 'object',
  properties: {
    status: { type: 'string', enum: ['done', 'blocked'] },
    skills_loaded: { type: 'array', items: { type: 'string' } },
    fixed: { type: 'array', items: { type: 'string' } },
    rejected: { type: 'array', items: { type: 'string' } },
    commits: { type: 'array', items: { type: 'string' } },
    tests: { type: 'string' },
    fails_without_fix: { type: 'array', items: { type: 'string' } },
    mutants: { type: 'string' },
    behaviour_changes: { type: 'string' },
    open_questions: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
  },
  required: ['status', 'skills_loaded', 'fixed', 'rejected', 'commits', 'tests', 'fails_without_fix', 'mutants', 'behaviour_changes', 'open_questions', 'summary'],
}
const FINDINGS_SCHEMA = {
  type: 'object',
  properties: { skills_loaded: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', properties: {
    severity: { type: 'string', enum: ['blocker', 'major', 'minor', 'nit'] },
    file: { type: 'string' }, line: { type: 'integer' }, summary: { type: 'string' },
    evidence: { type: 'string' }, suggested_fix: { type: 'string' },
  }, required: ['severity', 'file', 'summary', 'evidence', 'suggested_fix'] } } },
  required: ['skills_loaded', 'findings'],
}
// Reviews run as the code-reviewer agent (.claude/agents/code-reviewer.md, shipped in the repo
// so every session has it); its Critical/Required/Optional/Nit labels map onto the schema's.
const review = (scope, body) => `Review in ${REPO}. Read-only: no edits, no cargo builds or tests, no GitHub writes; \`rg\` is available. Load the \`anthropic-skills:rust-skills\` skill first and report the skills you loaded in \`skills_loaded\`. Scope: ${scope}.
SPEC: ${A.spec}
${body}
STANDARDS: AGENTS.md, CODING_STANDARDS.md (apply every rule), the area CLAUDE.md files for what the diff touches, and the glossary terms in the CONTEXT.md files; ponytail's minimal diff; accurate doc comments, the surrounding style, 100-column wrap; #[cfg] code clean on both Linux and Windows; tests never change the current directory.
Report only real defects, with evidence (file:line + quote). Severity: Critical is blocker, Required is major, Optional is minor, Nit is nit; no praise or verdict; fill only the schema's fields.`
const report = (r) => ({ fixed: r.fixed, rejected: r.rejected, tests: r.tests, fails_without_fix: r.fails_without_fix, mutants: r.mutants, behaviour_changes: r.behaviour_changes, open_questions: r.open_questions })
const skillsLog = (who, r) => log(`${who} loaded skills: ${(r.skills_loaded || []).join(', ') || 'none'}`)
// agent() returns null when skipped or dead after retries, but throws on a budget ceiling or an
// unsatisfiable schema; both take the same stop path below, so the owed-review reminder is
// always logged.
const callAgent = async (prompt, opts) => {
  try {
    return await agent(prompt, opts)
  } catch (e) {
    log(`${opts.label} threw: ${e?.message ?? e}`)
    return null
  }
}
// Every exit after a commit: the workflow's re-review is not the Standards + Spec review AGENTS.md requires.
const nextSteps = () => log(`Next, by hand: the code-review skill over ${A.base}..${BM}, fix or accept its findings, then \`scripts/review-gate.sh record-range ${A.base} ${BM} "..."\` before \`jj gpush\`.`)

phase('Implement')
const implTask = FIX_MODE
  ? `Fix the items below in the ${BM} commit. Verify each against the code first; reject the ones that are wrong with a one-line reason.`
  : `Implement #${A.issue} in one commit${A.commit_hint ? `, e.g. "${A.commit_hint}"` : ''}.`
const impl = await callAgent(`${ctx(STATE)}
${SKILLS}

${implTask}

${A.spec}

Trace each expected input through the code before and after. Write each behaviour test first. Each guard the diff adds (a check, bound or branch) gets a test you show once fails without it; list them in \`fails_without_fix\`. Keep the diff minimal, in the surrounding style, with no new dependencies. In \`behaviour_changes\`, list every input whose outcome changes.

Then \`cargo fmt --all\`, \`jj gate\`, and commit${FIX_MODE ? ' (fold into the bookmark)' : ' and create the bookmark'}.
${mutants(SCOPE_FROM)}

Finally ${FIX_MODE ? `update ${NOTES_FILE} wherever it no longer matches the code` : `write ${NOTES_FILE} (it becomes the PR body): Summary (what and why, closes #${A.issue}), Evidence (tests and results, fail-without-fix checks, mutation check, gate), Merge Danger, Open questions, Manual checks`}.`, { label: FIX_MODE ? 'fix' : 'implement', phase: 'Implement', model: 'opus', effort: 'high', schema: WORK_SCHEMA })

if (impl) skillsLog(FIX_MODE ? 'Fix' : 'Implement', impl)
if (!impl || impl.status !== 'done') {
  log(`${FIX_MODE ? 'Fix' : 'Implement'} ${impl ? 'blocked' : 'failed or was skipped'}; stopping.`)
  // A blocked implement may have stopped before creating the bookmark.
  if (!FIX_MODE) log(`Check whether bookmark ${BM} exists (\`jj bookmark list ${BM}\`); if it does not, nothing was committed and no review is owed.`)
  nextSteps()
  return { impl }
}

phase('Re-review')
const rereview = await callAgent(review(
  `\`git diff ${SCOPE_FROM} ${BM}\` (\`git log ${A.base}..${BM}\`)${FIX_MODE ? `, in the context of the whole change \`git diff ${A.base} ${BM}\`` : ''}`,
  `The implementer reports: ${JSON.stringify(report(impl))}. PR notes: ${NOTES_FILE}.
Check both axes. SPEC axis: every requirement met, nothing unasked-for added; hand-trace each expected input through the new code. ${A.review_focus || ''}
Tests: each guard the diff adds has a test that reaches it, and every mutation-check survivor has a test or a sound reason. Docs, the commit message (\`git log -1 --format=%B ${BM}\`: no │/○, no AI trailer) and the PR notes match the code.`,
), { label: 're-review', phase: 'Re-review', model: 'sonnet', effort: 'xhigh', agentType: 'code-reviewer', schema: FINDINGS_SCHEMA })

if (!rereview) { log('Re-review failed or was skipped; stopping with no review.'); nextSteps(); return { impl, rereview } }
skillsLog('Re-review', rereview)
const findings = rereview.findings
log(`Re-review: ${findings.length} findings`)
if (!findings.length) { nextSteps(); return { impl, findings } }

phase('Follow-up')
const follow = await callAgent(`${ctx(`State: the #${A.issue} work is in bookmark ${BM}; @ should be an empty child of it. Note \`git rev-parse ${BM}\` before your first edit: that commit id is <R>. Fold changes in with ${FOLD}.`)}
${SKILLS}

A Sonnet re-review of ${BM} reported:
${JSON.stringify(findings, null, 1)}
Verify each against the code; fix the valid ones minimally (a behaviour fix gets a test shown to fail without it); reject the others with a one-line reason. A change to the settled design itself is not yours to make: report it in open_questions. Targeted tests, \`cargo fmt --all\`, \`jj gate\`, then fold into ${BM}.
Only if you added or changed a guard, with <R> replaced by that commit id: ${mutants('<R>')}
Otherwise say "not rerun" in \`mutants\`. Update ${NOTES_FILE} wherever it no longer matches. \`commits\` lists commit IDs you created (empty if you only folded).`, { label: 'follow-up', phase: 'Follow-up', model: 'opus', effort: 'high', schema: WORK_SCHEMA })

if (follow) skillsLog('Follow-up', follow)
if (!follow || follow.status !== 'done' || !follow.fixed.length) {
  log(follow ? `Follow-up ${follow.status}, ${follow.fixed.length} fixed; no last-commit review.` : 'Follow-up failed or was skipped.')
  nextSteps()
  return { impl, findings, follow }
}

phase('Last-commit review')
const last = await callAgent(review(
  `\`git diff ${SCOPE_FROM} ${BM}\`, focused on what these findings touched`,
  `The re-review findings were: ${JSON.stringify(findings)}. The follow-up reports: ${JSON.stringify(report(follow))}.
Check that each fix is correct and complete, each rejection is sound, new tests fail without their fix, nothing regressed, and ${NOTES_FILE} matches the code. Both axes.`,
), { label: 'last-commit review', phase: 'Last-commit review', model: 'sonnet', effort: 'xhigh', agentType: 'code-reviewer', schema: FINDINGS_SCHEMA })

if (!last) log('Last-commit review failed or was skipped; the follow-up is unreviewed.')
else {
  skillsLog('Last-commit review', last)
  log(last.findings.length
    ? `Last-commit review: ${last.findings.length} UNRESOLVED findings (in the result's \`last\`); fix or accept them before the review below.`
    : 'Last-commit review: 0 findings')
}
nextSteps()
return { impl, findings, follow, last }
