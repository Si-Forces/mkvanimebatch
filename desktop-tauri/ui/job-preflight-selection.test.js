import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { createFakeCore, modeView, row } from "./fake-core.js";

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
    this.classList = { add() {}, remove() {} };
  }

  addEventListener(type, handler) { (this.handlers ??= new Map()).set(type, handler); }
  appendChild(child) { this.children.push(child); return child; }
  querySelector() { return new FakeElement(); }
  remove() {}
  focus() {}
}


/** A manual subtitle assignment in the shape app.js stores in `QueueTracker.assignments`. */
const subtitle = (id, path) => ({
  target_video: id,
  asset_path: path,
  track_type: "subtitle",
  language: "ind",
  title: null,
  offset_ms: null,
});

/** Run/Stop/log/status controls for each mode, plus `extra` ids. */
function makeElements(modes, extra = {}) {
  const elements = new Map();
  for (const mode of modes) {
    for (const id of ["run", "stop", "log", "status"]) elements.set(`${mode}-${id}`, new FakeElement());
  }
  for (const [id, value] of Object.entries(extra)) elements.set(id, new FakeElement(value));
  return elements;
}

/** Installs a DOM stub that also lets a real QueueTracker render synchronously. */
function installDom(t, elements, overrides = {}) {
  const previousDocument = globalThis.document;
  const previousRaf = globalThis.requestAnimationFrame;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
    createElement: () => new FakeElement(),
    createDocumentFragment: () => new FakeElement(),
    ...overrides,
  };
  globalThis.requestAnimationFrame = callback => callback();
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.requestAnimationFrame = previousRaf;
  });
}

/** The Effective Settings `resolve_batch_request` answers with. */
const SETTINGS = { workers: 8, io_jobs: 2, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };

/** A runtime of fakes; `resolve_batch_request` answers `SETTINGS`, so `invoke` only sees the other commands. */
function makeRuntime(tracker, { invoke = async () => null, ...overrides } = {}) {
  return {
    coordinator: createJobCoordinator(),
    appendLog() {},
    clearLog() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async () => true,
    showModal: async () => true,
    syncState: async () => null,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    listen: async () => () => {},
    invoke: (command, args) => (command === "resolve_batch_request" ? SETTINGS : invoke(command, args)),
    ...overrides,
  };
}

/** `invoke` that records each `run_job` request and answers with `jobId`. */
function recordRunJob(requests, jobId) {
  return async (command, args) => {
    if (command !== "run_job") return null;
    requests.push(args.request);
    return jobId;
  };
}

/** Mock tracker whose last inspection of `dir` found `ids`, with `selected` checked. */
function scannedTracker(dir, ids, selected, overrides = {}) {
  return {
    currentDirectory: dir,
    assignments: new Map(),
    getSelectedOrderedItemIds: () => selected,
    manifestItemIds: () => ids,
    state: { totalCount: ids.length },
    ...overrides,
  };
}

/** A real Queue Tracker of `mode` over a fake core, rendering what the core publishes. */
function trackerOverCore(mode = "remux", core = createFakeCore()) {
  const tracker = new QueueTracker(mode, async intent => {
    try {
      return await core.invoke("dispatch", { intent });
    } catch {
      return false;
    }
  });
  core.onPublish(state => tracker.applyView(state[mode], state.job));
  return { tracker, core };
}

test("job controller includes selected_items and filters assignments to selected items", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/anime/series" }));

  const requests = [];
  const queue = scannedTracker(null, ["ep1.mkv", "ep2.mkv"], ["ep1.mkv"], {
    assignments: new Map([
      ["ep1.mkv", subtitle("ep1.mkv", "/subs/ep1.ass")],
      ["ep2.mkv", subtitle("ep2.mkv", "/subs/ep2.ass")],
    ]),
  });
  const controller = new JobController("remux", makeRuntime(queue, { invoke: recordRunJob(requests, 42) }));
  await controller.start();

  assert.equal(controller.jobId, 42);
  assert.deepEqual(requests[0].selected_items, ["ep1.mkv"]);
  assert.deepEqual(requests[0].assignments.map(a => a.target_video), ["ep1.mkv"]);
});

test("Start submits the selection and assignments captured before the forced scan, and rejects vanished IDs", async t => {
  const elements = makeElements(["remux"], {
    "remux-input-dir": "/anime/series",
    "remux-episode-list": "",
    "remux-queue-count": "",
  });
  installDom(t, elements, {
    querySelectorAll: selector => selector.startsWith("#remux-settings input,") ? [elements.get("remux-input-dir")] : [],
  });

  const { tracker, core } = trackerOverCore();
  const [one, two, three, four, five] = ["01", "02", "03", "04", "05"].map(n => `Series/${n}.mkv`);
  const unselected = id => row(id, { selected: false });
  // The queue after the user reordered, deselected 03 and rescanned, which found 04 unselected.
  core.accept("remux", "/anime/series", [two, one, unselected(three), unselected(four)], {
    assignments: {
      [two]: subtitle(two, "/subs/02.ass"),
      [three]: subtitle(three, "/subs/03.ass"),
      [four]: subtitle(four, "/subs/04.ass"),
    },
  });
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);
  assert.deepEqual(tracker.state.order.map(id => tracker.cards.get(id).checkbox.checked), [true, true, false, false]);

  const logs = [];
  const calls = [];
  const listeners = new Map();
  let found = [two, one, unselected(three), unselected(four), unselected(five)];
  let vanished = [];
  const runtime = makeRuntime(tracker, {
    appendLog(_el, line) { logs.push(line.text); },
    async scanInputDirectory(path, mode, { force }) {
      assert.equal(path, "/anime/series");
      assert.equal(mode, "remux");
      assert.equal(force, true);
      assert.equal(elements.get("remux-input-dir").disabled, true);
      // What the forced scan publishes: an assignment changed meanwhile must not reach the run.
      core.accept("remux", "/anime/series", found, {
        assignments: { [two]: subtitle(two, "/subs/changed.ass") },
        vanished_selected_ids: vanished,
      });
      return true;
    },
    async listen(event, callback) { listeners.set(event, callback); return () => listeners.delete(event); },
    async invoke(command, args) {
      if (command === "run_job") { calls.push(args.request); return 42; }
      throw new Error(`Unexpected command ${command}`);
    },
  });
  const controller = new JobController("remux", runtime);
  await controller.start();
  assert.deepEqual(calls.map(request => request.selected_items), [[two, one]]);
  assert.deepEqual(calls[0].assignments.map(assignment => assignment.target_video), [two]);
  assert.equal(calls[0].assignments[0].asset_path, "/subs/02.ass");
  assert.deepEqual(tracker.state.order, [two, one, three, four, five]);

  listeners.get("job-done")({ payload: { mode: "remux", job_id: 42, code: 0 } });
  found = [one, unselected(three), unselected(four), unselected(five)];
  vanished = [two];
  await controller.start();
  assert.equal(calls.length, 1);
  assert.equal(controller.running, false);
  assert.equal(runtime.coordinator.activeMode, null);
  assert.ok(logs.some(message => message.includes(two) && message.includes("disappeared")));
});

test("Start freezes selection for the whole preflight and submits only the captured IDs", async t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = makeElements(["remux"], {
    "remux-input-dir": "/anime/series",
    "remux-episode-list": "",
    "remux-queue-count": "",
  });
  for (const [id, button] of bulkButtons) elements.set(id, button);
  installDom(t, elements);

  const { tracker, core } = trackerOverCore();
  const [one, two, three] = ["01", "02", "03"].map(n => `Series/${n}.mkv`);
  core.accept("remux", "/anime/series", [two, one]);

  let resolveScan;
  const scanGate = new Promise(resolve => { resolveScan = resolve; });
  const requests = [];
  const listeners = new Map();
  const runtime = makeRuntime(tracker, {
    scanInputDirectory: async (path, mode, { force }) => {
      assert.equal(force, true);
      return scanGate;
    },
    async listen(event, callback) { listeners.set(event, callback); return () => listeners.delete(event); },
    async invoke(command, args) {
      if (command === "run_job") { requests.push(args.request); return 77; }
      throw new Error(`Unexpected command ${command}`);
    },
  });
  const { coordinator } = runtime;
  const controller = new JobController("remux", runtime);

  const startPromise = controller.start();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(requests.length, 0);
  for (const [, button] of bulkButtons) assert.equal(button.disabled, true, "bulk control must be disabled during preflight");

  // The core does not lock the queue before run_job, so the view must not send any change.
  assert.equal(controller.applyBulkSelection("remux", false), false);
  assert.equal(tracker.selectionLocked, true, "tracker must refuse selection mutations during preflight");
  const list = elements.get("remux-episode-list");
  const checkbox = { dataset: { itemId: one }, checked: false };
  list.handlers.get("change")({ target: { closest: () => checkbox } });
  assert.equal(checkbox.checked, true, "a refused checkbox click must be reverted in the DOM");
  list.handlers.get("click")({ target: { closest: sel => (sel === ".ep-reorder-btn" ? { dataset: { itemId: one }, classList: { contains: () => true } } : null) } });
  tracker.selectAll();
  tracker.deselectAll();
  assert.deepEqual(core.intents, [], "no Intent may leave the view during preflight");

  // The forced scan re-renders the rows; a fresh checkbox must not escape the freeze.
  core.accept("remux", "/anime/series", [two, one, row(three, { selected: false })]);
  assert.equal(tracker.selectionLocked, true);
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: one }, checked: false }) } });
  assert.deepEqual(core.intents, []);
  resolveScan(true);
  await startPromise;

  assert.deepEqual(requests.map(request => request.selected_items), [[two, one]]);
  assert.equal(controller.jobId, 77);

  listeners.get("job-done")({ payload: { mode: "remux", job_id: 77, code: 0 } });
  assert.equal(coordinator.activeMode, null);
  assert.equal(tracker.selectionLocked, false, "the freeze must lift when the job releases ownership");
  for (const [, button] of bulkButtons) assert.equal(button.disabled, false, "bulk control must return after completion");
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: one }, checked: false }) } });
  assert.equal(controller.applyBulkSelection("remux", false), true);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(core.intents.map(intent => intent.type), ["set_selected", "set_all_selected"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), []);
});

test("An idle coordinator freezes only the queue that is scanning, not the other tab", t => {
  const elements = makeElements(["remux", "extract"], {
    "remux-episode-list": "",
    "remux-queue-count": "",
    "extract-episode-list": "",
    "extract-queue-count": "",
  });
  for (const { id } of BULK_SELECTION_CONTROLS) elements.set(id, new FakeElement());
  installDom(t, elements);

  const queues = { remux: new QueueTracker("remux"), extract: new QueueTracker("extract") };
  const shown = mode => modeView([`${mode}.mkv`], { directory: `/anime/${mode}`, manifest_rev: 1 });
  for (const [mode, tracker] of Object.entries(queues)) tracker.applyView(shown(mode));
  const runtime = makeRuntime(null, { queueFor: mode => queues[mode] });
  const { coordinator } = runtime;
  const remuxController = new JobController("remux", runtime);
  new JobController("extract", runtime);

  // The remux queue is mid-rescan while the coordinator is idle.
  queues.remux.applyView({ ...shown("remux"), scan: { state: "scanning", directory: "/anime/remux" } });
  coordinator.syncControls();

  assert.equal(coordinator.activeMode, null, "no job is running");
  for (const { id, mode } of BULK_SELECTION_CONTROLS) {
    // The scanning queue is frozen; the idle one keeps its controls live.
    assert.equal(elements.get(id).disabled, mode === "remux", `${id} must follow its own queue's scan state`);
  }
  assert.equal(queues.remux.selectionLocked, true, "the scanning queue is frozen");
  assert.equal(queues.extract.selectionLocked, false, "an idle extract queue must stay editable");
  assert.equal(remuxController.applyBulkSelection("extract", false), true, "the other tab must accept bulk selection");

  // Once the rescan lands, both queues are editable again.
  queues.remux.applyView({ ...shown("remux"), manifest_rev: 2 });
  coordinator.syncControls();
  for (const { id } of BULK_SELECTION_CONTROLS) assert.equal(elements.get(id).disabled, false);
  assert.equal(queues.remux.selectionLocked, false);
});

test("Start with different input directory defaults to all-selected and drops previous directory assignments", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/anime/series-b" }));

  const requests = [];
  // The queue holds series-a with only 01 selected and an assignment.
  const tracker = scannedTracker("/anime/series-a", ["01.mkv", "02.mkv"], ["01.mkv"], {
    assignments: new Map([["01.mkv", subtitle("01.mkv", "/subs/old.ass")]]),
  });
  const runtime = makeRuntime(tracker, {
    scanInputDirectory: async dir => {
      // Inspecting series-b replaces the queue with its all-selected default.
      tracker.currentDirectory = dir;
      tracker.assignments = new Map();
      tracker.getSelectedOrderedItemIds = () => ["01.mkv", "02.mkv"];
      tracker.manifestItemIds = () => ["01.mkv", "02.mkv"];
      return true;
    },
    invoke: recordRunJob(requests, 101),
  });

  await new JobController("remux", runtime).start();

  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0].selected_items, ["01.mkv", "02.mkv"]);
  assert.equal(requests[0].assignments, null, "no assignments carried over from series-a");
});

test("Start from empty queue with unscanned first directory discovers and runs all items", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/anime/first-show" }));

  const requests = [];
  const tracker = scannedTracker(null, [], []);
  const runtime = makeRuntime(tracker, {
    scanInputDirectory: async dir => {
      tracker.currentDirectory = dir;
      tracker.getSelectedOrderedItemIds = () => ["01.mkv", "02.mkv"];
      tracker.manifestItemIds = () => ["01.mkv", "02.mkv"];
      tracker.state.totalCount = 2;
      return true;
    },
    invoke: recordRunJob(requests, 201),
  });

  await new JobController("remux", runtime).start();

  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0].selected_items, ["01.mkv", "02.mkv"]);
});

test("start rejects vanished selection when input is an alias of the scanned canonical directory", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/link/show" }));

  const logs = [];
  const requests = [];
  const tracker = scannedTracker("/real/show", ["ep1.mkv", "ep2.mkv"], ["ep1.mkv", "ep2.mkv"]);
  const runtime = makeRuntime(tracker, {
    appendLog(_el, line) { logs.push(line.text); },
    scanInputDirectory: async () => {
      tracker.manifestItemIds = () => ["ep1.mkv"];
      tracker.getSelectedOrderedItemIds = () => ["ep1.mkv"];
      return true;
    },
    invoke: recordRunJob(requests, 101),
  });

  await new JobController("remux", runtime).start();

  assert.equal(requests.length, 0);
  assert.ok(logs.some(message => message.includes("ep2.mkv") && message.includes("disappeared")));
});

test("start rejects selections that vanished during an earlier user rescan", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/anime/show" }));

  const logs = [];
  const requests = [];
  const tracker = scannedTracker("/anime/show", ["ep1.mkv"], ["ep1.mkv"], {
    state: { totalCount: 1, vanishedSelectedIds: ["ep2.mkv"] },
  });
  const runtime = makeRuntime(tracker, {
    appendLog(_el, line) { logs.push(line.text); },
    invoke: recordRunJob(requests, 101),
  });

  await new JobController("remux", runtime).start();

  assert.equal(requests.length, 0);
  assert.ok(logs.some(message => message.includes("ep2.mkv") && message.includes("disappeared")));
});

test("start rejects when a vanished selection reappears during the forced rescan", async t => {
  installDom(t, makeElements(["remux"], { "remux-input-dir": "/anime/show" }));

  const logs = [];
  const requests = [];
  const tracker = scannedTracker("/anime/show", ["ep1.mkv"], ["ep1.mkv"], {
    state: { totalCount: 1, vanishedSelectedIds: ["ep2.mkv"] },
  });
  const runtime = makeRuntime(tracker, {
    appendLog(_el, line) { logs.push(line.text); },
    scanInputDirectory: async () => {
      tracker.manifestItemIds = () => ["ep1.mkv", "ep2.mkv"];
      tracker.state = { totalCount: 2, vanishedSelectedIds: [] };
      tracker.getSelectedOrderedItemIds = () => ["ep1.mkv", "ep2.mkv"];
      return true;
    },
    invoke: recordRunJob(requests, 101),
  });

  await new JobController("remux", runtime).start();

  assert.equal(requests.length, 0);
  const message = logs.find(text => text.includes("Queue selection changed during inspection"));
  assert.ok(message?.includes("ep2.mkv"), "the reappeared episode must be named");
});
