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
  // Valid combined
  const res1 = extractInspectionOptions(["--workers", "6", "--io-jobs", "0", "--no-cache"]);
  assert.deepEqual(res1.options, { workers: 6, ioJobs: 0, noCache: true });
  assert.equal(res1.error, undefined);

  // Defaults on empty
  const res2 = extractInspectionOptions([]);
  assert.deepEqual(res2.options, {});

  // Boundary workers
  assert.equal(extractInspectionOptions(["--workers", "1"]).options.workers, 1);
  assert.equal(extractInspectionOptions(["--workers", "64"]).options.workers, 64);
  assert.ok(extractInspectionOptions(["--workers", "0"]).error);
  assert.ok(extractInspectionOptions(["--workers", "-1"]).error);
  assert.ok(extractInspectionOptions(["--workers", "65"]).error);
  assert.ok(extractInspectionOptions(["--workers", "abc"]).error);

  // Boundary io-jobs
  assert.equal(extractInspectionOptions(["--io-jobs", "0"]).options.ioJobs, 0);
  assert.equal(extractInspectionOptions(["--io-jobs", "64"]).options.ioJobs, 64);
  assert.ok(extractInspectionOptions(["--io-jobs", "-1"]).error);
  assert.ok(extractInspectionOptions(["--io-jobs", "65"]).error);
  assert.ok(extractInspectionOptions(["--io-jobs", "NaN"]).error);

  // Invalid spellings
  for (const spelling of ["1e1", "4.0", "0x10", ""]) {
    assert.ok(extractInspectionOptions(["--workers", spelling]).error, `workers should reject '${spelling}'`);
    assert.ok(extractInspectionOptions(["--io-jobs", spelling]).error, `io-jobs should reject '${spelling}'`);
  }

  // Trailing flags
  assert.ok(extractInspectionOptions(["--workers"]).error, "trailing --workers should be rejected");
  assert.ok(extractInspectionOptions(["--io-jobs"]).error, "trailing --io-jobs should be rejected");
  assert.ok(extractInspectionOptions(["--input-dir", "/dir", "--workers"]).error, "trailing --workers after flags should be rejected");
  assert.ok(extractInspectionOptions(["--input-dir", "/dir", "--io-jobs"]).error, "trailing --io-jobs after flags should be rejected");
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

  // Verify run_job args still reflect original snapshot, not mutated DOM
  const runJobArgs = invocations.find(([cmd]) => cmd === "run_job")[1].args;
  assert.equal(runJobArgs[runJobArgs.indexOf("--input-dir") + 1], "/my-anime");
  assert.equal(runJobArgs[runJobArgs.indexOf("--workers") + 1], "6");
  assert.equal(runJobArgs[runJobArgs.indexOf("--io-jobs") + 1], "0");
  assert.ok(runJobArgs.includes("--no-cache"));
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
