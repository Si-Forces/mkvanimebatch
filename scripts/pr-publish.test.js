import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { tmp, home, sh } from "./test-support/home-path.js";

const publisher = fileURLToPath(new URL("pr-publish.js", import.meta.url));
test("PR publisher checks exact captured create/edit bytes and performs no write on refusal or read failure", () => {
  const bin = path.join(tmp, "pr-bin"); fs.mkdirSync(bin);
  const called = path.join(tmp, "published"), payload = path.join(tmp, "payload"), argv = path.join(tmp, "argv");
  fs.writeFileSync(path.join(bin, "gh"), '#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$ARGV"\ncat > "$PAYLOAD"\ntouch "$CALLED"\n[ "$GH_PROMPT_DISABLED" = 1 ] || exit 9\nprintf "https://github.com/Si-Forces/mkvanimebatch/pull/1\\n"\n');
  fs.chmodSync(path.join(bin, "gh"), 0o755);
  const body = path.join(tmp, "pr-body");
  const env = { PATH: bin + path.delimiter + process.env.PATH, CALLED: called, PAYLOAD: payload, ARGV: argv };
  for (const operation of ["create", "edit"]) {
    const prefix = operation === "create" ? ["create", "--title", "prepared", "--head", "topic"] : ["edit", "1"];
    const publish = (extra = [], source = body) => sh(tmp, process.execPath, [publisher, ...prefix, "--body-file", source, ...extra], env);
    for (const invalid of [`${home}/private\n`, `clean\n# ${home}/private\n`]) {
      fs.writeFileSync(body, invalid);
      const refused = publish(); assert.equal(refused.status, 1, refused.out);
      assert.match(refused.out, /pr-body:[12]/); assert.ok(!refused.out.includes(home)); assert.equal(fs.existsSync(called), false);
    }
    assert.equal(publish([], path.join(home, "missing-body")).status, 1);
    assert.equal(fs.existsSync(called), false);
    fs.writeFileSync(body, "clean body\r\n\n\n");
    for (const flags of [["--body", "unchecked"], ["--fill"], ["--editor"], ["--web"], ["--recover", "x"], ["--template", body], ["--body-file", body]]) {
      assert.equal(publish(flags).status, 1); assert.equal(fs.existsSync(called), false);
    }
    const accepted = publish(); assert.equal(accepted.status, 0, accepted.out);
    assert.equal(fs.readFileSync(payload, "utf8"), "clean body\r\n\n\n");
    assert.match(fs.readFileSync(argv, "utf8"), /--body-file\n-\n$/);
    fs.unlinkSync(called);
  }
});

test("PR publication uses its snapshot after the source changes and hides scanner or gh failures", () => {
  const bin = path.join(tmp, "snapshot-bin"); fs.mkdirSync(bin);
  const original = path.join(tmp, "snapshot-source"), payload = path.join(tmp, "snapshot-payload");
  const called = path.join(tmp, "snapshot-called");
  const bash = sh(tmp, "bash", ["-c", "command -v bash"]).out.trim();
  fs.writeFileSync(path.join(bin, "bash"), '#!/bin/sh\nif [ "$FAIL_SCAN" = 1 ]; then echo "$HOME/private" >&2; exit 2; fi\nprintf "%s/private\\n" "$HOME" > "$ORIGINAL"\nexec "$REAL_BASH" "$@"\n');
  fs.writeFileSync(path.join(bin, "gh"), '#!/bin/sh\ncat > "$PAYLOAD"\ntouch "$CALLED"\nif [ "$FAIL_GH" = 1 ]; then echo "$HOME/private"; echo "$HOME/private" >&2; exit 1; fi\necho https://github.com/Si-Forces/mkvanimebatch/pull/2\n');
  for (const file of ["bash", "gh"]) fs.chmodSync(path.join(bin, file), 0o755);
  const env = { PATH: bin + path.delimiter + process.env.PATH, REAL_BASH: bash, ORIGINAL: original, PAYLOAD: payload, CALLED: called };
  const invoke = extra => sh(tmp, process.execPath, [publisher, "create", "--title", "clean", "--head", "topic", "--body-file", original], { ...env, ...extra });
  fs.writeFileSync(original, "captured\n\n");
  assert.equal(invoke({}).status, 0);
  assert.equal(fs.readFileSync(payload, "utf8"), "captured\n\n");
  assert.ok(fs.readFileSync(original, "utf8").includes(home));
  fs.unlinkSync(called); fs.writeFileSync(original, "clean\n");
  const scanError = invoke({ FAIL_SCAN: "1" });
  assert.equal(scanError.status, 1); assert.ok(!scanError.out.includes(home)); assert.equal(fs.existsSync(called), false);
  fs.writeFileSync(original, "clean\n");
  const ghError = invoke({ FAIL_GH: "1" });
  assert.equal(ghError.status, 1); assert.ok(!ghError.out.includes(home));
});

test("PR creation requires nonblank explicit title and head before any publisher call", () => {
  const bin = path.join(tmp, "required-pr-bin"); fs.mkdirSync(bin);
  const marker = path.join(tmp, "required-pr-called"), body = path.join(tmp, "required-pr-body");
  fs.writeFileSync(body, "clean prepared description\n");
  fs.writeFileSync(path.join(bin, "gh"), '#!/bin/sh\ntouch "$CALLED"\ncat > /dev/null\n');
  fs.chmodSync(path.join(bin, "gh"), 0o755);
  const env = { PATH: bin + path.delimiter + process.env.PATH, CALLED: marker };
  for (const fields of [[], ["--head", "topic"], ["--title", "title"],
    ["--title", "", "--head", "topic"], ["--title", " \t", "--head", "topic"],
    ["--title", "title", "--head", ""], ["--title", "title", "--head", " \t"]]) {
    const r = sh(tmp, process.execPath, [publisher, "create", ...fields, "--body-file", body], env);
    assert.equal(r.status, 1, r.out); assert.equal(fs.existsSync(marker), false);
  }
});
