import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";
import { IDLE_RUN, controls, draft, modeView } from "./fake-core.js";

// Rendering and wiring only: the Job Coordinator, the request a Run builds and its preflight are
// the Desktop Core's, tested in `crates/mkv-desktop-core/src/{coordinator,draft,actor}.rs`, and so
// are the Run Log lines a Run, a Stop or a cache clear writes (`actor/start.rs`, `actor.rs`) and
// the controls' texts and rules (`presentation.rs`, `controls`).

class FakeElement {
  constructor(value = "") {
    this.value = value;
    this.checked = false;
    this.disabled = false;
    this.textContent = "";
    this.className = "";
    this.innerHTML = "";
    this.dataset = {};
    this.style = {};
    this.children = [];
    this.focused = false;
    const classes = new Set();
    this.classList = {
      add: name => classes.add(name),
      remove: name => classes.delete(name),
      contains: name => classes.has(name),
      toggle: (name, on) => (on ? classes.add(name) : classes.delete(name)),
    };
  }

  addEventListener(type, handler) { (this.handlers ??= new Map()).set(type, handler); }
  appendChild(child) { this.children.push(child); return child; }
  querySelector() { return new FakeElement(); }
  remove() {}
  focus() { this.focused = true; }
}

/** Run/Stop/log/status controls for each of `modes`, plus `extra` elements by id. */
function makeElements(modes = ["remux"], extra = {}) {
  const elements = new Map();
  for (const mode of modes) {
    for (const id of ["run", "stop", "log"]) elements.set(`${mode}-${id}`, new FakeElement());
    // The pill's markup: the dot, then the label's text node.
    const status = new FakeElement();
    status.lastChild = { textContent: " Idle" };
    elements.set(`${mode}-status`, status);
  }
  for (const [id, value] of Object.entries(extra)) elements.set(id, value instanceof FakeElement ? value : new FakeElement(value));
  return elements;
}

/** Installs a DOM stub. */
function installDom(t, elements) {
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => {
    globalThis.document = previousDocument;
  });
}

/** A published View State with `fields` over an idle one. */
function viewState(fields = {}) {
  return {
    seq: 1,
    job: null,
    run: { ...IDLE_RUN },
    busy: false,
    defaults: null,
    presets: [],
    remux: modeView(),
    extract: modeView([], {}, "extract"),
    ...fields,
  };
}

/**
 * A runtime of fakes over `page.state`, the View State applied last. `send` answers each Intent
 * with `reply(intent)` and records it in `page.sent`.
 */
function makeRuntime(tracker, { reply = async () => true, ...overrides } = {}) {
  const page = { state: viewState(), sent: [], opened: [] };
  page.runtime = {
    queueFor: () => tracker,
    safeAddListener: (el, event, handler) => el?.addEventListener(event, handler),
    scanInputDirectory: async () => true,
    showModal: async () => true,
    openSettings: mode => page.opened.push(mode),
    send: async intent => {
      page.sent.push(intent);
      return reply(intent);
    },
    state: () => page.state,
    ...overrides,
  };
  return page;
}

/** A queue a controller sets the selection lock on and selects all of. */
const idleTracker = () => ({ selectionLocked: false, selectAll() {}, deselectAll() {} });

/** A View State whose `mode` view publishes `fields` over its idle controls. */
const withControls = (mode, fields, state = {}) => viewState({
  ...state,
  [mode]: modeView([], { controls: controls(mode, fields) }, mode),
});

test("index.html chips name their state without inventing a worker or I/O job count", () => {
  const html = fs.readFileSync(path.join(import.meta.dirname, "index.html"), "utf8");
  const chips = [...html.matchAll(/<span\b[^>]*\bmetric-(?:workers|iojobs)-chip\b[^>]*>([^<]*)<\/span>/g)];
  assert.equal(chips.length, 2, "a Workers and an I/O Jobs chip in the bottom bar");
  for (const [, text] of chips) assert.doesNotMatch(text, /\d/, `chip "${text}" shows a count before the engine reported one`);
});

test("index.html workers and Disk I/O inputs default to auto, with no preset value", () => {
  const html = fs.readFileSync(path.join(import.meta.dirname, "index.html"), "utf8");
  for (const id of ["remux-workers", "extract-workers", "remux-io-jobs", "extract-io-jobs"]) {
    const tag = html.match(new RegExp(`<input\\b[^>]*\\bid="${id}"[^>]*>`))?.[0];
    assert.ok(tag, `${id} input is missing`);
    assert.match(tag, /\bplaceholder="auto"/, `${id} must show the auto placeholder`);
    assert.doesNotMatch(tag, /\svalue=/, `${id} must start empty so the draft holds no value and the engine's default applies`);
  }
});

test("Run and Stop send their Intents, with the mode and nothing else", async t => {
  const elements = makeElements();
  installDom(t, elements);
  const page = makeRuntime(idleTracker());
  new JobController("remux", page.runtime);
  await elements.get("remux-run").handlers.get("click")();
  await elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(page.sent, [{ type: "run", mode: "remux" }, { type: "stop", mode: "remux" }]);
});

test("a confirmed cache clear sends its Intent and writes nothing itself; a refused Stop or clear throws nothing", async t => {
  const clear = new FakeElement();
  const elements = makeElements(["remux"], { "remux-clear-cache": clear });
  installDom(t, elements);
  let confirmed = false;
  const page = makeRuntime(idleTracker(), {
    showModal: async () => confirmed,
    reply: async () => { throw "Permission denied"; },
  });
  new JobController("remux", page.runtime);
  await clear.handlers.get("click")();
  assert.deepEqual(page.sent, [], "a cancelled dialog sends nothing");
  confirmed = true;
  await clear.handlers.get("click")();
  await elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(page.sent, [{ type: "clear_cache", mode: "remux" }, { type: "stop", mode: "remux" }]);
});

test("a Run click locks Run and the settings until its answer, so a second click sends nothing", async t => {
  const elements = makeElements();
  installDom(t, elements);
  let answer;
  const page = makeRuntime(idleTracker(), { reply: () => new Promise(resolve => { answer = resolve; }) });
  const controller = new JobController("remux", page.runtime);
  const field = new FakeElement();
  controller.formControls = [field];
  const preset = new FakeElement();
  globalThis.document.querySelectorAll = selector => (selector.includes(".lock-while-job") ? [preset] : []);
  const runBtn = elements.get("remux-run");

  // Still idle in the published state: the lock is the click's own.
  const running = controller.start();
  assert.equal(runBtn.disabled, true);
  assert.equal(field.disabled, true, "an edit typed after the click cannot reach the core");
  assert.equal(preset.disabled, true, "nor can a preset picked after it");
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true, "a publication before the answer keeps the lock");
  await controller.start();
  assert.deepEqual(page.sent, [{ type: "run", mode: "remux" }]);

  // A refused Run unlocks: the published state, still idle, rules again.
  answer(Promise.reject("A batch is running; the queue is locked until it ends."));
  await running;
  assert.equal(runBtn.disabled, false);
  assert.equal(field.disabled, false);
  assert.equal(preset.disabled, false);
});

test("a Run refused over a flagged field opens the settings sheet on it, and a failed start opens nothing", async t => {
  const offset = new FakeElement();
  const elements = makeElements(["remux"], { "remux-sub-offset": offset });
  installDom(t, elements);
  const text = "Invalid sub-offset value 'abc': must be a number of seconds within range.";
  const page = makeRuntime(idleTracker(), {
    reply: async () => {
      // The core publishes the flag before it answers; the answer comes after that state.
      page.state = viewState({ remux: modeView([], { draft: draft("remux", { sub_offset: "abc", invalid: "sub_offset" }) }) });
      throw text;
    },
  });
  const controller = new JobController("remux", page.runtime);
  await controller.start();
  assert.deepEqual(page.opened, ["remux"]);
  assert.equal(offset.focused, true);
  controller.syncGlobalControls();
  assert.equal(offset.classList.contains("input-error"), true);

  // An edit of the field clears the flag in the core, and its publication clears the class.
  page.state = viewState();
  controller.syncGlobalControls();
  assert.equal(offset.classList.contains("input-error"), false);

  // A failed start flags nothing: the core only logs it.
  page.runtime.send = async () => { throw "No episodes selected in queue."; };
  await controller.start();
  assert.deepEqual(page.opened, ["remux"]);
});

test("Run, Stop and the status pill render the published controls, and a pending Run keeps Run disabled", async t => {
  const elements = makeElements();
  installDom(t, elements);
  const page = makeRuntime(idleTracker());
  const runBtn = elements.get("remux-run");
  const stopBtn = elements.get("remux-stop");
  const controller = new JobController("remux", page.runtime);

  page.state = withControls("remux", { run_label: "Plan remux · 2", run_title: "Scanning the folder", run_disabled: true });
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);
  assert.equal(runBtn.textContent, "Plan remux · 2");
  assert.equal(runBtn.title, "Scanning the folder");
  assert.equal(runBtn.hidden, false);
  assert.equal(stopBtn.hidden, true);

  page.state = withControls("remux", {
    owns: true,
    stop_label: "Stopping…",
    stop_disabled: true,
    status: { class: "status-running", label: "Stopping" },
  });
  controller.syncGlobalControls();
  assert.equal(runBtn.hidden, true);
  assert.equal(stopBtn.hidden, false);
  assert.equal(stopBtn.disabled, true);
  assert.equal(stopBtn.textContent, "Stopping…");
  assert.equal(elements.get("remux-status").className, "status-pill status-running");
  assert.equal(elements.get("remux-status").lastChild.textContent, " Stopping");
  assert.equal(elements.get("remux-status").innerHTML, "", "the label is never written as markup");

  // An enabled Run stays disabled while a Run sent is not answered.
  page.state = withControls("remux", {});
  let answer;
  page.runtime.send = () => new Promise(resolve => { answer = resolve; });
  const running = controller.start();
  assert.equal(runBtn.disabled, true);
  answer(true);
  await running;
  assert.equal(runBtn.disabled, false);
});

test("a busy batch locks the settings and every control a batch locks, and each queue follows its own selection lock", async t => {
  const bulk = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = makeElements(["remux", "extract"]);
  for (const [id, button] of bulk) elements.set(id, button);
  installDom(t, elements);
  const queues = { remux: idleTracker(), extract: idleTracker() };
  const page = makeRuntime(null, { queueFor: mode => queues[mode] });
  const remux = new JobController("remux", page.runtime);
  const field = new FakeElement();
  remux.formControls = [field];
  const preset = new FakeElement();
  globalThis.document.querySelectorAll = selector => (selector.includes(".lock-while-job") ? [preset] : []);

  // A start holds the batch: both queues are locked.
  page.state = viewState({
    busy: true,
    remux: modeView([], { controls: controls("remux", { selection_locked: true }) }),
    extract: modeView([], { controls: controls("extract", { selection_locked: true }) }, "extract"),
  });
  remux.syncGlobalControls();
  for (const [id, button] of bulk) assert.equal(button.disabled, true, id);
  assert.equal(queues.remux.selectionLocked, true);
  assert.equal(queues.extract.selectionLocked, true);
  assert.equal(remux.applyBulkSelection("extract", false), false);
  assert.equal(field.disabled, true);
  assert.equal(preset.disabled, true);

  // Idle, with only the remux queue rescanning: the other tab keeps its controls live.
  page.state = withControls("remux", { selection_locked: true });
  remux.syncGlobalControls();
  for (const { id, mode } of BULK_SELECTION_CONTROLS) {
    assert.equal(elements.get(id).disabled, mode === "remux", `${id} must follow its own queue's lock`);
  }
  assert.equal(queues.remux.selectionLocked, true);
  assert.equal(queues.extract.selectionLocked, false);
  assert.equal(remux.applyBulkSelection("extract", true), true);
  assert.equal(field.disabled, false);
  assert.equal(preset.disabled, false);
});

test("before the first View State the markup's labels stand and the queues stay unlocked", t => {
  const elements = makeElements();
  installDom(t, elements);
  const tracker = idleTracker();
  const page = makeRuntime(tracker);
  page.state = null;
  const runBtn = elements.get("remux-run");
  runBtn.textContent = "Run";
  new JobController("remux", page.runtime).syncGlobalControls();
  assert.equal(runBtn.textContent, "Run");
  assert.equal(runBtn.disabled, false);
  assert.equal(tracker.selectionLocked, false);
  assert.equal(elements.get("remux-status").lastChild.textContent, " Idle");
});
