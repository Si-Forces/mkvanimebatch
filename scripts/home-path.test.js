// scripts/hooks/home-path.sh, fed diffs on stdin (or pointed at a throwaway git repo) with a
// throwaway HOME, so the machine's real home directory never decides a result.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { script, tmp, home, envFor, run, sh, gitRepo, hit, diff, publishedArgs } from "./test-support/home-path.js";

test("refuses an added line naming HOME, with the file and line number but not the text", () => {
  const r = run(diff("a.js", `+const REPO = '${home}/My Project/repo'`));
  hit(r);
  // HOME and its resolved form are the same path here: one line per hit, not one per form.
  assert.equal(r.stderr.split("a.js:1\n").length - 1, 1, r.stderr);
  // The report may end up in a PR description, which nothing checks.
  assert.ok(!r.stderr.includes(home) && !r.stderr.includes("My Project"), r.stderr);
});

test("allows generic home paths, removed lines and context", () => {
  assert.equal(run(diff("t.rs", '+assert_eq!(p("/home/user/show"), "/home/u/App.AppImage");')).status, 0);
  assert.equal(run(diff("a.js", `-const REPO = '${home}/repo'`, `+const REPO = A.repo`)).status, 0);
  assert.equal(run(diff("a.js", ` const REPO = '${home}/repo'`)).status, 0);
  assert.equal(run("").status, 0);
});

test("matches HOME with ASCII case folding and the documented segment boundaries", () => {
  assert.equal(run(diff("a.rs", `+mod x; // ${home}z/lib.rs`)).status, 0, "a longer name");
  assert.equal(run(diff("a.sh", `+ls ${home}_old ${home}-2 ${home}2 ${home}~`)).status, 0, "siblings");
  hit(run(diff("a.rs", `+let p = "src${home}";`)), "inside another path");
  hit(run(diff("a.sh", `+gcc -I${home}/include`)), "after a flag");
  hit(run(diff("a.sh", `+cp /mnt/backup${home}/x .`)), "under a mount point");
  hit(run(diff("a.sh", `+cp ${home}z/a ${home}/b`)), "after an occurrence that does not count");
  hit(run(diff("a.md", `+[doc](file://${home}/doc.md)`)), "after a file URL");
  hit(run(diff("a.md", `+see ${home}`)), "at the end of a line");
  hit(run(diff("a.md", `+cloned into ${home}.`)), "at the end of a sentence");
  hit(run(diff("a.md", `+cloned into ${home}...`)), "before an ellipsis");
  hit(run(diff("a.md", `+see ${home}...more`)), "before an ellipsis and more text");
  hit(run(diff("a.sh", `+git diff ${home}..main`)), "in a git range");
  assert.equal(run(diff("a.md", `+cp ${home}.bak/x .`)).status, 0, "a sibling with a suffix");
  hit(run(diff("a.md", `+see ${home.toUpperCase()}/x`)), "in another case");
  for (const suffix of ["+x", "./x", "..x", "é/x"]) {
    hit(run(diff("a.md", `+see ${home}${suffix}`)), `conservative boundary: ${suffix}`);
  }
});

test("redacts literal and dashed home spellings in reported file names", () => {
  const windows = "C:\\Users\\tester";
  const fixtures = [
    { file: `${home.slice(1)}/f.txt`, hidden: path.basename(home) },
    { file: `foo${home}/f.txt`, hidden: home },
    { file: `foo${home.replaceAll("/", "//")}/f.txt`, hidden: path.basename(home) },
    { file: `foo${home.toUpperCase()}/f.txt`, hidden: home.toUpperCase() },
    { file: `foo${home}/again${home}/again${home}/again${home}/f.txt`, hidden: home },
    { file: `foo/${dashed(home)}/f.txt`, hidden: dashed(home) },
    { file: `foo/${windows}/f.txt`, hidden: windows },
    { file: `foo/${windows}${home}/f.txt`, hidden: windows },
    { file: `foo${home}/${windows}/f.txt`, hidden: windows },
    {
      file: `foo/${windows.replaceAll("\\", "\\\\")}/f.txt`,
      hidden: windows.replaceAll("\\", "\\\\"),
    },
  ];
  for (const { file, hidden } of fixtures) {
    const r = run(diff(file, `+see ${home}/x`), { USERPROFILE: windows });
    hit(r, file);
    assert.ok(r.stderr.includes("<home>") && r.stderr.includes("/f.txt:1\n"), r.stderr);
    assert.ok(!r.stderr.includes(hidden), r.stderr);
    assert.ok(!r.stderr.toLowerCase().includes(home.toLowerCase()), r.stderr);
  }
  // Exercise real Git quoting, including escaped slashes, literal backslashes and octal bytes.
  const { dir, git, commit } = gitRepo("report-home");
  const base = commit("clean\n");
  git("config", "diff.noprefix", "true");
  for (const file of [`${home.slice(1)}/relative.txt`, `foo${home}/f".txt`, `foo${home.replaceAll("/", "\\/")}/f.txt`]) {
    fs.mkdirSync(path.join(dir, path.dirname(file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), `clean\n${home}/x\n`);
  }
  for (const quotePath of ["true", "false"]) {
    git("config", "core.quotePath", quotePath);
    git("add", "-A");
    const staged = run("", {}, { args: ["--staged"], cwd: dir });
    hit(staged);
    assert.ok(staged.stderr.includes("<home>") && !staged.stderr.includes(path.basename(home)), staged.stderr);
    assert.ok(staged.stderr.includes('<home>/relative.txt:2\n') && staged.stderr.includes('"foo<home>/f\\".txt":2\n'), staged.stderr);
    assert.doesNotMatch(staged.stderr, /"b\//);
  }
  git("commit", "-q", "-m", "quoted filenames");
  const tip = git("rev-parse", "HEAD");
  for (const quotePath of ["true", "false"]) {
    git("config", "core.quotePath", quotePath);
    const published = run("", {}, { args: publishedArgs(tip, base), cwd: dir });
    hit(published);
    assert.ok(published.stderr.includes(`${tip.slice(0, 12)} "foo<home>/f\\".txt":2\n`), published.stderr);
    assert.ok(published.stderr.includes("<home>/relative.txt:2\n") && !published.stderr.includes(path.basename(home)), published.stderr);
  }
  // Boundaries apply to redaction too, so an unrelated sibling name stays recognizable.
  const sibling = run(diff(`foo${home}ish/f.txt`, `+${home}/x`));
  hit(sibling);
  assert.ok(sibling.stderr.includes(`foo${home}ish/f.txt:1`), sibling.stderr);
});

test("refuses newline identities instead of splitting them into unrelated home fragments", () => {
  for (const env of [{ HOME: "/no-such-home/check\nhome" }, { USERPROFILE: "C:\\Users\\check\nhome" }]) {
    const r = run(diff("home.txt", "+home/x"), env);
    assert.equal(r.status, 1);
    assert.match(r.stderr, /must not contain newlines/);
    assert.doesNotMatch(r.stderr, /home\.txt|check/);
  }
  const target = path.join(tmp, "target\n");
  const alias = path.join(tmp, "newline-alias");
  fs.mkdirSync(target);
  fs.symlinkSync(target, alias);
  const resolved = run(diff("f.txt", "+clean"), { HOME: alias });
  assert.equal(resolved.status, 1);
  assert.match(resolved.stderr, /resolved name contains a newline/);
  assert.ok(!resolved.stderr.includes(tmp), resolved.stderr);
});

test("redacts Git-quoted Unicode and backslash home names while keeping control bytes quoted", () => {
  const { dir, git } = gitRepo("encoded-report");
  for (const [i, name] of ["josé", "a\\b", 'a"b', "a\tb"].entries()) {
    const identity = `/no-such-home/${name}`;
    const file = `foo${identity}/f${i}\n.txt`;
    fs.mkdirSync(path.join(dir, path.dirname(file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), `${identity}/x\n`);
    git("add", "-A");
    const r = run("", { HOME: identity }, { args: ["--staged"], cwd: dir });
    hit(r, name);
    assert.ok(r.stderr.includes(`"foo<home>/f${i}\\012.txt":1\n`), r.stderr);
    git("reset", "-q"); // the next identity must not report earlier fixtures
  }
});

test("preserves every supported Git filename escape in staged and published reports", () => {
  const { dir, git, commit } = gitRepo("retained-escapes");
  let old = commit("clean\n");
  for (const [character, encoded] of [
    ["\x01", "\\001"], ["\x1f", "\\037"], [" ", " "], ["~", "~"], ["\x7f", "\\177"],
    ["\x07", "\\007"], ["\b", "\\010"], ["\t", "\\011"], ["\n", "\\012"],
    ["\v", "\\013"], ["\f", "\\014"], ["\r", "\\015"],
    ["\\", "\\\\"], ['"', '\\"'], ["é", "\\303\\251"],
  ]) {
    const file = `foo${home}/f${character}\n.txt`;
    fs.mkdirSync(path.join(dir, path.dirname(file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), `clean\n${home}/x\n`);
    git("add", "-A");
    const expected = `"foo<home>/f${encoded}\\012.txt":2\n`;
    const staged = run("", {}, { args: ["--staged"], cwd: dir });
    hit(staged, encoded);
    assert.ok(staged.stderr.includes(expected), staged.stderr);
    git("commit", "-q", "-m", "escaped filename");
    const tip = git("rev-parse", "HEAD");
    const published = run("", {}, { args: publishedArgs(tip, old), cwd: dir });
    hit(published, encoded);
    assert.ok(published.stderr.includes(`${tip.slice(0, 12)} ${expected}`), published.stderr);
    old = tip;
  }
});

test("quotes the highest filename byte in staged and published reports", { skip: process.platform === "win32" && "raw POSIX filename bytes" }, () => {
  const { dir, git, commit } = gitRepo("byte-255");
  const base = commit("base\n");
  fs.mkdirSync(path.join(dir, `foo${home}`), { recursive: true });
  const filename = Buffer.concat([Buffer.from(path.join(dir, `foo${home}/f`)), Buffer.from([255]), Buffer.from(".txt")]);
  fs.writeFileSync(filename, `clean\n${home}/x\n`);
  git("add", "-A");
  const expected = '"foo<home>/f\\377.txt":2\n';
  const staged = run("", {}, { args: ["--staged"], cwd: dir });
  hit(staged);
  assert.ok(staged.stderr.includes(expected), staged.stderr);
  git("commit", "-q", "-m", "raw filename byte");
  const tip = git("rev-parse", "HEAD");
  const published = run("", {}, { args: publishedArgs(tip, base), cwd: dir });
  hit(published);
  assert.ok(published.stderr.includes(`${tip.slice(0, 12)} ${expected}`), published.stderr);
});

test("hides malformed Git-quoted filenames instead of echoing their encoded bytes", () => {
  for (const name of ['"b/foo\\12/bar"', '"b/foo\\777/bar"', '"b/foo\\q/bar"', '"b/foo', '"', '"b/foo\\"', `"b${home.replace(/\/([^/]+)$/, '/\\000$1')}/f.txt"`]) {
    const r = run(`diff --git a/f b/f\n--- a/f\n+++ ${name}\n@@ -0,0 +1 @@\n+${home}/x\n`);
    hit(r);
    assert.match(r.stderr, /     "<quoted filename>":1\n/);
    assert.ok(!r.stderr.includes("foo"), r.stderr);
  }
});

test("names the commit in git log output", () => {
  const sha = "0123456789abcdef0123456789abcdef01234567";
  const r = run(`commit ${sha}\n${diff(".codex/x.toml", `+see ${home}/Downloads/doc.md`)}`);
  hit(r);
  assert.ok(r.stderr.includes("0123456789ab .codex/x.toml:1\n"), r.stderr);
  // git writes "+++ b/<name><tab>" for a name that holds a space.
  const spaced = run(`diff --git a/my file.md b/my file.md\n--- a/my file.md\t\n+++ b/my file.md\t\n@@ -0,0 +1 @@\n+${home}/x\n`);
  assert.ok(spaced.stderr.includes("     my file.md:1\n"), spaced.stderr);
});

test("scans only hunks, never git show metadata before or between commits", () => {
  const first = "1".repeat(40), second = "2".repeat(40);
  for (const suffix of ["", " (HEAD -> topic)"]) {
    const metadata = (sha) => `commit ${sha}${suffix}\nAuthor: t\n\n    Mention ${home}/x\n\n`;
    assert.deepEqual(run(metadata(first)), { status: 0, stderr: "" });
    // A custom git log --format can also print unindented message bodies.
    const r = run(metadata(first) + diff("first.txt", "+clean") + metadata(second) +
      `+Mention ${home}/x\n` + diff(`foo${home}/fixture.txt`, "+clean") +
      diff("second.txt", `+${home}/x`));
    hit(r);
    assert.deepEqual(r.stderr.split("\n").filter(l => l.startsWith("     ")), [
      "     222222222222 second.txt:1",
    ]);
  }
});

test("streams refusals before stdin closes instead of buffering the diff and report", async () => {
  const child = spawn("bash", [script], { cwd: tmp, env: envFor({}), stdio: ["pipe", "ignore", "pipe"] });
  const closed = once(child, "close");
  let output = "";
  child.stderr.on("data", chunk => { output += chunk; });
  try {
    const firstOutput = once(child.stderr, "data", { signal: AbortSignal.timeout(5000) });
    child.stdin.write(diff("stream.txt", ...Array(1000).fill(`+${home}/x`)));
    await firstOutput;
    assert.match(output, /name your home directory/);
  } finally {
    child.stdin.end();
    await closed;
  }
  assert.equal(child.exitCode, 1, output);
  assert.match(output, /stream\.txt:1000\n/);
  assert.equal(output.split("these added lines name your home directory").length - 1, 1, output);
});

test("a file header is not content, even before the first hunk", () => {
  // Repo-relative names are not read, so a fixture folder shaped like the home passes.
  const named = `a${home}/x`;
  const deleted = `diff --git ${named} b/y\ndeleted file mode 100644\n--- ${named}\n+++ /dev/null\n@@ -1 +0,0 @@\n-clean\n`;
  assert.deepEqual(run(deleted), { status: 0, stderr: "" });
});

test("stdin diff --cc recognizes headers and reports the result file and line", () => {
  const header = (file) => `diff --cc ${file}\nindex a,b..c\n` +
    `--- a/${file}\n+++ b/${file}\n@@@ -1 -1 +1 @@@\n`;
  // A filename that resembles the home must never be scanned as added content.
  assert.deepEqual(run(`${header(`foo${home}/f.txt`)}++clean\n`), {
    status: 0, stderr: "",
  });
  const r = run(`${header("merged.txt")}++${home}/x\n`);
  hit(r);
  assert.match(r.stderr, /     merged\.txt:1\n/);
});

test("combined file headers reset hunk state after another file", () => {
  for (const kind of ["cc", "combined"]) {
    const second = `diff --${kind} second.txt\n--- a/second.txt\n+++ b/second.txt\n@@@ -1 -1 +37 @@@\n++${home}/x\n`;
    const r = run(diff("first.txt", "+clean") + second);
    hit(r, kind);
    assert.deepEqual(r.stderr.split("\n").filter(line => line.startsWith("     ")), ["     second.txt:37"]);
  }
});

test("a +++ or --- line inside a hunk is content, not a file header", () => {
  const r = run(`${diff("a.sql", "-- old")}+++ ${home}/notes\n`);
  hit(r);
  assert.ok(r.stderr.includes("a.sql:1\n"), r.stderr);
});

test("numbers a line by its place in the new file", () => {
  const r = run(
    "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n" +
      `@@ -1,2 +1,3 @@\n context\n+clean\n+${home}/x\n` +
      `@@ -10 +20,2 @@\n-old\n+clean\n+${home}/y\n`,
  );
  hit(r);
  assert.ok(r.stderr.includes("a.md:3\n") && r.stderr.includes("a.md:21\n"), r.stderr);
});

test("counts a blank context line, which diff.suppressBlankEmpty writes with no leading space", () => {
  const r = run(
    "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n" + `@@ -1,3 +1,5 @@\n 1\n\n 3\n+clean\n+${home}/x\n`,
  );
  hit(r);
  assert.ok(r.stderr.includes("a.md:5\n"), r.stderr);
});

test("--published counts a blank context line git adds back to a -U0 diff", () => {
  // -U0 asks for no context; diff.interHunkContext and GIT_DIFF_OPTS put some back anyway.
  const { dir: repo, git, commit } = gitRepo("blank-context");
  git("config", "diff.suppressBlankEmpty", "true");
  git("update-ref", "refs/remotes/origin/main", commit("1\n\n3\n\n5\n\n7\n\n9\n"));
  const tip = commit(`1\n\n3\n\n5\nX\n7\n\n9\n${home}/x\n`);
  for (const env of [{ GIT_DIFF_OPTS: "-u9" }, { GIT_DIFF_OPTS: undefined }]) {
    if (env.GIT_DIFF_OPTS === undefined) git("config", "diff.interHunkContext", "9");
    const r = run("", env, { args: publishedArgs(tip), cwd: repo });
    hit(r, JSON.stringify(env));
    assert.ok(r.stderr.includes(`${tip.slice(0, 12)} f.txt:10\n`), r.stderr);
  }
});

test("checks USERPROFILE with backslashes and with forward slashes", () => {
  const env = { USERPROFILE: "C:\\Users\\tester" };
  hit(run(diff("a.ps1", "+cd C:\\Users\\tester\\repo"), env));
  hit(run(diff("a.ps1", "+cd c:\\users\\tester\\repo"), env));
  hit(run(diff("a.js", "+'C:/Users/tester/repo'"), env));
  const escaped = diff("a.toml", '+path = "C:\\\\Users\\\\tester\\\\repo"');
  hit(run(escaped, env), "escaped in a string");
  // bash before 4.3 keeps the quotes of a quoted replacement, as macOS's bash 3.2 does.
  hit(run(escaped, { ...env, BASH_COMPAT: "42" }), "escaped, on an old bash");
  hit(run(diff("a.bat", "+cl -IC:\\Users\\tester\\inc"), env), "after a flag");
  assert.equal(run(diff("a.js", "+'C:/Users/other/repo'"), env).status, 0);
});

test("ignores trailing separators on USERPROFILE and HOME", () => {
  const line = diff("a.ps1", "+cd C:\\Users\\tester\\repo");
  for (const p of ["C:\\Users\\tester\\", "C:/Users/tester/", "C:\\Users\\tester\\\\"]) {
    hit(run(line, { USERPROFILE: p }), p);
  }
  // A drive alone names nobody, as a HOME of / does not.
  assert.equal(run(diff("a.ps1", "+cd C:\\tools"), { USERPROFILE: "C:\\" }).status, 0);
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester//" }));
});

test("a doubled separator inside HOME or USERPROFILE counts squeezed and as set", () => {
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home//tester" }));
  hit(run(diff("a.sh", "+ls /no-such-home//tester/x"), { HOME: "/no-such-home//tester/" }), "doubled in both");
  // Only a run of slashes is squeezed in the line, so a doubled backslash matches as set.
  const odd = { HOME: "/no-such-home/we\\\\ird/tester" };
  hit(run(diff("a.sh", "+ls /no-such-home/we\\\\ird/tester/x"), odd), "HOME as set, with a doubled backslash");
  const profile = { USERPROFILE: "C:\\Users\\\\tester\\" };
  hit(run(diff("a.ps1", "+cd C:\\Users\\tester\\repo"), profile));
  hit(run(diff("a.ps1", "+cd C:\\Users\\\\tester\\repo"), profile), "USERPROFILE as set");
});

test("checks the \\/ a JSON writer may put for /", () => {
  const escaped = `${home}/x`.replaceAll("/", "\\/");
  hit(run(diff("a.json", `+{"p":"${escaped}"}`)));
  const env = { USERPROFILE: "C:\\Users\\tester" };
  hit(run(diff("a.json", '+{"p":"C:\\/Users\\/tester\\/x"}'), env), "USERPROFILE");
  // That escape inside another string doubles the backslash.
  const twice = `${home}/x`.replaceAll("/", "\\\\/");
  hit(run(diff("a.js", `+const s = "{\\"p\\":\\"${twice}\\"}";`)), "escaped twice");
});

test("reads a doubled / in the line as one", () => {
  // HOME is squeezed, so the line is too: a shell or a joined path writes // freely.
  hit(run(diff("a.sh", `+ls ${home.replaceAll("/", "//")}//x`)), "every separator doubled");
  hit(run(diff("a.sh", `+ls ${path.dirname(home)}///${path.basename(home)}`)), "tripled, at the end");
  hit(run(diff("a.json", `+{"p":"${home.replaceAll("/", "\\/\\/")}"}`)), "doubled and escaped");
  hit(run(diff("a.js", "+'C://Users//tester/repo'"), { USERPROFILE: "C:\\Users\\tester" }), "USERPROFILE");
  assert.equal(run(diff("a.sh", `+ls ${home}z//x ${path.dirname(home)}//other`)).status, 0, "siblings");
});

test("fully squeezes long runs of profile backslashes", () => {
  const profile = "C:" + "\\".repeat(4) + "Users" + "\\".repeat(4) + "tester";
  hit(run(diff("f.txt", "+C:\\Users\\tester/x"), { USERPROFILE: profile }));
});

// Claude Code's spelling of a working directory in its scratchpad and memory paths.
const dashed = (p) => p.replace(/[^A-Za-z0-9]/g, "-");

test("checks the dashed spelling Claude Code gives a working directory", () => {
  hit(run(diff("a.log", `+see /tmp/claude-1000/${dashed(home)}-My-Project-repo/scratchpad/x.log`)));
  hit(run(diff("a.log", `+see ${dashed(home).toUpperCase()}`)), "at the end, in another case");
  assert.equal(run(diff("a.log", `+see ${dashed(home)}z-x ${dashed(home)}2-x`)).status, 0, "a longer name");
  hit(run(diff("a.log", "+see C--Users-tester-repo"), { USERPROFILE: "C:\\Users\\tester" }), "USERPROFILE");
  // Nothing comes before it at the start of a line, though gawk's substr(line, 0, 1) is the
  // line's first character.
  const windows = { USERPROFILE: "C:\\Users\\tester", HOME: "/" };
  hit(run(diff("a.log", "+C--Users-tester-repo"), windows), "USERPROFILE at the start of a line");
  for (const lead of ["/", ".", "_", "+", "-"]) hit(run(diff("a.log", `+${lead}C--Users-tester-repo`), windows), lead);
  hit(run(diff("a.log", "+tester-x-repo"), { HOME: "tester/x" }), "a relative HOME at the start of a line");
  assert.equal(run(diff("a.log", "+xC--Users-tester-repo"), windows).status, 0, "inside a word");
  const absent = { HOME: "/no-such-home/tester" };
  hit(run(diff("a.log", "+aa x-no-such-home-tester-a /-no-such-home-tester-b"), absent), "after an occurrence that does not count");
  // A full stop inside the name is already a -, so one after it starts an extension.
  hit(run(diff("a.log", "+see /tmp/claude/-no-such-home-tester.jsonl"), absent), "before an extension");
  hit(run(diff("a.log", "+see -no-such-home-tester."), absent), "at the end of a sentence");
  hit(run(diff("a.log", "+see -no-such-home-tester-x"), { HOME: "/no-such-home/tester" }), "a HOME with no resolved form");
  // One - for each UTF-16 unit that is not an ASCII letter or digit: one for a character in
  // the Basic Multilingual Plane, however many bytes it takes, and two for one outside it.
  for (const LC_ALL of ["C", "C.UTF-8"]) {
    hit(run(diff("a.log", "+see /tmp/claude-1000/-no-such-home-jos--repo/x"), { HOME: "/no-such-home/josé", LC_ALL }), `accented, ${LC_ALL}`);
    hit(run(diff("a.log", "+see -no-such-home----repo"), { HOME: "/no-such-home/山田", LC_ALL }), `CJK, ${LC_ALL}`);
    hit(run(diff("a.log", "+see -no-such-home-a---repo"), { HOME: "/no-such-home/a😀", LC_ALL }), `emoji, ${LC_ALL}`);
    const inside = { HOME: "/no-such-home/a😀b", LC_ALL };
    hit(run(diff("a.log", "+see -no-such-home-a--b-repo"), inside), `emoji inside, ${LC_ALL}`);
    assert.equal(run(diff("a.log", "+see -no-such-home-a-b-repo"), inside).status, 0, `one - for an emoji, ${LC_ALL}`);
  }
  hit(run(diff("a.log", "+see C--Users-Jos--repo"), { USERPROFILE: "C:\\Users\\José" }), "non-ASCII USERPROFILE");
  assert.equal(run(diff("a.log", "+C---Users--tester-repo"), {
    HOME: "/", USERPROFILE: "C:\\Users\\tester",
  }).status, 0, "a JSON-doubled literal is not another dashed identity");
  // An alphanumeric character immediately before the dashed identity keeps it inside a word.
  assert.equal(run(diff("a.css", "+.manage-no-such-users-admin-panel, .v2-no-such-users-admin {}"), { HOME: "/no-such-users/admin" }).status, 0);
  assert.equal(run(diff("a.md", "+See the take-no-such-home-dev-guide."), { HOME: "/no-such-home/dev" }).status, 0);
  // The resolved form too: Claude Code may name the folder by it.
  fs.symlinkSync(home, path.join(tmp, "dash-link"));
  hit(run(diff("a.log", `+see ${dashed(home)}-x`), { HOME: path.join(tmp, "dash-link") }), "resolved");
});

test("checks dashed as-set homes with doubled backslashes and a different resolved spelling", () => {
  const absent = "/no-such-home/we\\\\ird/tester";
  hit(run(diff("a.log", `+${dashed(absent)}-repo`), { HOME: absent }), "absent HOME as set");
  const alias = path.join(tmp, "dash-we\\\\ird");
  fs.symlinkSync(home, alias);
  hit(run(diff("a.log", `+${dashed(alias)}-repo`), { HOME: alias }), "symlinked HOME as set");
  const profile = "C:\\\\\\Users\\\\\\tester";
  hit(run(diff("a.log", `+${dashed(profile)}-repo`), {
    HOME: "/", USERPROFILE: profile,
  }), "USERPROFILE as set");
});

test("refuses non-Unicode identity bytes rather than silently dropping them from dashed forms", () => {
  for (const byte of ["200", "361", "366"]) {
    const r = spawnSync("bash", ["-c",
      `USERPROFILE=$'C:\\\\Users\\\\a\\${byte}b' exec bash "$1"`, "test", script], {
      input: diff("f.txt", "+C--Users-a-b-repo"), cwd: tmp, encoding: "utf8",
      env: envFor({ HOME: "/" }),
    });
    assert.equal(r.status, 1, r.stderr);
    assert.match(r.stderr, /cannot derive home spellings/);
  }
});

test("checks a POSIX home whose own name holds a backslash before a slash, also with the slash repeated", () => {
  const base = path.join(tmp, "slash-home");
  fs.mkdirSync(base);
  const weird = path.join(base, "we\\");
  fs.mkdirSync(weird);
  const literal = path.join(weird, "home");
  fs.mkdirSync(literal);
  const combined = literal.replace("\\/", "\\//");
  const lines = [`+see ${literal}/x`, `+see ${combined}/x`];
  const stdin = run(diff("a.js", ...lines), { HOME: literal });
  hit(stdin, "stdin");
  assert.match(stdin.stderr, /\n {5}a\.js:2\n/);
  const { dir: repo, git, commit } = gitRepo("slash-home-repo");
  fs.writeFileSync(path.join(repo, "f.txt"), `${lines.join("\n")}\n`);
  git("add", "-A");
  const staged = run("", { HOME: literal }, { args: ["--staged"], cwd: repo });
  hit(staged, "--staged");
  assert.match(staged.stderr, /\n {5}f\.txt:2\n/);
  const sha = commit(`${lines.join("\n")}\n`);
  const published = run("", { HOME: literal }, { args: publishedArgs(sha), cwd: repo });
  hit(published, "--published");
  assert.match(published.stderr, /\n {5}(?:[0-9a-f]+ )?f\.txt:2\n/);
});

test("checks squeezed, dashed and filename spellings of HOME with repeated slashes", () => {
  const real = path.join(tmp, "as-set-real");
  fs.mkdirSync(real);
  fs.mkdirSync(path.join(real, "home"));
  const link = path.join(tmp, "as-set-link");
  fs.symlinkSync(real, link);
  const asSet = `${link}//home`;
  hit(run(diff("a.js", `+see ${asSet}/x`), { HOME: asSet }), "squeezed");
  hit(run(diff("a.log", `+see ${dashed(asSet)}-repo`), { HOME: asSet }), "dashed as set");
  const report = run(diff(`foo${asSet}/f.txt`, `+see ${asSet}/x`), { HOME: asSet });
  hit(report, "filename as set");
  assert.ok(report.stderr.includes("     foo<home>/f.txt:1\n"), report.stderr);
});

test("preserves literal doubled backslashes while squeezing a symlinked HOME's slashes", () => {
  const parent = path.join(tmp, "combined-we\\\\ird");
  fs.mkdirSync(parent);
  fs.symlinkSync(home, path.join(parent, "alias"));
  const asSet = `${parent}////alias`;
  const slashOnly = `${parent}/alias`;
  hit(run(diff("f.txt", `+${asSet}/x`), { HOME: asSet }));
  hit(run(diff("f.txt", `+${dashed(slashOnly)}-repo`), { HOME: asSet }));
  const report = run(diff(`foo${asSet}/f.txt`, `+${asSet}/x`), { HOME: asSet });
  hit(report);
  assert.ok(report.stderr.includes("     foo<home>/f.txt:1\n"), report.stderr);
});

test("redacts the longest recognized home when two start at the same place", () => {
  const r = run(diff("foo/C:/Users/anna/f.txt", "+see C:/Users/ann/x"), {
    HOME: "C:/Users/ann",
    USERPROFILE: "C:\\Users\\anna",
  });
  hit(r);
  assert.ok(r.stderr.includes("     foo/<home>/f.txt:1\n"), r.stderr);
});

test("redacts overlapping as-set and resolved homes even when they start at different places", () => {
  const resolved = path.join(tmp, "overlap-real", "alice");
  // The link ends with the resolved home's parent, so their spellings overlap in the name.
  const link = path.join(tmp, "overlap-link") + path.dirname(resolved);
  fs.mkdirSync(resolved, { recursive: true });
  fs.mkdirSync(path.dirname(link), { recursive: true });
  fs.symlinkSync(resolved, link);
  const fixtures = [
    { file: `fixtures${link}/alice/f.txt`, report: "fixtures<home>/f.txt" },
    { file: `fixtures${link.toUpperCase()}/alice/f.txt`, report: "fixtures<home>/f.txt" },
    { file: `fixtures/${dashed(link)}-alice/f.txt`, report: "fixtures/<home>/f.txt" },
  ];
  for (const { file, report } of fixtures) {
    const r = run(diff(file, `+see ${resolved}/x`), { HOME: link });
    hit(r, file);
    assert.ok(r.stderr.includes(`     ${report}:1\n`), r.stderr);
    assert.ok(!r.stderr.includes("alice"), r.stderr);
  }
});

test("redacts chains of overlapping home occurrences and keeps separate spans separate", () => {
  const h = "/no-such-home/no-such-home";
  const fixtures = [
    { file: `foo${h}/no-such-home/no-such-home/f.txt`, report: "foo<home>/f.txt" },
    { file: `foo/${dashed(h)}-no-such-home-no-such-home/f.txt`, report: "foo/<home>/f.txt" },
    { file: `foo${h}/spacer${h}/f.txt`, report: "foo<home>/spacer<home>/f.txt" },
  ];
  for (const { file, report } of fixtures) {
    const r = run(diff(file, `+see ${h}/x`), { HOME: h });
    hit(r, file);
    assert.ok(r.stderr.includes(`     ${report}:1\n`), r.stderr);
  }
});

test("a home of one segment names nobody, so it is not looked for", () => {
  const lines = ["+cd /no-such-root/x", "+cd //no-such-root/x", "+cd C:\\app\\x", "+cd C:\\\\app\\x", "+cd -no-such-root-x", "+cd C--app-x"];
  for (const env of [{ HOME: "/no-such-root" }, { HOME: "//no-such-root/", USERPROFILE: "C:\\app" }, { HOME: "/", USERPROFILE: "C:\\\\app" }, { HOME: "C:\\\\app" }]) {
    const r = run(diff("a.sh", ...lines), env);
    assert.equal(r.status, 0);
    assert.match(r.stderr, /name no home directory/);
  }
  // Each form by itself: a one-segment HOME that resolves to a longer path is looked for
  // there. A one-segment folder sits directly under /, where only root can make one, so a
  // relative HOME stands in for it.
  fs.symlinkSync(home, path.join(tmp, "short"));
  const r = run(diff("a.js", `+'${home}/repo'`), { HOME: "short" });
  hit(r);
  assert.doesNotMatch(r.stderr, /name no home directory/);
  // And the other way: a longer HOME that resolves to one segment is looked for only as written.
  const top = ["/tmp", "/usr", "/var"].find((d) => fs.existsSync(d) && fs.realpathSync(d) === d);
  assert.ok(top, "a folder directly under / that is not a symlink");
  fs.symlinkSync(top, path.join(tmp, "to-top"));
  assert.deepEqual(run(diff("a.sh", `+ls ${top}/x`), { HOME: path.join(tmp, "to-top") }), { status: 0, stderr: "" });
});

test("refuses a HOME it cannot resolve, and warns when there is no home to look for", () => {
  const dangling = path.join(tmp, "dangling");
  fs.symlinkSync(path.join(tmp, "nowhere"), dangling);
  const loop = path.join(tmp, "loop");
  fs.symlinkSync(path.join(tmp, "loop2"), loop);
  fs.symlinkSync(loop, path.join(tmp, "loop2"));
  for (const h of [dangling, loop, "dangling"]) { // the last one relative: one segment
    const r = run(diff("a.txt", "+clean"), { HOME: h });
    assert.equal(r.status, 1, h);
    assert.match(r.stderr, /cannot resolve HOME.*symlink/);
  }
  const file = path.join(tmp, "a-file");
  fs.writeFileSync(file, "");
  fs.symlinkSync(file, path.join(tmp, "to-file")); // a symlink, but not a dangling one
  for (const h of [file, path.join(tmp, "to-file")]) {
    assert.match(run(diff("a.txt", "+clean"), { HOME: h }).stderr, /HOME: not a folder this user can enter/, h);
  }
  if (process.getuid?.() !== 0) { // root enters any directory
    const locked = path.join(tmp, "locked");
    fs.mkdirSync(locked, { mode: 0o000 });
    try {
      assert.match(run(diff("a.txt", "+clean"), { HOME: locked }).stderr, /HOME: not a folder this user can enter/, "no permission");
      // Under a folder that cannot be searched, a missing home cannot be told from a hidden one.
      const r = run(diff("a.txt", "+clean"), { HOME: path.join(locked, "tester") });
      assert.equal(r.status, 1, "under no permission");
      assert.match(r.stderr, /above it is not a folder/);
    } finally {
      fs.chmodSync(locked, 0o700); // so the cleanup can remove it
    }
  }
  // Under a dangling symlink the same holds; under missing folders the home is just absent.
  const under = run(diff("a.txt", "+clean"), { HOME: path.join(dangling, "tester") });
  assert.equal(under.status, 1, "under a dangling symlink");
  assert.match(under.stderr, /above it is not a folder/);
  assert.ok(!under.stderr.includes(tmp), "no message names the home it could not resolve");
  const absent = run(diff("a.txt", "+clean"), { HOME: "/no-such-home/tester" });
  assert.deepEqual(absent, { status: 0, stderr: "" }, "two missing levels");
  for (const env of [{ HOME: undefined }, { HOME: "/" }, { HOME: "/", USERPROFILE: "C:\\" }]) {
    const r = run(diff("a.txt", "+clean"), env);
    assert.equal(r.status, 0);
    assert.match(r.stderr, /name no home directory/);
  }
  // USERPROFILE alone is a home to look for.
  const windows = run(diff("a.txt", "+clean"), { HOME: "/", USERPROFILE: "C:\\Users\\tester" });
  assert.deepEqual(windows, { status: 0, stderr: "" });
});

test("keeps Node and canonical-pwd failure details out of diagnostics", () => {
  for (const [tool, message] of [["node", "cannot derive home spellings"], ["pwd", "cannot resolve HOME"]]) {
    const startup = path.join(tmp, `private-${tool}-error.bash`);
    fs.writeFileSync(startup, `${tool}() { echo "$HOME/private" >&2; return 1; }\n`);
    const r = run("", { BASH_ENV: startup });
    assert.equal(r.status, 1, tool);
    assert.ok(r.stderr.includes(message), r.stderr);
    assert.ok(!r.stderr.includes(home) && !r.stderr.includes("/private"), r.stderr);
  }
});

test("refuses an ancestor cd failure at the filesystem root", () => {
  const startup = path.join(tmp, "unenterable-root.bash");
  fs.writeFileSync(startup, 'cd() { [ "$1" != / ] && builtin cd "$@"; }\n');
  const r = run("", { HOME: "/no-such-home/tester", BASH_ENV: startup });
  assert.equal(r.status, 1, r.stderr);
  assert.match(r.stderr, /above it is not a folder this user can search/);
});

test("drains large stdin even without a qualifying identity", () => {
  const r = run(diff("drain.txt", ...Array(20000).fill("+" + "x".repeat(127))), { HOME: "/" });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stderr, /name no home directory/);
});

test("checks the resolved form of a symlinked HOME", () => {
  const link = path.join(tmp, "link");
  fs.symlinkSync(home, link);
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: link }));
  // A relative HOME is resolved from the current folder, not from CDPATH.
  fs.mkdirSync(path.join(tmp, "elsewhere", "tester"), { recursive: true });
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: "tester", CDPATH: path.join(tmp, "elsewhere") }), "CDPATH");
  // Nor a drive path, which gets no ./ because it is absolute on Git Bash: here it is relative.
  fs.mkdirSync(path.join(tmp, "elsewhere", "C:", "Users"), { recursive: true });
  fs.symlinkSync(home, path.join(tmp, "elsewhere", "C:", "Users", "x"));
  const drive = run(diff("a.js", `+'${home}/repo'`), { HOME: "C:/Users/x", CDPATH: path.join(tmp, "elsewhere") });
  assert.deepEqual(drive, { status: 0, stderr: "" }, "a drive path and CDPATH");
  // Nor is a HOME of - read as cd's "the previous folder".
  fs.symlinkSync(home, path.join(tmp, "-"));
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: "-" }), "a HOME of -");
  // A backslash is an ordinary character in a POSIX file name, so HOME is resolved as it is
  // set: a squeezed doubled backslash would name a folder that does not exist, and the
  // resolved form would be lost.
  const doubled = path.join(tmp, "we\\\\ird");
  fs.symlinkSync(home, doubled);
  hit(run(diff("a.js", `+'${home}/repo'`), { HOME: doubled }), "a doubled backslash in HOME's own name");
});

test("HOME's ancestor check does not search CDPATH for a drive path on POSIX", () => {
  const cwd = path.join(tmp, "ancestor-cdpath");
  const elsewhere = path.join(cwd, "elsewhere");
  fs.mkdirSync(path.join(elsewhere, "C:"), { recursive: true });
  fs.writeFileSync(path.join(cwd, "C:"), "not a directory");
  const r = run(diff("a.txt", "+clean"), {
    HOME: "C:/Users/alice", CDPATH: elsewhere,
  }, { cwd });
  assert.deepEqual(r, {
    status: 1,
    stderr: "❌ [home-path] cannot resolve HOME: something above it is not a folder this user can search.\n",
  });
});

test("the locale changes neither the case folding nor the parse", () => {
  // A Turkish locale folds I to a dotless i. Where it is not installed, awk falls back to C.
  const turkish = { HOME: "/no-such-home/TESTERI", LC_ALL: "tr_TR.UTF-8" };
  hit(run(diff("a.sh", "+ls /no-such-home/testeri/x"), turkish), "Turkish");
  // A diff is bytes: a line that is not UTF-8 must not derail the lines after it.
  const bytes = Buffer.concat([Buffer.from(diff("a.bin", "+clean")), Buffer.from("+"), Buffer.from([0xff, 0xfe]), Buffer.from(` ${home}/x\n`)]);
  // GNU tr tolerates these bytes in UTF-8; this double also enforces BSD tr's byte-locale contract.
  const strictTr = path.join(tmp, "strict-tr.sh");
  fs.writeFileSync(strictTr, 'tr() { [ "${LC_ALL:-}" = C ] || return 1; command tr "$@"; }\n');
  const localeList = spawnSync("locale", ["-a"], { encoding: "utf8" });
  const utf8 = localeList.status === 0 && localeList.stdout.split(/\r?\n/).find(name => /utf-?8$/i.test(name));
  const byteEnv = { LC_ALL: utf8 || "POSIX", BASH_ENV: strictTr };
  assert.equal(run(Buffer.concat([Buffer.from(diff("clean.bin", "+clean")), Buffer.from([255, 254])]), byteEnv).status, 0);
  const r = run(bytes, byteEnv);
  hit(r);
  assert.ok(r.stderr.includes("     a.bin:2\n") && !r.stderr.includes("warning"), r.stderr);
  // bash drops a NUL with a warning naming the script's path, which can hold the home.
  const nul = run(diff("a.txt", "+a\0b", `+${home}/x`));
  hit(nul);
  assert.ok(nul.stderr.includes("     a.txt:2\n") && !nul.stderr.includes("warning"), nul.stderr);
});

test("strips NUL bytes embedded inside a home identity", () => {
  const interrupted = home.slice(0, -1) + "\0" + home.slice(-1);
  const r = run(diff("nul.txt", `+${interrupted}/x`));
  hit(r);
  assert.ok(r.stderr.includes("     nul.txt:1\n"), r.stderr);
});

test("a HOME of / checks nothing, and a trailing slash on HOME is ignored", () => {
  assert.equal(run(diff("a.sh", "+cd / && ls"), { HOME: "/" }).status, 0);
  // Before bash 5.3, `cd ""` stays in the current directory; the script must not take that
  // for the home.
  const work = path.join(tmp, "work");
  fs.mkdirSync(work);
  assert.equal(run(diff("a.sh", `+ls ${work}/x`), { HOME: "/" }, { cwd: work }).status, 0);
  hit(run(diff("a.sh", "+ls /no-such-home/tester/x"), { HOME: "/no-such-home/tester/" }));
});

test("--published checks incoming history against the destination baseline, past hostile git settings", () => {
  const { dir: repo, git, commit } = gitRepo("repo", { hostile: true });
  git("remote", "add", "origin", path.join(tmp, "baseline.git"));
  const base = commit("base\n");
  const leak = commit(`${home}/secret\n`);
  const fix = commit("clean\n");
  // From a subfolder, where diff.relative would drop f.txt.
  fs.mkdirSync(path.join(repo, "sub"));
  const published = (env = {}) => run("", env, { args: publishedArgs(fix), cwd: path.join(repo, "sub") });

  // The leak is in history even though the tip removed it.
  git("update-ref", "refs/remotes/origin/main", base);
  const r = published();
  hit(r);
  assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt:1\n`), r.stderr);
  // Already public: not this push's to refuse.
  git("update-ref", "refs/remotes/origin/main", leak);
  assert.equal(published().status, 0);
  // With nothing published yet, the first commit counts too.
  git("update-ref", "-d", "refs/remotes/origin/main");
  git("checkout", "-q", "--orphan", "fresh");
  const first = commit(`${home}/secret\n`);
  const fresh = run("", {}, { args: publishedArgs(first), cwd: repo });
  hit(fresh);
  assert.ok(fresh.stderr.includes(`${first.slice(0, 12)} f.txt:1\n`), fresh.stderr);
  // A sha git cannot read, SHA-1 or SHA-256, fails closed, with only the script's own line:
  // git's error could name a path.
  for (const unknown of ["0".repeat(40), "0".repeat(64)]) {
    assert.deepEqual(run("", {}, { args: publishedArgs(unknown), cwd: repo }), {
      status: 1, stderr: "❌ [home-path] cannot list the commits to check (git log says why).\n",
    });
  }
  // A ref or path, whose text git's own error would print, gets the usage line, and so does
  // a short id, a missing one, an extra argument, or a sha without --published, which would
  // otherwise wait on stdin.
  const usage = { status: 1, stderr: "❌ [home-path] usage: home-path.sh [--staged | --published <old> <new> [remote]]\n" };
  for (const args of [
    ["--published", `${home}/bad-ref`], ["--published", fix.slice(0, 12)], ["--published"],
    ["--published", fix, "x"], ["--published", `${home}/bad-ref`, fix],
    ["--published", "0", fix.slice(0, 12)], ["--published", "0", fix, "origin", "extra"],
    ...["00", "0a", "x" + fix, fix + "x", "a".repeat(39), "a".repeat(41), "a".repeat(63), "a".repeat(65), "A".repeat(40)]
      .flatMap(invalid => [["--published", invalid, fix], ["--published", "0", invalid]]),
    [fix], ["--staged", "x"],
  ]) {
    assert.deepEqual(run("", {}, { args, cwd: repo }), usage, args.join(" "));
  }
});

test("--published accepts SHA-256 old and new endpoints", () => {
  const { dir, commit } = gitRepo("sha256", { objectFormat: "sha256" });
  const base = commit("base\n");
  const leak = commit(`${home}/secret\n`);
  const tip = commit("clean again\n");
  assert.equal(base.length, 64);
  assert.equal(tip.length, 64);
  for (const old of [base, "0".repeat(64)]) {
    const r = run("", {}, { args: publishedArgs(tip, old), cwd: dir });
    hit(r);
    assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt:1\n`), r.stderr);
  }
});

test("baseline lookup rejects paths and noncommit refs and hides Git failure details", () => {
  const { dir, git, commit } = gitRepo("baseline-lookup");
  git("remote", "add", "origin", path.join(tmp, "baseline-lookup.git"));
  const file = "refs/remotes/origin/main^{commit}";
  fs.mkdirSync(path.join(dir, path.dirname(file)), { recursive: true });
  fs.writeFileSync(path.join(dir, file), "a file, not a revision\n");
  const base = commit("clean\n");
  assert.deepEqual(run("", {}, { args: publishedArgs(base), cwd: dir }), { status: 0, stderr: "" });
  const leak = commit(`${home}/x\n`);
  hit(run("", {}, { args: publishedArgs(leak), cwd: dir }));
  const blob = git("hash-object", "-w", "f.txt");
  git("update-ref", "refs/remotes/origin/main", blob);
  assert.deepEqual(run("", {}, { args: publishedArgs(base), cwd: dir }), { status: 0, stderr: "" });
  git("update-ref", "refs/remotes/origin/main", leak);
  git("replace", "-f", leak, blob);
  assert.deepEqual(run("", {}, { args: publishedArgs(leak), cwd: dir }), { status: 0, stderr: "" },
    "a type-changing local replacement cannot invalidate a trusted original baseline");
  const startup = path.join(tmp, "private-baseline-error.bash");
  fs.writeFileSync(startup, 'git() { if [ "${2:-}" = rev-parse ]; then echo "$HOME/private" >&2; return 1; fi; command git "$@"; }\n');
  assert.deepEqual(run("", { BASH_ENV: startup }, { args: publishedArgs(base), cwd: dir }), { status: 0, stderr: "" });
});

test("--staged fails closed when git cannot read the index, with only the script's own line", () => {
  // git's error names the path it was given, which could hold the home.
  assert.deepEqual(run("", { GIT_DIR: path.join(home, "no-repo") }, { args: ["--staged"] }), {
    status: 1, stderr: "❌ [home-path] cannot read the staged diff (git diff --cached says why).\n",
  });
});

test("refuses input, NUL-stripper and scanner failures with its own diagnostic", () => {
  for (const [tool, body, message] of [
    ["cat", "return 1", "cannot read the diff."],
    ["tr", "command cat > /dev/null; return 1", "cannot scan the diff."],
    ["awk", "return 2", "cannot scan the diff."],
  ]) {
    const startup = path.join(tmp, `failing-${tool}.bash`);
    fs.writeFileSync(startup, `${tool}() { ${body}; }\n`);
    assert.deepEqual(run("", { BASH_ENV: startup }), {
      status: 1, stderr: `❌ [home-path] ${message}\n`,
    }, tool);
  }
});

test("refuses report output failure", { skip: process.platform !== "linux" && "/dev/full is Linux-only" }, () => {
  const output = fs.openSync("/dev/full", "w");
  try {
    const r = spawnSync("bash", [script], {
      input: diff("f.txt", `+${home}/x`), cwd: tmp, env: envFor({}), stdio: ["pipe", "ignore", output],
    });
    assert.equal(r.status, 1);
  } finally { fs.closeSync(output); }
});

test("staged and published reports stream clean input and many hits", () => {
  const { dir, git, commit } = gitRepo("many-reports");
  const base = commit("base\n");
  const content = Array(1000).fill(`${home}/x`).join("\n") + "\n";
  fs.writeFileSync(path.join(dir, "f.txt"), content);
  git("add", "-A");
  const staged = run("", {}, { args: ["--staged"], cwd: dir });
  hit(staged);
  assert.ok(staged.stderr.includes("f.txt:1000\n"), staged.stderr);
  git("commit", "-q", "-m", "many hits");
  const tip = git("rev-parse", "HEAD");
  const published = run("", {}, { args: publishedArgs(tip, base), cwd: dir });
  hit(published);
  assert.ok(published.stderr.includes(`${tip.slice(0, 12)} f.txt:1000\n`), published.stderr);
  assert.equal(staged.stderr.split("these added lines").length - 1, 1);
  assert.equal(published.stderr.split("these added lines").length - 1, 1);
  assert.deepEqual(run("", {}, { args: ["--staged"], cwd: dir }), { status: 0, stderr: "" });
  assert.deepEqual(run("", {}, { args: publishedArgs(tip, tip), cwd: dir }), { status: 0, stderr: "" });
});

test("--published scans fetched add/remove history against the destination, including for new refs", () => {
  const { dir, git, commit } = gitRepo("fetched-history");
  git("remote", "add", "origin", path.join(tmp, "public.git"));
  git("remote", "add", "private", path.join(tmp, "private.git"));
  const base = commit("clean\n");
  assert.deepEqual(run("", {}, { args: ["--published", "0", base], cwd: dir }), { status: 0, stderr: "" },
    "a clean new ref without a named remote scans from the root");
  const leak = commit(`${home}/secret\n`);
  const tip = commit("clean again\n");
  git("update-ref", "refs/remotes/origin/main", base);
  git("update-ref", "refs/remotes/origin/cloud-topic", tip);
  git("update-ref", "refs/remotes/private/main", tip);
  git("update-ref", "refs/remotes/unconfigured/main", tip);
  for (const args of [
    ["--published", "0", tip],
    publishedArgs(tip, base),
    publishedArgs(tip),
    publishedArgs(tip, "0", "unconfigured"),
    publishedArgs(tip, "0", `${home}/unknown-remote`),
  ]) {
    const r = run("", {}, { args, cwd: dir });
    hit(r, args.join(" "));
    assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt:1\n`), r.stderr);
    assert.ok(!r.stderr.includes(home), r.stderr);
  }
  assert.equal(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }).status, 0,
    "only the named destination's main is a trusted baseline for a new ref");
  git("remote", "add", "--", "--all", path.join(tmp, "private.git"));
  git("update-ref", "refs/remotes/--all/main", leak);
  assert.deepEqual(run("", {}, { args: publishedArgs(tip, "0", "--all"), cwd: dir }), { status: 0, stderr: "" },
    "an option-like remote still supplies its trusted main");
  assert.equal(run("", {}, { args: publishedArgs(tip, tip), cwd: dir }).status, 0,
    "an unchanged destination has no incoming commits");
  git("config", "remote.private.pushurl", path.join(tmp, "elsewhere.git"));
  hit(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }),
    "a private fetch URL cannot authorize a different push destination");
  git("config", "--unset", "remote.private.pushurl");
  git("config", "--add", "remote.private.pushurl", path.join(tmp, "private.git"));
  git("config", "--add", "remote.private.pushurl", path.join(tmp, "another.git"));
  hit(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }),
    "multiple push URLs do not share a trusted baseline");
  git("config", "--unset-all", "remote.private.pushurl");
  git("config", "remote.private.pushurl", `${path.join(tmp, "private.git")}\n`);
  hit(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }),
    "a push URL's trailing newline must not disappear during comparison");
  git("config", "remote.private.url", `${path.join(tmp, "private.git")}\n`);
  git("config", "remote.private.pushurl", path.join(tmp, "private.git"));
  hit(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }),
    "a URL's trailing newline must not disappear during comparison");
  git("config", "remote.private.pushurl", `${path.join(tmp, "private.git")}\n`);
  hit(run("", {}, { args: publishedArgs(tip, "0", "private"), cwd: dir }),
    "matching URLs containing a newline cannot authorize a baseline");
  const missingBase = run("", {}, { args: publishedArgs(tip, "f".repeat(40)), cwd: dir });
  assert.equal(missingBase.status, 1);
  assert.match(missingBase.stderr, /cannot list the commits/);
});

test("--published reads the objects a push sends, past git replace, grafts and NUL bytes", () => {
  const { dir: repo, git, commit } = gitRepo("replaced");
  const base = commit("base\n");
  const leak = commit(`${home}/secret\n`);
  // Text past git's 8000-byte binary sniff may hold a NUL.
  const fix = commit(`${"x".repeat(9000)}\0\nclean\n`);
  git("update-ref", "refs/remotes/origin/main", base);
  const published = () => run("", {}, { args: publishedArgs(fix), cwd: repo });
  // A replace ref or a graft that makes base the cleanup's parent would hide the leak from
  // a plain git log; the push still sends it.
  git("replace", "--graft", fix, base);
  let r = published();
  hit(r, "git replace");
  assert.ok(r.stderr.includes(`${leak.slice(0, 12)} f.txt:1\n`) && !r.stderr.includes("warning"), r.stderr);
  git("replace", "-d", fix);
  fs.writeFileSync(path.join(repo, ".git", "info", "grafts"), `${fix} ${base}\n`);
  r = published();
  hit(r, "info/grafts");
});

test("--staged and --published read a file git takes for binary as text", () => {
  const { dir: repo, git, commit } = gitRepo("binary");
  git("config", "core.hooksPath", path.dirname(script));
  fs.writeFileSync(path.join(repo, ".gitattributes"), "*.dat -diff\n");
  git("update-ref", "refs/remotes/origin/main", commit("base\n"));
  // By an attribute, or by a NUL in its first 8000 bytes: git would print only that the
  // binary files differ.
  for (const file of ["a.dat", "b.bin"]) {
    fs.writeFileSync(path.join(repo, file), `x\0y\n${home}/secret\n`);
    git("add", "-A");
    const staged = sh(repo, "git", ["commit", "-q", "-m", "c"]);
    assert.equal(staged.status, 1, staged.out);
    // Without tr, bash's warning about the NUL would name the script's path.
    assert.ok(staged.out.includes(`     ${file}:2\n`) && !staged.out.includes("warning"), staged.out);
    git("commit", "-q", "--no-verify", "-m", "c");
    const sha = git("rev-parse", "HEAD");
    const r = run("", {}, { args: publishedArgs(sha), cwd: repo });
    hit(r, file);
    assert.ok(r.stderr.includes(`${sha.slice(0, 12)} ${file}:2\n`), r.stderr);
  }
});

test("--published checks what a merge resolution adds, and nothing a parent already has", () => {
  const { dir: repo, git, commit: commitTo } = gitRepo("merge", { hostile: true });
  git("remote", "add", "origin", path.join(tmp, "merge.git"));
  fs.mkdirSync(path.join(repo, "b")); // a folder named like git's b/ prefix, to keep in the report
  const commit = (content) => commitTo(content, path.join("b", "f.txt"));
  commit("1\n2\n3\n4\n5\n");
  git("checkout", "-q", "-b", "side");
  // Published already, so not this push's to refuse, though the merge brings it in.
  git("update-ref", "refs/remotes/origin/main", commit(`1\n${home}/old\n3\nside\n5\n`));
  git("checkout", "-q", "main");
  commit("1\n2\n3\n4\nmain\n");
  sh(repo, "git", ["merge", "-q", "--no-commit", "side"]); // conflicts, which the resolution settles
  // Line 4 replaces a line from each parent, so the hunk's dropped lines come before it.
  fs.writeFileSync(path.join(repo, "b", "f.txt"), `1\n${home}/old\n3\n${home}/new\nmain\n`);
  git("add", "-A");
  git("commit", "-q", "-m", "merge");
  const merge = git("rev-parse", "HEAD");
  const r = run("", {}, { args: publishedArgs(merge), cwd: repo });
  hit(r);
  assert.equal(r.stderr.split("\n").filter((l) => l.startsWith("     ")).join("\n"), `     ${merge.slice(0, 12)} b/f.txt:4`);
});
