import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";

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
  querySelector(selector) {
    const el = new FakeElement();
    if (selector === ".ep-select-check") el.checked = this.innerHTML.includes('class="ep-select-check" checked ');
    return el;
  }
  focus() {}
}

test("job controller includes selected_items and filters assignments to selected items", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/series")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const invocations = [];
  const coordinator = createJobCoordinator();
  const queue = {
    assignments: new Map([
      ["ep1.mkv", "/subs/ep1.ass"],
      ["ep2.mkv", "/subs/ep2.ass"],
    ]),
    lastManifest: {
      version: 2,
      items: [
        { id: "ep1.mkv", name: "ep1.mkv" },
        { id: "ep2.mkv", name: "ep2.mkv" },
      ],
    },
    state: { totalCount: 2 },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent() {},
    finalize() {},
  };
  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => queue,
    safeAddListener() {},
    scanInputDirectory: async () => true,
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(command, args) {
      invocations.push([command, args]);
      if (command === "run_job") return 42;
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.jobId, 42);
  const runJobCall = invocations.find(([cmd]) => cmd === "run_job");
  assert.ok(runJobCall);
  const req = runJobCall[1].request;
  assert.deepEqual(req.selected_items, ["ep1.mkv"]);
  assert.ok(Array.isArray(req.assignments));
  assert.equal(req.assignments.length, 1);
  assert.equal(req.assignments[0].target_video, "ep1.mkv");
});

test("Start preserves selected order across real rescans and rejects vanished IDs", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/series")],
    ["remux-episode-list", new FakeElement()],
    ["remux-queue-count", new FakeElement()],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  const previousRaf = globalThis.requestAnimationFrame;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: selector => selector.startsWith("#view-remux input,") ? [elements.get("remux-input-dir")] : [],
    querySelector: () => null,
    createElement: () => new FakeElement(),
    createDocumentFragment: () => new FakeElement(),
  };
  globalThis.requestAnimationFrame = callback => callback();
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.requestAnimationFrame = previousRaf;
  });

  const manifest = (...ids) => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const tracker = new QueueTracker("remux");
  const one = "Series/01.mkv";
  const two = "Series/02.mkv";
  const three = "Series/03.mkv";
  const four = "Series/04.mkv";
  const five = "Series/05.mkv";
  assert.equal(tracker.acceptInspection(tracker.startScan(), "/anime/series", manifest(one, two, three)), true);
  tracker.reorderCard(two, "up");
  const thirdCheckbox = tracker.cards.get(three).checkbox;
  thirdCheckbox.dataset.itemId = three;
  thirdCheckbox.checked = false;
  elements.get("remux-episode-list").handlers.get("change")({ target: { closest: () => thirdCheckbox } });
  tracker.assignments = new Map([[two, "/subs/02.ass"], [three, "/subs/03.ass"]]);

  // A user-initiated same-directory inspection discovers a new unselected item.
  assert.equal(tracker.acceptInspection(tracker.startScan(), "/anime/series", manifest(one, two, three, four)), true);
  assert.deepEqual(tracker.state.order, [two, one, three, four]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);
  assert.deepEqual([...tracker.state.order].map(id => tracker.cards.get(id).checkbox.checked), [true, true, false, false]);
  tracker.assignments.set(four, "/subs/04.ass");

  const coordinator = createJobCoordinator();
  const logs = [];
  const calls = [];
  const listeners = new Map();
  let replacement = manifest(one, two, three, four, five);
  const runtime = {
    coordinator,
    appendLog(_el, text) { logs.push(text); },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    async scanInputDirectory(path, mode, { force }) {
      assert.equal(path, "/anime/series");
      assert.equal(mode, "remux");
      assert.equal(force, true);
      assert.equal(elements.get("remux-input-dir").disabled, true);
      tracker.assignments.set(two, "/subs/changed.ass");
      return tracker.acceptInspection(tracker.startScan(), path, replacement);
    },
    updateWorkerChips() {},
    telemetryLoop: { setJobActive() {} },
    async listen(event, callback) { listeners.set(event, callback); return () => listeners.delete(event); },
    async invoke(command, args) {
      if (command === "run_job") { calls.push(args.request); return 42; }
      throw new Error(`Unexpected command ${command}`);
    },
  };
  const controller = new JobController("remux", runtime);
  await controller.start();
  assert.deepEqual(calls.map(request => request.selected_items), [[two, one]]);
  assert.deepEqual(calls[0].assignments.map(assignment => assignment.target_video), [two]);
  assert.equal(calls[0].assignments[0].asset_path, "/subs/02.ass");
  assert.deepEqual(tracker.state.order, [two, one, three, four, five]);
  assert.deepEqual([...tracker.state.order].map(id => tracker.cards.get(id).checkbox.checked), [true, true, false, false, false]);

  listeners.get("job-done")({ payload: { mode: "remux", job_id: 42, code: 0 } });
  replacement = manifest(one, three, four, five);
  await controller.start();
  assert.equal(calls.length, 1);
  assert.equal(controller.running, false);
  assert.equal(coordinator.activeMode, null);
  assert.ok(logs.some(message => message.includes(two) && message.includes("disappeared")));
});

test("Start freezes bulk selection for the whole preflight and submits only the captured IDs", async t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/series")],
    ["remux-episode-list", new FakeElement()],
    ["remux-queue-count", new FakeElement()],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
    ...bulkButtons,
  ]);
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

  const manifest = (...ids) => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const tracker = new QueueTracker("remux");
  const one = "Series/01.mkv";
  const two = "Series/02.mkv";
  const three = "Series/03.mkv";
  assert.equal(tracker.acceptInspection(tracker.startScan(), "/anime/series", manifest(one, two)), true);
  tracker.reorderCard(two, "up");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);

  let resolveScan;
  const scanGate = new Promise(resolve => {
    resolveScan = resolve;
  });
  const requests = [];
  const listeners = new Map();
  const coordinator = createJobCoordinator();
  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (path, mode, { force }) => {
      assert.equal(path, "/anime/series");
      assert.equal(mode, "remux");
      assert.equal(force, true);
      return scanGate;
    },
    updateWorkerChips() {},
    telemetryLoop: { setJobActive() {} },
    async listen(event, callback) { listeners.set(event, callback); return () => listeners.delete(event); },
    async invoke(command, args) {
      if (command === "run_job") { requests.push(args.request); return 77; }
      throw new Error(`Unexpected command ${command}`);
    },
  };
  const controller = new JobController("remux", runtime);

  // A queue mid-inspection is not editable either, with no coordinator involved.
  const scanningToken = tracker.startScan();
  assert.equal(controller.applyBulkSelection("remux", false), false);
  assert.deepEqual(tracker.state.order, [two, one]);
  assert.equal(tracker.acceptInspection(scanningToken, "/anime/series", manifest(one, two)), true);

  const startPromise = controller.start();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(requests.length, 0);
  for (const [, button] of bulkButtons) assert.equal(button.disabled, true, "bulk control must be disabled during preflight");

  // The user clicks both bulk buttons mid-preflight: neither may reach the queue.
  assert.equal(controller.applyBulkSelection("remux", false), false);
  assert.equal(controller.applyBulkSelection("remux", true), false);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);

  // Card-level controls must obey the same freeze. The guard lives on the
  // mutation, not on the disabled attribute: the forced scan below re-renders
  // the cards, and a freshly mounted checkbox arrives enabled.
  const list = elements.get("remux-episode-list");
  assert.equal(tracker.selectionLocked, true, "tracker must refuse selection mutations during preflight");
  const checkbox = { dataset: { itemId: one }, checked: false };
  list.handlers.get("change")({ target: { closest: () => checkbox } });
  assert.equal(tracker.state.items.get(one).selected, true, "card checkbox must not deselect during preflight");
  assert.equal(checkbox.checked, true, "a refused checkbox click must be reverted in the DOM");
  const reorderBtn = { dataset: { itemId: one }, classList: { contains: () => false } };
  list.handlers.get("click")({ target: { closest: () => reorderBtn } });
  tracker.selectAll();
  tracker.deselectAll();
  assert.deepEqual(tracker.state.order, [two, one], "card reorder must not change order during preflight");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one], "card controls must not change selection during preflight");

  tracker.acceptInspection(tracker.startScan(), "/anime/series", manifest(one, two, three));
  assert.equal(tracker.selectionLocked, true, "a re-rendered card must not escape the freeze");
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: one }, checked: false }) } });
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);

  tracker.acceptInspection(tracker.startScan(), "/anime/series", manifest(one, two, three));
  resolveScan(true);
  await startPromise;

  assert.deepEqual(requests.map(request => request.selected_items), [[two, one]]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), [two, one]);
  assert.equal(tracker.state.items.get(three).selected, false);
  assert.equal(controller.jobId, 77);

  listeners.get("job-done")({ payload: { mode: "remux", job_id: 77, code: 0 } });
  assert.equal(coordinator.activeMode, null);
  assert.equal(tracker.selectionLocked, false, "the card freeze must lift when the job releases ownership");
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: one }, checked: false }) } });
  assert.equal(tracker.state.items.get(one).selected, false, "card selection must work again once idle");
  for (const [, button] of bulkButtons) assert.equal(button.disabled, false, "bulk control must return after completion");
  assert.equal(controller.applyBulkSelection("remux", false), true);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), []);
});

test("An idle coordinator freezes only the queue that is scanning, not the other tab", t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = new Map([
    ["remux-episode-list", new FakeElement()],
    ["remux-queue-count", new FakeElement()],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
    ["extract-episode-list", new FakeElement()],
    ["extract-queue-count", new FakeElement()],
    ["extract-run", new FakeElement()],
    ["extract-stop", new FakeElement()],
    ["extract-log", new FakeElement()],
    ["extract-status", new FakeElement()],
    ...bulkButtons,
  ]);
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

  const manifest = (...ids) => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const queues = { remux: new QueueTracker("remux"), extract: new QueueTracker("extract") };
  for (const mode of Object.keys(queues)) {
    const tracker = queues[mode];
    assert.equal(tracker.acceptInspection(tracker.startScan(), `/anime/${mode}`, manifest(`${mode}.mkv`)), true);
  }
  const coordinator = createJobCoordinator();
  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: mode => queues[mode],
    safeAddListener() {},
    scanInputDirectory: async () => true,
    updateWorkerChips() {},
    telemetryLoop: { setJobActive() {} },
    async listen() { return () => {}; },
    async invoke() { return 1; },
  };
  const remuxController = new JobController("remux", runtime);
  new JobController("extract", runtime);

  // The remux queue is mid-rescan while the coordinator is idle.
  const rescanToken = queues.remux.startScan();
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
  queues.remux.acceptInspection(rescanToken, "/anime/remux", manifest("remux.mkv"));
  coordinator.syncControls();
  for (const { id } of BULK_SELECTION_CONTROLS) assert.equal(elements.get(id).disabled, false);
  assert.equal(queues.remux.selectionLocked, false);
});

test("Start with different input directory defaults to all-selected and drops previous directory assignments", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/series-b")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const calls = [];
  const coordinator = createJobCoordinator();
  // Queue currently holds state for series-a with only ep1 selected and an assignment
  const tracker = {
    currentDirectory: "/anime/series-a",
    assignments: new Map([["01.mkv", "/subs/old.ass"]]),
    getSelectedOrderedItemIds: () => ["01.mkv"],
    lastManifest: {
      version: 2,
      items: [
        { id: "01.mkv", name: "01.mkv" },
        { id: "02.mkv", name: "02.mkv" },
      ],
    },
    state: { totalCount: 2 },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (dir, mode, opts) => {
      // Inspection of series-b completes: update tracker to series-b state
      tracker.currentDirectory = dir;
      tracker.assignments = new Map();
      tracker.getSelectedOrderedItemIds = () => ["01.mkv", "02.mkv"]; // all selected default
      tracker.lastManifest = {
        version: 2,
        items: [
          { id: "01.mkv", name: "01.mkv" },
          { id: "02.mkv", name: "02.mkv" },
        ],
      };
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen(event, callback) { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 101;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 1);
  // All items from series-b should be selected
  assert.deepEqual(calls[0].selected_items, ["01.mkv", "02.mkv"]);
  // No assignments carried over from series-a
  assert.equal(calls[0].assignments, null);
});
test("Start from empty queue with unscanned first directory discovers and runs all items", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/first-show")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const calls = [];
  const coordinator = createJobCoordinator();
  // Queue starts completely empty and unscanned (currentDirectory is null)
  const tracker = {
    currentDirectory: null,
    lastScannedPath: null,
    assignments: new Map(),
    getSelectedOrderedItemIds: () => [],
    lastManifest: null,
    state: { totalCount: 0 },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (dir, mode, opts) => {
      // Inspection populates the newly scanned directory
      tracker.currentDirectory = dir;
      tracker.lastScannedPath = dir;
      tracker.getSelectedOrderedItemIds = () => ["01.mkv", "02.mkv"];
      tracker.lastManifest = {
        version: 2,
        items: [
          { id: "01.mkv", name: "01.mkv" },
          { id: "02.mkv", name: "02.mkv" },
        ],
      };
      tracker.state.totalCount = 2;
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen(event, callback) { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 201;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].selected_items, ["01.mkv", "02.mkv"]);
});

test("Start preserves selection across canonical directory representations with trailing slashes", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/show/")] // Note trailing slash
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const calls = [];
  const coordinator = createJobCoordinator();
  // Queue was scanned without trailing slash, and user selected only 02.mkv
  const tracker = {
    currentDirectory: "/anime/show", // No trailing slash
    lastScannedPath: "/anime/show",
    assignments: new Map([["02.mkv", "/subs/02.ass"]]),
    getSelectedOrderedItemIds: () => ["02.mkv"],
    lastManifest: {
      version: 2,
      items: [
        { id: "01.mkv", name: "01.mkv" },
        { id: "02.mkv", name: "02.mkv" },
      ],
    },
    state: { totalCount: 2 },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async () => true,
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 202;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 1);
  // Preserves exact user selection because /anime/show and /anime/show/ are canonically identical
  assert.deepEqual(calls[0].selected_items, ["02.mkv"]);
  assert.equal(calls[0].assignments.length, 1);
  assert.equal(calls[0].assignments[0].target_video, "02.mkv");
});

test("start rejects vanished selection when input is an alias of the scanned canonical directory", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/link/show")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const logs = [];
  const calls = [];
  const coordinator = createJobCoordinator();
  const tracker = {
    currentDirectory: "/real/show",
    lastScannedPath: "/real/show",
    assignments: new Map(),
    getSelectedOrderedItemIds: () => ["ep1.mkv", "ep2.mkv"],
    lastManifest: {
      version: 2,
      items: [
        { id: "ep1.mkv", name: "ep1.mkv" },
        { id: "ep2.mkv", name: "ep2.mkv" },
      ],
    },
    state: { totalCount: 2 },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog(_el, text) { logs.push(text); },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (dir, mode, opts) => {
      tracker.currentDirectory = "/real/show";
      tracker.lastScannedPath = "/real/show";
      tracker.lastManifest = {
        version: 2,
        items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
      };
      tracker.getSelectedOrderedItemIds = () => ["ep1.mkv"];
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 101;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 0);
  assert.ok(logs.some(message => message.includes("ep2.mkv") && message.includes("disappeared")));
});

test("start rejects selections that vanished during an earlier user rescan", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/show")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const logs = [];
  const calls = [];
  const coordinator = createJobCoordinator();
  const tracker = {
    currentDirectory: "/anime/show",
    lastScannedPath: "/anime/show",
    assignments: new Map(),
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    lastManifest: {
      version: 2,
      items: [
        { id: "ep1.mkv", name: "ep1.mkv" },
      ],
    },
    state: { totalCount: 1, vanishedSelectedIds: ["ep2.mkv"] },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog(_el, text) { logs.push(text); },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (dir, mode, opts) => {
      tracker.currentDirectory = "/anime/show";
      tracker.lastScannedPath = "/anime/show";
      tracker.lastManifest = {
        version: 2,
        items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
      };
      tracker.getSelectedOrderedItemIds = () => ["ep1.mkv"];
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 101;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 0);
  assert.ok(logs.some(message => message.includes("ep2.mkv") && message.includes("disappeared")));
});

test("start rejects when a vanished selection reappears during the forced rescan", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/show")],
    ["remux-out-dir", new FakeElement("")],
    ["remux-fonts-dir", new FakeElement("")],
    ["remux-assets-dir", new FakeElement("")],
    ["remux-sub-offset", new FakeElement("0")],
    ["remux-workers", new FakeElement("")],
    ["remux-io-jobs", new FakeElement("")],
    ["remux-ffmpeg-threads", new FakeElement("")],
    ["remux-bitrate", new FakeElement("")],
    ["remux-complexity", new FakeElement("")],
    ["remux-dry-run", new FakeElement(false)],
    ["remux-no-cache", new FakeElement(false)],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  const logs = [];
  const calls = [];
  const coordinator = createJobCoordinator();
  const tracker = {
    currentDirectory: "/anime/show",
    lastScannedPath: "/anime/show",
    assignments: new Map(),
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    lastManifest: {
      version: 2,
      items: [
        { id: "ep1.mkv", name: "ep1.mkv" },
      ],
    },
    state: { totalCount: 1, vanishedSelectedIds: ["ep2.mkv"] },
    handleEvent() {},
    finalize() {},
  };

  const runtime = {
    coordinator,
    appendLog(_el, text) { logs.push(text); },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (dir, mode, opts) => {
      tracker.currentDirectory = "/anime/show";
      tracker.lastScannedPath = "/anime/show";
      tracker.lastManifest = {
        version: 2,
        items: [
          { id: "ep1.mkv", name: "ep1.mkv" },
          { id: "ep2.mkv", name: "ep2.mkv" },
        ],
      };
      tracker.state = { totalCount: 2, vanishedSelectedIds: [] };
      tracker.getSelectedOrderedItemIds = () => ["ep1.mkv", "ep2.mkv"];
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(cmd, { request }) {
      if (cmd === "run_job") {
        calls.push(request);
        return 101;
      }
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(calls.length, 0);
  assert.ok(logs.some(message => message.includes("Queue selection changed during inspection")));
  assert.ok(logs.some(message => message.includes("press Run again")));
  assert.ok(logs.some(message => message.includes("ep2.mkv")));
});
