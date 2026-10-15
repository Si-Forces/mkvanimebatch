import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { KIND_CLASS, lineClass, pushCapped } from "./log-classify.js";

test("lineClass maps each Log Channel kind to its console class", () => {
  assert.equal(lineClass("error"), "line-err");
  assert.equal(lineClass("warning"), "line-warn");
  assert.equal(lineClass("skipped"), "line-skip");
  assert.equal(lineClass("succeeded"), "line-ok");
  assert.equal(lineClass("step"), "line-info");
  assert.equal(lineClass("cache"), "line-done");
  assert.equal(lineClass("note"), "");
});

// Drift guard: KIND_CLASS must name every `LogKind` variant, serialized lowercase.
test("KIND_CLASS lists exactly the LogKind names log_line.rs serializes", () => {
  const src = readFileSync(new URL("../../crates/mkv-core/src/log_line.rs", import.meta.url), "utf8");
  const body = src.match(/pub enum LogKind \{([^}]*)\}/)[1];
  const kinds = [...body.matchAll(/^\s*(\w+),/gm)].map(([, name]) => name.toLowerCase());
  assert.deepEqual(kinds.sort(), Object.keys(KIND_CLASS).sort());
});

test("pushCapped keeps the newest max lines across single and batch pushes", () => {
  let queue = [];
  for (const line of ["a", "b", "c"]) queue = pushCapped(queue, line, 4);
  assert.deepEqual(queue, ["a", "b", "c"]);

  queue = pushCapped(queue, ["d", "e"], 4);
  assert.deepEqual(queue, ["b", "c", "d", "e"]);

  queue = pushCapped(queue, "f", 4);
  assert.deepEqual(queue, ["c", "d", "e", "f"]);

  // A batch larger than the cap keeps only its own newest lines.
  queue = pushCapped(queue, ["g", "h", "i", "j", "k"], 4);
  assert.deepEqual(queue, ["h", "i", "j", "k"]);

  // A cap of 0 keeps nothing (slice(-0) would have kept everything).
  assert.deepEqual(pushCapped(["a"], ["b"], 0), []);
});

test("pushCapped leaves the queue it was given unchanged", () => {
  const queue = ["a", "b"];
  assert.deepEqual(pushCapped(queue, ["c", "d"], 3), ["b", "c", "d"]);
  assert.deepEqual(queue, ["a", "b"]);
});
