import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { KIND_CLASS, createRunLogs, lineClass } from "./log-view.js";

// Rendering only: what a Run Log keeps, its error count and its display text are the core's,
// tested in `crates/mkv-desktop-core/src/log.rs`.

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

class El {
  constructor() {
    this.children = [];
    this.scrollHeight = 0;
    this.scrollTop = 0;
    this.clientHeight = 0;
  }
  appendChild(child) {
    for (const node of child.fragment ? child.children : [child]) {
      node.parent = this;
      this.children.push(node);
    }
    return child;
  }
  replaceChildren(...nodes) {
    this.children = [];
    for (const node of nodes) this.appendChild(node);
  }
  get firstElementChild() { return this.children[0] ?? null; }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter(node => node !== this); }
}

/** Run Logs over two console elements, with frames run only by `frame()`. */
function makeLogs() {
  const els = { "remux-log": new El(), "extract-log": new El() };
  const frames = new Map();
  let nextFrame = 0;
  let badges = 0;
  const logs = createRunLogs({
    doc: {
      getElementById: id => els[id] ?? null,
      createElement: () => new El(),
      createDocumentFragment: () => Object.assign(new El(), { fragment: true }),
    },
    onErrors: () => { badges++; },
    requestFrame: callback => { frames.set(++nextFrame, callback); return nextFrame; },
    cancelFrame: id => frames.delete(id),
  });
  return {
    logs,
    shown: mode => els[`${mode}-log`].children.map(line => line.textContent),
    classes: mode => els[`${mode}-log`].children.map(line => line.className),
    frame() {
      const pending = [...frames.values()];
      frames.clear();
      for (const callback of pending) callback();
    },
    frames: () => frames.size,
    badges: () => badges,
  };
}

const PROMPT = "No output yet. Run a batch to see its output here.";
const line = (text, kind = "note") => ({ kind, text });
const batch = (mode, first, lines, fields = {}) => ({ mode, generation: 0, first, keep_from: 0, errors: 0, lines, ...fields });

/** Run Logs whose `get_log` answered empty, so batches draw at once. */
function loaded() {
  const page = makeLogs();
  for (const mode of ["remux", "extract"]) page.logs.load(mode, { mode, generation: 0, first: 0, errors: 0, lines: [] });
  return page;
}

test("a batch draws in its own mode's console on the next frame, its display text over its raw text", () => {
  const page = loaded();
  assert.deepEqual(page.shown("remux"), [PROMPT]);
  page.logs.apply(batch("extract", 0, [{ kind: "succeeded", text: "/a/b.mkv", display: "a › b.mkv" }, line("ERROR: x", "error")], { errors: 1 }));
  assert.deepEqual(page.shown("extract"), [PROMPT], "nothing draws before the frame");
  page.frame();
  assert.deepEqual(page.shown("extract"), ["a › b.mkv", "ERROR: x"]);
  assert.deepEqual(page.classes("extract"), ["log-line line-ok", "log-line line-err"]);
  assert.deepEqual(page.shown("remux"), [PROMPT]);
  assert.equal(page.logs.errors("extract"), 1);
  assert.equal(page.logs.errors("remux"), 0);
  // Copy reads the raw text, drawn or not, with no call to the core.
  page.logs.apply(batch("extract", 2, [line("queued")], { errors: 1 }));
  assert.equal(page.logs.text("extract"), "/a/b.mkv\nERROR: x\nqueued");
});

test("a new generation clears the console, the lines waiting for a frame and that frame", () => {
  const page = loaded();
  page.logs.apply(batch("remux", 0, [line("one")]));
  page.frame();
  page.logs.apply(batch("remux", 1, [line("waiting", "error")], { errors: 1 }));
  assert.equal(page.frames(), 1);
  page.logs.apply(batch("remux", 0, [], { generation: 1 }));
  assert.equal(page.frames(), 0, "the pending frame is cancelled");
  assert.deepEqual(page.shown("remux"), [PROMPT]);
  assert.equal(page.logs.text("remux"), "");
  assert.equal(page.logs.errors("remux"), 0);
  // An older generation's batch is dropped.
  page.logs.apply(batch("remux", 2, [line("stale")]));
  page.frame();
  assert.deepEqual(page.shown("remux"), [PROMPT]);
  page.logs.apply(batch("remux", 0, [line("fresh")], { generation: 1 }));
  page.frame();
  assert.deepEqual(page.shown("remux"), ["fresh"]);
});

test("lines below keep_from leave the console and the lines waiting for a frame", () => {
  const page = loaded();
  page.logs.apply(batch("remux", 0, [line("a"), line("b"), line("c")]));
  page.frame();
  page.logs.apply(batch("remux", 3, [line("d"), line("e")], { keep_from: 2 }));
  page.frame();
  assert.deepEqual(page.shown("remux"), ["c", "d", "e"]);
  // A batch over the core's cap keeps only its own newest, so the drawn lines and the older
  // waiting ones all go.
  page.logs.apply(batch("remux", 5, [line("f")], { keep_from: 3 }));
  page.logs.apply(batch("remux", 9, [line("j"), line("k")], { keep_from: 9 }));
  page.frame();
  assert.deepEqual(page.shown("remux"), ["j", "k"]);
  assert.equal(page.logs.text("remux"), "j\nk");
});

test("batches sent before get_log answered wait for it, and the lines it already holds draw once", () => {
  const page = makeLogs();
  page.logs.apply(batch("remux", 1, [line("b"), line("c")], { errors: 0 }));
  page.logs.apply(batch("remux", 3, [line("d", "error")], { errors: 1 }));
  page.frame();
  assert.deepEqual(page.shown("remux"), [PROMPT], "nothing draws before get_log");
  const badges = page.badges();
  page.logs.load("remux", { mode: "remux", generation: 0, first: 0, errors: 0, lines: [line("a"), line("b"), line("c")] });
  page.frame();
  assert.deepEqual(page.shown("remux"), ["a", "b", "c", "d"]);
  assert.equal(page.logs.errors("remux"), 1);
  assert.ok(page.badges() > badges, "the badge is told");
  // A failed get_log draws the waiting batches alone.
  page.logs.apply(batch("extract", 4, [line("x")]));
  page.logs.load("extract", null);
  page.frame();
  assert.deepEqual(page.shown("extract"), ["x"]);
});

test("a waiting batch the get_log view already covers keeps the view's error count", () => {
  const page = makeLogs();
  page.logs.apply(batch("remux", 0, [line("a")], { errors: 0 }));
  page.logs.load("remux", { mode: "remux", generation: 0, first: 0, errors: 1, lines: [line("a"), line("b", "error")] });
  assert.equal(page.logs.errors("remux"), 1);
});
