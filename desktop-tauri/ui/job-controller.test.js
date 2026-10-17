import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { IDLE_RUN, draft, jobView, modeView } from "./fake-core.js";

// Rendering and wiring only: the Job Coordinator, the request a Run builds and its preflight are
// the Desktop Core's, tested in `crates/mkv-desktop-core/src/{coordinator,draft,actor}.rs`, and so
// are the Run Log lines a Run, a Stop or a cache clear writes (`actor/start.rs`, `actor.rs`).

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
    for (const id of ["run", "stop", "log", "status"]) elements.set(`${mode}-${id}`, new FakeElement());
  }
  for (const [id, value] of Object.entries(extra)) elements.set(id, value instanceof FakeElement ? value : new FakeElement(value));
  return elements;
}

/** Installs a DOM stub that also lets a real QueueTracker render synchronously. */
function installDom(t, elements) {
  const previousDocument = globalThis.document;
  const previousRaf = globalThis.requestAnimationFrame;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
    createElement: () => new FakeElement(),
    createDocumentFragment: () => new FakeElement(),
  };
  globalThis.requestAnimationFrame = callback => callback();
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.requestAnimationFrame = previousRaf;
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

const idleTracker = (fields = {}) => ({ state: { totalCount: 2 }, getSelectedCount: () => 2, isScanning: false, ...fields });

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

test("Run is disabled while its queue scans, while starting, or while another mode's job runs", async t => {
  const dryRun = new FakeElement();
  const elements = makeElements(["remux"], { "remux-dry-run": dryRun });
  installDom(t, elements);
  const tracker = idleTracker({ isScanning: true });
  const page = makeRuntime(tracker);
  const runBtn = elements.get("remux-run");

  const controller = new JobController("remux", page.runtime);
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);
  assert.equal(runBtn.title, "Scanning the folder");
  assert.equal(runBtn.textContent, "Run remux · 2");

  tracker.isScanning = false;
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, false);
  assert.equal(runBtn.title, "");

  // A dry run plans; the label follows the checkbox.
  dryRun.checked = true;
  dryRun.handlers.get("change")();
  assert.equal(runBtn.textContent, "Plan remux · 2");

  // The other mode's start blocks this Run, and its tooltip says so.
  page.state = viewState({ run: { phase: "starting", mode: "extract", settings: null }, busy: true });
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);
  assert.equal(runBtn.title, "Extract batch is running");
  assert.equal(runBtn.hidden, false);

  // An active job with an idle coordinator still holds the batch.
  page.state = viewState({ job: jobView([], { mode: "extract" }), busy: true });
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);
  assert.equal(runBtn.title, "Extract batch is running");
});

test("while a start is before its launch every control a batch locks is disabled, in both modes", async t => {
  const bulk = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = makeElements(["remux", "extract"], {
    "remux-episode-list": "",
    "remux-queue-count": "",
    "extract-episode-list": "",
    "extract-queue-count": "",
  });
  for (const [id, button] of bulk) elements.set(id, button);
  installDom(t, elements);
  const queues = { remux: new QueueTracker("remux", async () => false), extract: new QueueTracker("extract", async () => false) };
  for (const [mode, queue] of Object.entries(queues)) queue.applyView(modeView([`${mode}.mkv`], { directory: `/anime/${mode}`, manifest_rev: 1 }));
  const page = makeRuntime(null, { queueFor: mode => queues[mode] });
  const remux = new JobController("remux", page.runtime);
  const extract = new JobController("extract", page.runtime);

  page.state = viewState({ run: { phase: "starting", mode: "remux", settings: null }, busy: true });
  remux.syncGlobalControls();
  extract.syncGlobalControls();
  for (const [id, button] of bulk) assert.equal(button.disabled, true, id);
  assert.equal(queues.remux.selectionLocked, true);
  assert.equal(queues.extract.selectionLocked, true);
  assert.equal(remux.applyBulkSelection("remux", false), false);
  assert.equal(remux.applyBulkSelection("extract", false), false);
  assert.equal(elements.get("extract-run").disabled, true);
  // The owner shows Stop, and its pill reads Running from the start on.
  assert.equal(elements.get("remux-run").hidden, true);
  assert.equal(elements.get("remux-stop").hidden, false);
  assert.equal(elements.get("remux-stop").disabled, false);
  assert.match(elements.get("remux-status").innerHTML, /Running/);
  assert.equal(elements.get("extract-stop").hidden, true);

  // A Stop pending: Stopping, and no second Stop.
  page.state = viewState({ run: { phase: "stopping", mode: "remux", settings: null }, busy: true });
  remux.syncGlobalControls();
  assert.equal(elements.get("remux-stop").disabled, true);
  assert.equal(elements.get("remux-stop").textContent, "Stopping…");
  assert.match(elements.get("remux-status").innerHTML, /Stopping/);

  // Cancelled before the launch: the lock lifts and the pill shows the outcome.
  page.state = viewState({ remux: modeView([], { outcome: "cancelled" }) });
  remux.syncGlobalControls();
  extract.syncGlobalControls();
  for (const [id, button] of bulk) assert.equal(button.disabled, false, id);
  assert.equal(queues.remux.selectionLocked, false);
  assert.equal(elements.get("remux-stop").hidden, true);
  assert.match(elements.get("remux-status").innerHTML, /Cancelled/);
});

test("an idle batch freezes only the queue that is scanning, not the other tab", t => {
  const elements = makeElements(["remux", "extract"], {
    "remux-episode-list": "",
    "remux-queue-count": "",
    "extract-episode-list": "",
    "extract-queue-count": "",
  });
  for (const { id } of BULK_SELECTION_CONTROLS) elements.set(id, new FakeElement());
  installDom(t, elements);

  const queues = { remux: new QueueTracker("remux", async () => false), extract: new QueueTracker("extract", async () => false) };
  const shown = mode => modeView([`${mode}.mkv`], { directory: `/anime/${mode}`, manifest_rev: 1 });
  for (const [mode, tracker] of Object.entries(queues)) tracker.applyView(shown(mode));
  const page = makeRuntime(null, { queueFor: mode => queues[mode] });
  const remuxController = new JobController("remux", page.runtime);
  const extractController = new JobController("extract", page.runtime);
  const sync = () => [remuxController, extractController].forEach(controller => controller.syncGlobalControls());

  // The remux queue is mid-rescan while nothing holds the batch.
  queues.remux.applyView({ ...shown("remux"), scan: { state: "scanning", directory: "/anime/remux" } });
  sync();
  for (const { id, mode } of BULK_SELECTION_CONTROLS) {
    // The scanning queue is frozen; the idle one keeps its controls live.
    assert.equal(elements.get(id).disabled, mode === "remux", `${id} must follow its own queue's scan state`);
  }
  assert.equal(queues.remux.selectionLocked, true, "the scanning queue is frozen");
  assert.equal(queues.extract.selectionLocked, false, "an idle extract queue must stay editable");
  assert.equal(remuxController.applyBulkSelection("extract", false), true, "the other tab must accept bulk selection");

  // Once the rescan lands, both queues are editable again.
  queues.remux.applyView({ ...shown("remux"), manifest_rev: 2 });
  sync();
  for (const { id } of BULK_SELECTION_CONTROLS) assert.equal(elements.get(id).disabled, false);
  assert.equal(queues.remux.selectionLocked, false);
});

test("the status pill shows the mode's published outcome, and Idle before its first batch", t => {
  const elements = makeElements();
  installDom(t, elements);
  const page = makeRuntime(idleTracker());
  const controller = new JobController("remux", page.runtime);
  for (const [outcome, label] of [[null, "Idle"], ["done", "Done"], ["cancelled", "Cancelled"], ["failed", "Failed"]]) {
    page.state = viewState({ remux: modeView([], { outcome }) });
    controller.syncGlobalControls();
    assert.match(elements.get("remux-status").innerHTML, new RegExp(label), String(outcome));
  }
  // Another mode's batch leaves this pill on this mode's own outcome.
  page.state = viewState({ run: { phase: "running", mode: "extract", settings: null }, busy: true, remux: modeView([], { outcome: "done" }) });
  controller.syncGlobalControls();
  assert.match(elements.get("remux-status").innerHTML, /Done/);
});
