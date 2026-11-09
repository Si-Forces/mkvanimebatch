// scripts/mutants-diff.sh's own guards, run against a throwaway git repo. cargo and cargo-mutants
// are stubs that record what they were given, and PATH holds only the tools the script needs, so
// a real cargo-mutants install never leaks in and the "missing" case is deterministic.
import test, { after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

// Every fixture lives under one temp dir, removed when the suite ends.
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "mutants-diff-"));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

const which = (tool) => {
  const r = spawnSync("bash", ["-c", `command -v ${tool}`], { encoding: "utf8" });
  assert.equal(r.status, 0, `${tool} not found`);
  return r.stdout.trim();
};
const BASH = which("bash");
const GIT = which("git");

// A bin dir with the plain tools, a git wrapper that fails `git status` when asked to, and
// optionally the cargo-mutants stubs.
const makeBin = (name, withMutants) => {
  const bin = path.join(tmp, name);
  fs.mkdirSync(bin);
  for (const tool of ["cat", "cp", "mkdir", "readlink", "sed", "touch"]) {
    fs.symlinkSync(which(tool), path.join(bin, tool));
  }
  const script = (file, body) => fs.writeFileSync(path.join(bin, file), `#!/bin/sh\n${body}`, { mode: 0o755 });
  script("git", `[ "$1" = status ] && [ -n "$FAKE_GIT_STATUS_FAIL" ] && exit 128\nexec "${GIT}" "$@"\n`);
  if (withMutants) {
    script("cargo-mutants", "exit 0\n");
    // `cargo mutants ...`: records its args, temp-dir env and diff, writes a report when
    // FAKE_REPORT, FAKE_MISSED or FAKE_TIMEOUT is set (the last two also fill missed.txt and
    // timeout.txt), and exits with FAKE_STATUS.
    script("cargo", `printf '%s\\n' "$@" > "$FAKE_LOG/args"
printf 'TMPDIR=%s\\nTMP=%s\\nTEMP=%s\\n' "$TMPDIR" "$TMP" "$TEMP" > "$FAKE_LOG/env"
while [ $# -gt 0 ]; do
  case $1 in --in-diff) cp "$2" "$FAKE_LOG/diff"; shift ;; --output) out=$2; shift ;; esac
  shift
done
if [ -n "$FAKE_REPORT$FAKE_MISSED$FAKE_TIMEOUT" ]; then
  mkdir -p "$out/mutants.out"
  echo '{}' > "$out/mutants.out/outcomes.json"
  : > "$out/mutants.out/missed.txt"
  : > "$out/mutants.out/timeout.txt"
  [ -z "$FAKE_MISSED" ] || printf '%s\\n' "$FAKE_MISSED" > "$out/mutants.out/missed.txt"
  [ -z "$FAKE_TIMEOUT" ] || printf '%s\\n' "$FAKE_TIMEOUT" > "$out/mutants.out/timeout.txt"
fi
exit "\${FAKE_STATUS:-0}"
`);
  }
  return bin;
};
const BIN = makeBin("bin", true);
const BIN_NO_MUTANTS = makeBin("bin-no-mutants", false);

const GIT_ENV = {
  GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@example.com",
  GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@example.com",
  GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
};

// A repo holding the script under test: c1 adds a.rs and README, c2 changes both, c3 changes
// README only. Returns { dir, c1, c2, git, write }; git and write act on the repo.
const fixture = () => {
  const dir = fs.mkdtempSync(path.join(tmp, "repo-"));
  const git = (...args) => {
    const r = spawnSync(GIT, args, { cwd: dir, encoding: "utf8", env: { ...process.env, ...GIT_ENV } });
    assert.equal(r.status, 0, r.stderr);
    return r.stdout.trim();
  };
  const write = (file, text) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), text);
  };
  git("init", "-q");
  write("scripts/mutants-diff.sh", fs.readFileSync(path.join(here, "mutants-diff.sh")));
  write("scripts/hooks/hidden-changes.sh", fs.readFileSync(path.join(here, "hooks/hidden-changes.sh")));
  write("a.rs", "fn a() {}\n");
  write("README", "one\n");
  git("add", "-A");
  git("commit", "-qm", "c1");
  const c1 = git("rev-parse", "HEAD");
  write("a.rs", "fn a() { b() }\n");
  write("README", "two\n");
  git("commit", "-qam", "c2");
  const c2 = git("rev-parse", "HEAD");
  write("README", "three\n");
  git("commit", "-qam", "c3");
  return { dir, c1, c2, git, write };
};

const run = (repo, args, env = {}, bin = BIN) => {
  const log = fs.mkdtempSync(path.join(tmp, "log-"));
  const r = spawnSync(BASH, [path.join(repo.dir, "scripts/mutants-diff.sh"), ...args], {
    cwd: repo.dir,
    encoding: "utf8",
    env: {
      ...GIT_ENV, PATH: bin, HOME: path.join(tmp, "home"), FAKE_LOG: log,
      MUTANTS_TMPDIR: path.join(repo.dir, "..", `${path.basename(repo.dir)}-mutants`), ...env,
    },
  });
  const read = (f) => (fs.existsSync(path.join(log, f)) ? fs.readFileSync(path.join(log, f), "utf8") : undefined);
  return { ...r, args: read("args"), env: read("env"), diff: read("diff") };
};

test("refuses a missing base or an option in its place", () => {
  const repo = fixture();
  for (const args of [[], ["-p", "mkv-core"]]) {
    const r = run(repo, args);
    assert.equal(r.status, 64, JSON.stringify(args));
    assert.match(r.stderr, /usage:/);
    assert.equal(r.args, undefined);
  }
});

test("refuses an unknown base", () => {
  const r = run(fixture(), ["no-such-ref"]);
  assert.equal(r.status, 64);
  assert.match(r.stderr, /unknown base: no-such-ref/);
  assert.equal(r.args, undefined);
});

test("refuses a dirty tree, untracked files included", () => {
  const repo = fixture();
  fs.writeFileSync(path.join(repo.dir, "new.rs"), "");
  const r = run(repo, [repo.c1]);
  assert.equal(r.status, 65);
  assert.match(r.stderr, /working tree differs from HEAD/);
  assert.equal(r.args, undefined);
});

test("refuses an edit hidden behind assume-unchanged", () => {
  const repo = fixture();
  assert.equal(spawnSync(GIT, ["update-index", "--assume-unchanged", "a.rs"], { cwd: repo.dir }).status, 0);
  fs.writeFileSync(path.join(repo.dir, "a.rs"), "edited\n");
  const r = run(repo, [repo.c1]);
  assert.equal(r.status, 65);
  assert.match(r.stderr, /a\.rs/);
  assert.equal(r.args, undefined);
});

test("a failing git status stops the run instead of reading as clean", () => {
  const repo = fixture();
  const r = run(repo, [repo.c1], { FAKE_GIT_STATUS_FAIL: "1" });
  assert.notEqual(r.status, 0);
  assert.equal(r.args, undefined);
});

test("exits 0 without cargo when no Rust changed since base", () => {
  const repo = fixture();
  const r = run(repo, [repo.c2], {}, BIN_NO_MUTANTS);
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /no Rust changes since .*; no mutants were tested/);
});

test("refuses when cargo-mutants is missing", () => {
  const repo = fixture();
  const r = run(repo, [repo.c1], {}, BIN_NO_MUTANTS);
  assert.equal(r.status, 69);
  assert.match(r.stderr, /cargo-mutants missing/);
});

test("mutates only the Rust diff from base to HEAD, with every temp dir off /tmp", () => {
  const repo = fixture();
  const mt = path.join(tmp, "mt");
  const r = run(repo, [repo.c1, "-p", "mkv-core"], { MUTANTS_TMPDIR: mt, FAKE_STATUS: "2", FAKE_MISSED: "a.rs:1: replace a" });
  assert.equal(r.status, 2, r.stderr);
  assert.match(r.diff, /^\+fn a\(\) \{ b\(\) \}$/m);
  assert.doesNotMatch(r.diff, /README/);
  assert.equal(r.env, `TMPDIR=${mt}\nTMP=${mt}\nTEMP=${mt}\n`);
  assert.deepEqual(r.args.trim().split("\n"), [
    "mutants", "--in-diff", `${mt}/out/in.diff`, "--output", `${mt}/out`, "--no-shuffle", "-p", "mkv-core",
  ]);
  assert.match(r.stdout, /--- missed:\na\.rs:1: replace a/);
});

test("defaults its temp dir to ~/.cache/mkv-mutants", () => {
  const repo = fixture();
  const r = run(repo, [repo.c1], { MUTANTS_TMPDIR: undefined });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.env, new RegExp(`^TMPDIR=${path.join(tmp, "home", ".cache", "mkv-mutants")}$`, "m"));
});

test("prints only this run's report, and only for a run that tested mutants", () => {
  const repo = fixture();
  const first = run(repo, [repo.c1], { FAKE_STATUS: "2", FAKE_MISSED: "old survivor" });
  assert.match(first.stdout, /old survivor/);
  // A later run that writes no report (--list, say) must not reprint the old one.
  const list = run(repo, [repo.c1, "--list"]);
  assert.equal(list.status, 0, list.stderr);
  assert.doesNotMatch(list.stdout, /old survivor/);
  // Exit 4 (the unmutated tests fail) writes a report too, but it is not a mutation result.
  const broken = run(repo, [repo.c1], { FAKE_STATUS: "4", FAKE_MISSED: "new survivor" });
  assert.equal(broken.status, 4);
  assert.doesNotMatch(broken.stdout, /survivor|no mutants were tested/);
  const timedOut = run(repo, [repo.c1], { FAKE_STATUS: "3", FAKE_TIMEOUT: "slow one" });
  assert.equal(timedOut.status, 3);
  assert.match(timedOut.stdout, /--- timeout:\nslow one/);
  assert.doesNotMatch(timedOut.stdout, /--- missed:/);
});

test("a base that a later amend rewrote scopes the run to the amend alone", () => {
  // A fix round passes the bookmark's commit from before the round, which a squash has since
  // rewritten, so it is no longer an ancestor of HEAD. Its merge-base with HEAD would widen the
  // run back to the whole change.
  const repo = fixture();
  repo.write("a.rs", "fn a() { b() }\nfn c() {}\n");
  repo.git("commit", "-qam", "c4");
  const before = repo.git("rev-parse", "HEAD");
  repo.write("a.rs", "fn a() { b() }\nfn c() {}\nfn d() {}\n");
  repo.git("commit", "-q", "--amend", "-am", "c4 amended");
  const r = run(repo, [before]);
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.diff, /^\+fn d\(\) \{\}$/m);
  assert.doesNotMatch(r.diff, /^\+fn c\(\) \{\}$/m);

  // An amend that leaves the Rust alone has nothing to mutate.
  const amended = repo.git("rev-parse", "HEAD");
  repo.write("README", "four\n");
  repo.git("commit", "-q", "--amend", "-am", "c4 amended again");
  const docs = run(repo, [amended]);
  assert.equal(docs.status, 0, docs.stderr);
  assert.match(docs.stdout, /no Rust changes since/);
  assert.equal(docs.args, undefined);
});

test("MUTANTS_OUT moves the diff and the report off the temp dir", () => {
  const repo = fixture();
  const out = path.join(tmp, "elsewhere");
  const r = run(repo, [repo.c1], { MUTANTS_OUT: out, FAKE_STATUS: "2", FAKE_MISSED: "a.rs:1: replace a" });
  assert.equal(r.status, 2, r.stderr);
  assert.deepEqual(r.args.trim().split("\n").slice(1, 5), ["--in-diff", `${out}/in.diff`, "--output", out]);
  assert.match(r.stdout, /--- missed:\na\.rs:1: replace a/);
});

test("exit 0 says when no mutant was tested, and stays quiet when all were caught", () => {
  const repo = fixture();
  // cargo-mutants exits 0 with no report when the diff selects no mutants.
  const none = run(repo, [repo.c1]);
  assert.equal(none.status, 0, none.stderr);
  assert.match(none.stdout, /no mutants were tested/);
  const caught = run(repo, [repo.c1], { FAKE_REPORT: "1" });
  assert.equal(caught.status, 0, caught.stderr);
  assert.doesNotMatch(caught.stdout, /no mutants were tested|---/);
});
