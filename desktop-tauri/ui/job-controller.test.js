import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { BULK_SELECTION_CONTROLS, JobController, extractInspectionOptions } from "./job-controller.js";
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

test("job controller completes inspect-run-event lifecycle and restores controls", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/input")],
    ["remux-workers", new FakeElement("4")],
    ["remux-io-jobs", new FakeElement("2")],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const handlers = new Map();
  let removedListeners = 0;
  const invocations = [];
  const handledEvents = [];
  const tracker = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent: event => handledEvents.push(event),
    finalize(code) {
      this.state = { batchCancelled: false, batchSuccess: code === 0 };
    },
  };
  const coordinator = createJobCoordinator();
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
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => { removedListeners++; };
    },
    async invoke(command, args) {
      invocations.push([command, args]);
      if (command === "run_job") return 7;
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.lifecycle, "running");
  assert.equal(controller.jobId, 7);
  assert.equal(invocations[0][0], "run_job");
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 7 } });
  assert.equal(handledEvents.length, 1);

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 7, code: 0 } });
  assert.equal(controller.lifecycle, "done");
  assert.equal(coordinator.activeMode, null);
  assert.equal(elements.get("remux-run").disabled, false);
  assert.equal(removedListeners, 3);
});

test("job controller buffers early events and filters stale events while run_job is in flight", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/input")],
    ["remux-workers", new FakeElement("4")],
    ["remux-io-jobs", new FakeElement("2")],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const handlers = new Map();
  let removedListeners = 0;
  const handledEvents = [];
  let resolveRunJob;
  const runJobPromise = new Promise(resolve => {
    resolveRunJob = resolve;
  });

  const tracker = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent: event => handledEvents.push(event),
    finalize(code) {
      this.state = { batchCancelled: false, batchSuccess: code === 0 };
    },
  };
  const coordinator = createJobCoordinator();
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
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => { removedListeners++; };
    },
    async invoke(command, args) {
      if (command === "run_job") return runJobPromise;
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();

  await new Promise(resolve => setTimeout(resolve, 0));

  assert.equal(controller.lifecycle, "running");
  assert.equal(controller.jobId, null);

  // 1. Stale event and stale job-done from an earlier job (job_id: 6) arrive while run_job is in flight
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 6 } });
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });

  // 2. Early event for current job (job_id: 7) arrives while run_job is in flight
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 7 } });

  // Neither stale nor early events should be dispatched to tracker while jobId is null,
  // and stale job-done must NOT complete the new job.
  assert.equal(handledEvents.length, 0);
  assert.equal(controller.lifecycle, "running");

  // 3. run_job resolves with job_id: 7
  resolveRunJob(7);
  await startPromise;

  assert.equal(controller.jobId, 7);
  assert.equal(controller.lifecycle, "running");
  // Stale event (job 6) is filtered out; early event (job 7) is replayed to tracker
  assert.equal(handledEvents.length, 1);
  assert.equal(handledEvents[0].job_id, 7);

  // 4. Stale event or done from job 6 arriving after launch are ignored
  handlers.get("job-event")({ payload: { version: 1, type: "item_complete", mode: "remux", job_id: 6 } });
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });
  assert.equal(handledEvents.length, 1);
  assert.equal(controller.lifecycle, "running");

  // 5. Valid job-done for job 7 completes the job
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 7, code: 0 } });
  assert.equal(controller.lifecycle, "done");
  assert.equal(controller.jobId, null);
  assert.equal(coordinator.activeMode, null);
});

test("job controller replays early job-done arriving while run_job is in flight", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/input")],
    ["remux-workers", new FakeElement("4")],
    ["remux-io-jobs", new FakeElement("2")],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const handlers = new Map();
  let resolveRunJob;
  const runJobPromise = new Promise(resolve => {
    resolveRunJob = resolve;
  });

  const tracker = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent() {},
    finalize(code) {
      this.state = { batchCancelled: false, batchSuccess: code === 0 };
    },
  };
  const coordinator = createJobCoordinator();
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
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => {};
    },
    async invoke(command, args) {
      if (command === "run_job") return runJobPromise;
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();

  await new Promise(resolve => setTimeout(resolve, 0));

  // Early job-done for job 8 arrives while run_job is in flight
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 8, code: 0 } });

  assert.equal(controller.lifecycle, "running");

  // run_job resolves with job_id: 8
  resolveRunJob(8);
  await startPromise;

  // Early done should be replayed and complete the job
  assert.equal(controller.lifecycle, "done");
  assert.equal(controller.jobId, null);
  assert.equal(coordinator.activeMode, null);
});

test("extractInspectionOptions parses valid options, preserves 0, and rejects invalid values", () => {
  // Valid combined - snake_case
  const res1 = extractInspectionOptions({ workers: 4, io_jobs: 2, no_cache: true });
  assert.deepEqual(res1.options, { workers: 4, ioJobs: 2, noCache: true });
  assert.equal(res1.error, undefined);

  // Valid combined - camelCase
  const res2 = extractInspectionOptions({ workers: 4, ioJobs: 2, noCache: true });
  assert.deepEqual(res2.options, { workers: 4, ioJobs: 2, noCache: true });
  assert.equal(res2.error, undefined);

  // Preserves 0 for io_jobs and ioJobs
  assert.equal(extractInspectionOptions({ io_jobs: 0 }).options.ioJobs, 0);
  assert.equal(extractInspectionOptions({ ioJobs: 0 }).options.ioJobs, 0);

  // Defaults on empty / null / undefined fields
  assert.deepEqual(extractInspectionOptions({}).options, {});
  assert.deepEqual(extractInspectionOptions(null).options, {});
  assert.deepEqual(extractInspectionOptions({ workers: null, io_jobs: null, no_cache: false }).options, {});
  assert.deepEqual(extractInspectionOptions({ workers: null, ioJobs: null, noCache: false }).options, {});

  // Boundary workers
  assert.equal(extractInspectionOptions({ workers: 1 }).options.workers, 1);
  assert.equal(extractInspectionOptions({ workers: 64 }).options.workers, 64);
  assert.ok(extractInspectionOptions({ workers: 0 }).error);
  assert.ok(extractInspectionOptions({ workers: -1 }).error);
  assert.ok(extractInspectionOptions({ workers: 65 }).error);
  assert.ok(extractInspectionOptions({ workers: "abc" }).error);
  assert.ok(extractInspectionOptions({ workers: 4.5 }).error);
  assert.ok(extractInspectionOptions({ workers: NaN }).error);

  // Boundary io-jobs (snake_case and camelCase)
  assert.equal(extractInspectionOptions({ io_jobs: 64 }).options.ioJobs, 64);
  assert.equal(extractInspectionOptions({ ioJobs: 64 }).options.ioJobs, 64);
  assert.ok(extractInspectionOptions({ io_jobs: -1 }).error);
  assert.ok(extractInspectionOptions({ ioJobs: -1 }).error);
  assert.ok(extractInspectionOptions({ io_jobs: 65 }).error);
  assert.ok(extractInspectionOptions({ ioJobs: 65 }).error);
  assert.ok(extractInspectionOptions({ io_jobs: "abc" }).error);
  assert.ok(extractInspectionOptions({ ioJobs: "abc" }).error);
  assert.ok(extractInspectionOptions({ io_jobs: 1.5 }).error);
  assert.ok(extractInspectionOptions({ ioJobs: 1.5 }).error);
  assert.ok(extractInspectionOptions({ io_jobs: NaN }).error);
  assert.ok(extractInspectionOptions({ ioJobs: NaN }).error);

  // noCache / no_cache
  assert.equal(extractInspectionOptions({ no_cache: true }).options.noCache, true);
  assert.equal(extractInspectionOptions({ no_cache: false }).options.noCache, undefined);
  assert.equal(extractInspectionOptions({ noCache: true }).options.noCache, true);
  assert.equal(extractInspectionOptions({ noCache: false }).options.noCache, undefined);
});

test("job controller forwards snapshotted execution args incl zero io-jobs and no-cache, ignoring subsequent DOM mutations", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/my-anime")],
    ["remux-workers", new FakeElement("6")],
    ["remux-io-jobs", new FakeElement("0")],
    ["remux-no-cache", Object.assign(new FakeElement(), { checked: true })],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const scanCalls = [];
  const workerChipCalls = [];
  const invocations = [];
  const tracker = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent() {},
    finalize() {},
  };
  const coordinator = createJobCoordinator();
  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (path, mode, opts) => {
      scanCalls.push({ path, mode, opts });
      // Mutate DOM during in-flight scan to ensure snapshot is used
      elements.get("remux-workers").value = "32";
      elements.get("remux-io-jobs").value = "16";
      elements.get("remux-input-dir").value = "/tampered";
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips: (...args) => {
      workerChipCalls.push(args);
    },
    async listen() {
      return () => {};
    },
    async invoke(command, args) {
      invocations.push([command, args]);
      if (command === "run_job") return 10;
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  // Verify scanInputDirectory received snapshotted options with workers=6, ioJobs=0, noCache=true
  assert.equal(scanCalls.length, 1);
  assert.equal(scanCalls[0].path, "/my-anime");
  assert.equal(scanCalls[0].mode, "remux");
  assert.deepEqual(scanCalls[0].opts, {
    force: true,
    options: { workers: 6, ioJobs: 0, noCache: true },
  });

  // Verify worker chips received (true, 6, 0) - 0 is not coerced!
  assert.deepEqual(workerChipCalls[0], [true, 6, 0]);

  // Verify run_job request still reflects original snapshot, not mutated DOM
  const runJobCall = invocations.find(([cmd]) => cmd === "run_job");
  const runJobReq = runJobCall[1].request;
  assert.equal(runJobReq.input_dir, "/my-anime");
  assert.equal(runJobReq.workers, 6);
  assert.equal(runJobReq.io_jobs, 0);
  assert.equal(runJobReq.no_cache, true);
});

test("deferred preflight stop releases ownership, finalizes cancelled queue, and avoids run_job for option-bearing preflight", async t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/my-anime")],
    ["remux-workers", new FakeElement("4")],
    ["remux-io-jobs", new FakeElement("2")],
    ["remux-no-cache", Object.assign(new FakeElement(), { checked: true })],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
    ...bulkButtons,
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  let resolveScan;
  const scanPromise = new Promise(resolve => {
    resolveScan = resolve;
  });
  const scanCalls = [];
  const invocations = [];
  let finalizedWith = null;

  const tracker = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent() {},
    finalize(code, cancelled = false) {
      finalizedWith = { code, cancelled };
      this.state = { batchCancelled: cancelled, batchSuccess: !cancelled && code === 0 };
    },
  };
  const coordinator = createJobCoordinator();
  const runtime = {
    coordinator,
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener() {},
    scanInputDirectory: async (path, mode, opts) => {
      scanCalls.push({ path, mode, opts });
      return scanPromise;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke(command, args) {
      invocations.push([command, args]);
      return null;
    },
  };

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();

  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(scanCalls.length, 1);
  assert.deepEqual(scanCalls[0].opts.options, { workers: 4, ioJobs: 2, noCache: true });
  for (const [, button] of bulkButtons) assert.equal(button.disabled, true, "bulk control must be disabled while preflight is pending");
  assert.equal(controller.applyBulkSelection("remux", false), false);

  await controller.stop();

  resolveScan(true);
  await startPromise;

  assert.equal(invocations.filter(([cmd]) => cmd === "run_job").length, 0);
  assert.equal(coordinator.activeMode, null);
  assert.equal(coordinator.canStart(), true);
  assert.deepEqual(finalizedWith, { code: -1, cancelled: true });
  assert.equal(tracker.state.batchCancelled, true);
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(controller.running, false);
  for (const [, button] of bulkButtons) assert.equal(button.disabled, false, "bulk control must return after abort");
});

test("job controller fails immediately when workers or io-jobs are invalid", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/input")],
    ["remux-workers", new FakeElement("0")], // invalid worker count
    ["remux-io-jobs", new FakeElement("2")],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const errorLogs = [];
  let scanCalled = false;
  const coordinator = createJobCoordinator();
  const runtime = {
    coordinator,
    appendLog(el, msg) {
      errorLogs.push(msg);
    },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => ({ assignments: new Map() }),
    safeAddListener() {},
    scanInputDirectory: async () => {
      scanCalled = true;
      return true;
    },
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke() { return null; },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(scanCalled, false);
  assert.equal(controller.running, false);
  assert.ok(errorLogs.some(msg => msg.includes("Invalid workers value '0'")));
});

test("job controller passes typed BatchRequest with track assignments to run_job", async t => {
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/anime/series")],
    ["remux-out-dir", new FakeElement("/anime/out")],
    ["remux-fonts-dir", new FakeElement("/anime/fonts")],
    ["remux-bitrate", new FakeElement("256k")],
    ["remux-complexity", new FakeElement("8")],
    ["remux-workers", new FakeElement("6")],
    ["remux-io-jobs", new FakeElement("3")],
    ["remux-ffmpeg-threads", new FakeElement("4")],
    ["remux-sub-offset", new FakeElement("1.5")],
    ["remux-dry-run", Object.assign(new FakeElement(), { checked: false })],
    ["remux-no-cache", Object.assign(new FakeElement(), { checked: true })],
    ["remux-run", new FakeElement()],
    ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: sel => {
      if (sel === ".profile-option-item.active") {
        return { dataset: { preset: "balanced" } };
      }
      return null;
    },
  };
  t.after(() => { globalThis.document = previousDocument; });

  const invocations = [];
  const coordinator = createJobCoordinator();
  const queue = {
    assignments: new Map([["ep1.mkv", "/subs/ep1.ass"]]),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
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
  assert.ok(req);
  assert.equal(req.mode, "remux");
  assert.equal(req.input_dir, "/anime/series");
  assert.equal(req.out_dir, "/anime/out");
  assert.equal(req.fonts_dir, "/anime/fonts");
  assert.equal(req.bitrate, "256k");
  assert.equal(req.complexity, 8);
  assert.equal(req.workers, 6);
  assert.equal(req.io_jobs, 3);
  assert.equal(req.ffmpeg_threads, 4);
  assert.equal(req.sub_offset, 1.5);
  assert.equal(req.no_cache, true);
  assert.equal(req.preset, "balanced");
  assert.ok(Array.isArray(req.assignments));
  assert.equal(req.assignments.length, 1);
  assert.equal(req.assignments[0].target_video, "ep1.mkv");
  assert.equal(req.assignments[0].asset_path, "/subs/ep1.ass");
});

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
  assert.ok(logs.some(message => message.includes("ep2.mkv")));
});

test("job controller aborts start if episodes exist in queue but zero are selected", async t => {
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

  const logs = [];
  const coordinator = createJobCoordinator();
  const queue = {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { totalCount: 1 },
    getSelectedOrderedItemIds: () => [],
    handleEvent() {},
    finalize() {},
  };
  const runtime = {
    coordinator,
    appendLog(el, msg) { logs.push(msg); },
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => queue,
    safeAddListener() {},
    scanInputDirectory: async () => true,
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    async listen() { return () => {}; },
    async invoke() { return null; },
  };

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.running, false);
  assert.ok(logs.some(l => l.includes("No episodes selected in queue")));
});
test("job controller disables run button while queue tracker isScanning", () => {
  const runBtn = new FakeElement();
  const elements = new Map([
    ["remux-run", runBtn],
    ["remux-stop", new FakeElement()],
    ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  const coordinator = createJobCoordinator();
  const tracker = {
    isScanning: true,
    state: { totalCount: 2 },
    getSelectedCount: () => 2,
  };
  const runtime = {
    coordinator,
    queueFor: () => tracker,
    safeAddListener() {},
  };
  try {
    const controller = new JobController("remux", runtime);
    controller.syncGlobalControls();
    assert.equal(runBtn.disabled, true);

    tracker.isScanning = false;
    controller.syncGlobalControls();
    assert.equal(runBtn.disabled, false);
  } finally {
    globalThis.document = previousDocument;
  }
});

test("job controller onDone is ignored when jobId is null or not running", t => {
  const elements = new Map([
    ["remux-stop", new FakeElement()],
    ["remux-run", new FakeElement()],
    ["remux-status", new FakeElement()],
    ["remux-log", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  let finalized = false;
  const queue = {
    assignments: new Map(),
    finalize() { finalized = true; },
  };
  const runtime = {
    coordinator: createJobCoordinator(),
    queueFor: () => queue,
    safeAddListener() {},
    updateWorkerChips() {},
    telemetryLoop: { setJobActive() {} },
  };
  const controller = new JobController("remux", runtime);
  controller.running = false;
  controller.jobId = null;

  controller.onDone(0);
  assert.equal(finalized, false);

  controller.running = true;
  controller.jobId = null;
  controller.onDone(0);
  assert.equal(finalized, false);
});

test("job controller stop transitions lifecycle to cancelled when aborted before launch", async t => {
  const elements = new Map([
    ["remux-stop", new FakeElement()],
    ["remux-run", new FakeElement()],
    ["remux-status", new FakeElement()],
    ["remux-log", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  t.after(() => { globalThis.document = previousDocument; });

  let finalizedCancelled = null;
  const queue = {
    assignments: new Map(),
    finalize(code, cancelled) { finalizedCancelled = cancelled; },
  };
  const coordinator = createJobCoordinator();
  coordinator.begin("remux");
  let invokedCommand = null;
  const runtime = {
    coordinator,
    appendLog() {},
    queueFor: () => queue,
    safeAddListener() {},
    updateWorkerChips() {},
    telemetryLoop: { setJobActive() {} },
    invoke: async (cmd, args) => {
      invokedCommand = cmd;
      return null; // Normal successful resolve
    },
  };

  const controller = new JobController("remux", runtime);
  controller.running = true;
  controller.jobId = null;

  await controller.stop();
  assert.equal(invokedCommand, "stop_job");
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(finalizedCancelled, true);
  assert.equal(controller.running, false);
});

test("restored controller filters IDs, targets Stop, and releases controls on completion", async t => {
  const elements = new Map([
    ["remux-run", new FakeElement()], ["remux-stop", new FakeElement()],
    ["remux-log", new FakeElement()], ["remux-status", new FakeElement()],
  ]);
  const previousDocument = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
  };
  t.after(() => { globalThis.document = previousDocument; });

  const signals = [];
  const invokes = [];
  const telemetry = [];
  let removed = 0;
  const tracker = {
    state: { items: new Map([["ep", { progress: 70 }]]), batchSuccess: false },
    handleEvent: event => signals.push(event),
    finalize(code, cancelled) { this.state.batchSuccess = code === 0 && !cancelled; },
  };
  const coordinator = createJobCoordinator();
  const controller = new JobController("remux", {
    coordinator,
    queueFor: () => tracker,
    safeAddListener: (el, event, handler) => el.addEventListener(event, handler),
    updateWorkerChips: active => telemetry.push(active),
    telemetryLoop: { setJobActive: active => telemetry.push(active) },
    appendLog() {},
    invoke: async (command, args) => { invokes.push([command, args]); },
  });
  controller.restoreActiveSnapshot({ job_id: 42, status: "running" }, () => removed++, () => removed++);
  assert.equal(controller.lifecycle, "running");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(elements.get("remux-stop").disabled, false);
  assert.deepEqual(telemetry, [true, true]);

  controller.handleSnapshotSignal("event", { job_id: 41, mode: "remux", type: "item_progress", id: "ep", progress: 95 });
  controller.handleSnapshotSignal("event", { job_id: 42, mode: "extract", type: "item_progress", id: "ep", progress: 95 });
  controller.handleSnapshotSignal("event", { job_id: 42, mode: "remux", type: "item_progress", id: "ep", progress: 40 }, true);
  assert.equal(signals.length, 0);
  controller.handleSnapshotSignal("event", { job_id: 42, mode: "remux", type: "item_progress", id: "ep", progress: 85 });
  assert.equal(signals.length, 1);

  await elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(invokes, [["stop_job", { jobId: 42 }]]);
  assert.equal(elements.get("remux-stop").disabled, true);
  controller.handleSnapshotSignal("done", { job_id: 41, mode: "remux", code: 0 });
  assert.equal(controller.lifecycle, "stopping");
  controller.handleSnapshotSignal("done", { job_id: 42, mode: "remux", code: 130 });
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(controller.jobId, null);
  assert.equal(coordinator.activeMode, null);
  assert.equal(removed, 2);
  assert.deepEqual(telemetry, [true, true, false, false]);
  controller.handleSnapshotSignal("done", { job_id: 42, mode: "remux", code: 130 });
  assert.equal(removed, 2);
});

test("completion during a pending Stop preserves the terminal state if Stop rejects", async t => {
  const previousDocument = globalThis.document;
  globalThis.document = { getElementById: () => null, querySelectorAll: () => [] };
  t.after(() => { globalThis.document = previousDocument; });
  let rejectStop;
  const stopPromise = new Promise((_, reject) => { rejectStop = reject; });
  const tracker = {
    state: { items: new Map(), batchSuccess: false },
    finalize(code, cancelled) { this.state.batchSuccess = code === 0 && !cancelled; },
  };
  const coordinator = createJobCoordinator();
  const controller = new JobController("remux", {
    coordinator, queueFor: () => tracker, safeAddListener() {},
    updateWorkerChips() {}, telemetryLoop: { setJobActive() {} }, appendLog() {},
    invoke: async () => stopPromise,
  });
  controller.restoreActiveSnapshot({ job_id: 24, status: "running" }, () => {}, () => {});
  const stopping = controller.stop();
  controller.handleSnapshotSignal("done", { job_id: 24, mode: "remux", code: 130 });
  rejectStop(new Error("already finished"));
  await stopping;
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(tracker.state.batchSuccess, false);
  assert.equal(coordinator.activeMode, null);
});
