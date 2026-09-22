import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { JobController, extractInspectionOptions } from "./job-controller.js";

class FakeElement {
  constructor(value = "") {
    this.value = value;
    this.checked = false;
    this.disabled = false;
    this.textContent = "";
    this.className = "";
    this.innerHTML = "";
    this.classList = { add() {}, remove() {} };
  }

  addEventListener() {}
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
    lastManifest: { version: 2, items: [] },
    state: { batchCancelled: false, batchSuccess: false },
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
    lastManifest: { version: 2, items: [] },
    state: { batchCancelled: false, batchSuccess: false },
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
    lastManifest: { version: 2, items: [] },
    state: { batchCancelled: false, batchSuccess: false },
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
    lastManifest: { version: 2, items: [] },
    state: { batchCancelled: false, batchSuccess: false },
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
  const elements = new Map([
    ["remux-input-dir", new FakeElement("/my-anime")],
    ["remux-workers", new FakeElement("4")],
    ["remux-io-jobs", new FakeElement("2")],
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
    ["remux-keep-fonts", Object.assign(new FakeElement(), { checked: true })],
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
  assert.equal(req.keep_fonts, true);
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
    ["remux-keep-fonts", new FakeElement(false)],
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
    ["remux-keep-fonts", new FakeElement(false)],
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

