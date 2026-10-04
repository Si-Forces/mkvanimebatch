import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { BULK_SELECTION_CONTROLS, JobController, validateConcurrency } from "./job-controller.js";

class FakeElement {
  constructor(value = "") {
    this.value = value;
    this.checked = false;
    this.disabled = false;
    this.textContent = "";
    this.className = "";
    this.innerHTML = "";
    this.dataset = {};
    this.classList = { add() {}, remove() {} };
  }

  addEventListener(type, handler) { (this.handlers ??= new Map()).set(type, handler); }
  focus() {}
}

const checkedBox = () => Object.assign(new FakeElement(), { checked: true });

/** A manual subtitle assignment in the shape app.js stores in `QueueTracker.assignments`. */
const subtitle = (id, path) => ({
  target_video: id,
  asset_path: path,
  track_type: "subtitle",
  language: "ind",
  title: null,
  offset_ms: null,
});

/** Remux controls every controller looks up, plus `values` keyed by id suffix. */
function remuxElements(values = {}) {
  const elements = new Map(["run", "stop", "log", "status"].map(id => [`remux-${id}`, new FakeElement()]));
  for (const [id, value] of Object.entries(values)) {
    elements.set(`remux-${id}`, value instanceof FakeElement ? value : new FakeElement(value));
  }
  return elements;
}

function installDocument(t, elements, overrides = {}) {
  const previous = globalThis.document;
  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: () => [],
    querySelector: () => null,
    ...overrides,
  };
  t.after(() => { globalThis.document = previous; });
}

function makeTracker(overrides = {}) {
  return {
    assignments: new Map(),
    lastManifest: { version: 2, items: [{ id: "ep1.mkv", name: "ep1.mkv" }] },
    state: { batchCancelled: false, batchSuccess: false },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    handleEvent() {},
    finalize(code, cancelled = false) {
      this.state = { batchCancelled: cancelled, batchSuccess: !cancelled && code === 0 };
    },
    ...overrides,
  };
}

function makeRuntime(tracker, overrides = {}) {
  return {
    coordinator: createJobCoordinator(),
    appendLog() {},
    clearLog() {},
    closeSubAssignModal() {},
    queueFor: () => tracker,
    safeAddListener: (el, event, handler) => el?.addEventListener(event, handler),
    scanInputDirectory: async () => true,
    showModal: async () => true,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    listen: async () => () => {},
    invoke: async () => null,
    ...overrides,
  };
}

const tick = () => new Promise(resolve => setTimeout(resolve, 0));

test("job controller completes inspect-run-event lifecycle and restores controls", async t => {
  const elements = remuxElements({ "input-dir": "/input", workers: "4", "io-jobs": "2" });
  installDocument(t, elements);

  const handlers = new Map();
  let removedListeners = 0;
  const invocations = [];
  const handledEvents = [];
  const tracker = makeTracker({ handleEvent: event => handledEvents.push(event) });
  const runtime = makeRuntime(tracker, {
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => { removedListeners++; };
    },
    async invoke(command, args) {
      invocations.push([command, args]);
      return command === "run_job" ? 7 : null;
    },
  });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.lifecycle, "running");
  assert.equal(controller.jobId, 7);
  assert.equal(invocations[0][0], "run_job");
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 7 } });
  assert.equal(handledEvents.length, 1);

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 7, code: 0 } });
  assert.equal(controller.lifecycle, "done");
  assert.equal(runtime.coordinator.activeMode, null);
  assert.equal(elements.get("remux-run").disabled, false);
  assert.equal(removedListeners, 3);
});

test("job controller buffers early events and filters stale events while run_job is in flight", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input", workers: "4", "io-jobs": "2" }));

  const handlers = new Map();
  const handledEvents = [];
  let resolveRunJob;
  const runJobPromise = new Promise(resolve => { resolveRunJob = resolve; });
  const runtime = makeRuntime(makeTracker({ handleEvent: event => handledEvents.push(event) }), {
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => {};
    },
    invoke: async command => (command === "run_job" ? runJobPromise : null),
  });

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();
  await tick();

  assert.equal(controller.lifecycle, "running");
  assert.equal(controller.jobId, null);

  // A stale event and job-done from an earlier job arrive while run_job is in flight,
  // followed by an early event for the job being launched.
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 6 } });
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });
  handlers.get("job-event")({ payload: { version: 1, type: "item_start", mode: "remux", job_id: 7 } });

  // Nothing is dispatched while the job ID is unknown, and the stale done must not complete the job.
  assert.equal(handledEvents.length, 0);
  assert.equal(controller.lifecycle, "running");

  resolveRunJob(7);
  await startPromise;

  assert.equal(controller.jobId, 7);
  assert.equal(controller.lifecycle, "running");
  // Only the current job's buffered event is replayed.
  assert.equal(handledEvents.length, 1);
  assert.equal(handledEvents[0].job_id, 7);

  handlers.get("job-event")({ payload: { version: 1, type: "item_complete", mode: "remux", job_id: 6 } });
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });
  assert.equal(handledEvents.length, 1);
  assert.equal(controller.lifecycle, "running");

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 7, code: 0 } });
  assert.equal(controller.lifecycle, "done");
  assert.equal(controller.jobId, null);
  assert.equal(runtime.coordinator.activeMode, null);
});

test("job controller replays early job-done arriving while run_job is in flight", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input", workers: "4", "io-jobs": "2" }));

  const handlers = new Map();
  let resolveRunJob;
  const runJobPromise = new Promise(resolve => { resolveRunJob = resolve; });
  const runtime = makeRuntime(makeTracker(), {
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => {};
    },
    invoke: async command => (command === "run_job" ? runJobPromise : null),
  });

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();
  await tick();

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 8, code: 0 } });
  assert.equal(controller.lifecycle, "running");

  resolveRunJob(8);
  await startPromise;

  assert.equal(controller.lifecycle, "done");
  assert.equal(controller.jobId, null);
  assert.equal(runtime.coordinator.activeMode, null);
});

test("validateConcurrency accepts the documented bounds, preserves 0 io-jobs, and rejects everything else", () => {
  assert.equal(validateConcurrency({}), null);
  assert.equal(validateConcurrency({ workers: null, io_jobs: null, ffmpeg_threads: null }), null);
  assert.equal(validateConcurrency({ workers: 1, io_jobs: 0 }), null);
  assert.equal(validateConcurrency({ workers: 64, io_jobs: 64 }), null);
  assert.equal(validateConcurrency({ ffmpeg_threads: 1 }), null);
  assert.equal(validateConcurrency({ ffmpeg_threads: 32 }), null);
  for (const workers of [0, -1, 65, "abc", 4.5, NaN]) {
    assert.ok(validateConcurrency({ workers }), `workers ${workers} must be rejected`);
  }
  for (const ioJobs of [-1, 65, "abc", 1.5, NaN]) {
    assert.ok(validateConcurrency({ io_jobs: ioJobs }), `io_jobs ${ioJobs} must be rejected`);
  }
  for (const ffmpegThreads of [0, -1, 33, "abc", 2.5, NaN]) {
    assert.ok(validateConcurrency({ ffmpeg_threads: ffmpegThreads }), `ffmpeg_threads ${ffmpegThreads} must be rejected`);
  }
});

test("job controller forwards snapshotted execution args incl zero io-jobs and no-cache, ignoring subsequent DOM mutations", async t => {
  const elements = remuxElements({ "input-dir": "/my-anime", workers: "6", "io-jobs": "0", "no-cache": checkedBox() });
  installDocument(t, elements);

  const scanCalls = [];
  const workerChipCalls = [];
  const invocations = [];
  const runtime = makeRuntime(makeTracker(), {
    scanInputDirectory: async (path, mode, opts) => {
      scanCalls.push({ path, mode, opts });
      // The DOM changes while the forced scan is in flight; the request must not follow it.
      elements.get("remux-workers").value = "32";
      elements.get("remux-io-jobs").value = "16";
      elements.get("remux-input-dir").value = "/tampered";
      return true;
    },
    updateWorkerChips: (...args) => workerChipCalls.push(args),
    async invoke(command, args) {
      invocations.push([command, args]);
      return command === "run_job" ? 10 : null;
    },
  });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(scanCalls.length, 1);
  assert.equal(scanCalls[0].path, "/my-anime");
  assert.equal(scanCalls[0].mode, "remux");
  assert.deepEqual(scanCalls[0].opts, {
    force: true,
    options: { noCache: true },
  });

  // io-jobs 0 reaches the chips uncoerced.
  assert.deepEqual(workerChipCalls[0], [true, 6, 0]);

  const runJobReq = invocations.find(([cmd]) => cmd === "run_job")[1].request;
  assert.equal(runJobReq.input_dir, "/my-anime");
  assert.equal(runJobReq.workers, 6);
  assert.equal(runJobReq.io_jobs, 0);
  assert.equal(runJobReq.no_cache, true);
});

test("deferred preflight stop releases ownership, finalizes cancelled queue, and avoids run_job for option-bearing preflight", async t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = remuxElements({ "input-dir": "/my-anime", workers: "4", "io-jobs": "2", "no-cache": checkedBox() });
  for (const [id, button] of bulkButtons) elements.set(id, button);
  installDocument(t, elements);

  let resolveScan;
  const scanPromise = new Promise(resolve => { resolveScan = resolve; });
  const scanCalls = [];
  const invocations = [];
  let finalizedWith = null;

  const tracker = makeTracker({
    lastManifest: { version: 2, items: [] },
    finalize(code, cancelled = false) {
      finalizedWith = { code, cancelled };
      this.state = { batchCancelled: cancelled, batchSuccess: !cancelled && code === 0 };
    },
  });
  const runtime = makeRuntime(tracker, {
    scanInputDirectory: async (path, mode, opts) => {
      scanCalls.push({ path, mode, opts });
      return scanPromise;
    },
    async invoke(command, args) {
      invocations.push([command, args]);
      return null;
    },
  });
  const { coordinator } = runtime;

  const controller = new JobController("remux", runtime);
  const startPromise = controller.start();

  await tick();
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(scanCalls.length, 1);
  assert.deepEqual(scanCalls[0].opts.options, { noCache: true });
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

test("job controller rejects invalid concurrency field values before preflight", async t => {
  for (const [field, value] of [
    ["workers", "0"],
    ["workers", "1.5"],
    ["io-jobs", "64.9"],
    ["ffmpeg-threads", "33"],
    ["ffmpeg-threads", "1.5"],
    ["ffmpeg-threads", "32.9"],
    ["ffmpeg-threads", "1e2"],
  ]) {
    await t.test(`${field}=${value}`, async t => {
      installDocument(t, remuxElements({ "input-dir": "/input", [field]: value }));
      const logs = [];
      let scanCalled = false;
      const runtime = makeRuntime(makeTracker(), {
        appendLog: (_el, msg) => logs.push(msg),
        scanInputDirectory: async () => {
          scanCalled = true;
          return true;
        },
      });

      const controller = new JobController("remux", runtime);
      await controller.start();

      assert.equal(scanCalled, false);
      assert.equal(controller.running, false);
      assert.equal(runtime.coordinator.activeMode, null);
      assert.ok(logs.some(msg => msg.includes(`Invalid ${field} value`)));
    });
  }
});

test("job controller passes typed BatchRequest with track assignments to run_job", async t => {
  installDocument(t, remuxElements({
    "input-dir": "/anime/series",
    "out-dir": "/anime/out",
    "fonts-dir": "/anime/fonts",
    bitrate: "256k",
    complexity: "8",
    workers: "6",
    "io-jobs": "3",
    "ffmpeg-threads": "4",
    "sub-offset": "1.5",
    "dry-run": new FakeElement(),
    "no-cache": checkedBox(),
  }), {
    querySelector: sel => (sel === ".profile-option-item.active" ? { dataset: { preset: "balanced" } } : null),
  });

  const invocations = [];
  const queue = makeTracker({ assignments: new Map([["ep1.mkv", subtitle("ep1.mkv", "/subs/ep1.ass")]]) });
  const runtime = makeRuntime(queue, {
    async invoke(command, args) {
      invocations.push([command, args]);
      return command === "run_job" ? 42 : null;
    },
  });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.jobId, 42);
  const req = invocations.find(([cmd]) => cmd === "run_job")[1].request;
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
  assert.equal(req.dry_run, false);
  assert.equal(req.no_cache, true);
  assert.equal(req.preset, "balanced");
  assert.deepEqual(req.assignments.map(a => [a.target_video, a.asset_path]), [["ep1.mkv", "/subs/ep1.ass"]]);
});

test("job controller aborts start if episodes exist in queue but zero are selected", async t => {
  installDocument(t, remuxElements({ "input-dir": "/anime/series", "sub-offset": "0" }));

  const logs = [];
  const queue = makeTracker({ state: { totalCount: 1 }, getSelectedOrderedItemIds: () => [] });
  const runtime = makeRuntime(queue, { appendLog: (_el, msg) => logs.push(msg) });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.running, false);
  assert.ok(logs.some(l => l.includes("No episodes selected in queue")));
});

test("job controller disables Run while scanning or snapshot restore blocks start", t => {
  const elements = remuxElements();
  installDocument(t, elements);
  const tracker = { isScanning: true, state: { totalCount: 2 }, getSelectedCount: () => 2 };
  const runtime = makeRuntime(tracker);
  const runBtn = elements.get("remux-run");

  const controller = new JobController("remux", runtime);
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);

  tracker.isScanning = false;
  runtime.coordinator.isRestoring = true;
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, true);

  runtime.coordinator.isRestoring = false;
  controller.syncGlobalControls();
  assert.equal(runBtn.disabled, false);
});

test("job controller onDone is ignored when jobId is null or not running", t => {
  installDocument(t, remuxElements());

  let finalized = false;
  const controller = new JobController("remux", makeRuntime({ finalize() { finalized = true; } }));
  controller.running = false;
  controller.jobId = null;
  controller.onDone(0);
  assert.equal(finalized, false);

  controller.running = true;
  controller.onDone(0);
  assert.equal(finalized, false);
});

test("job controller stop transitions lifecycle to cancelled when aborted before launch", async t => {
  installDocument(t, remuxElements());

  let finalizedCancelled = null;
  let invokedCommand = null;
  const runtime = makeRuntime({ finalize(_code, cancelled) { finalizedCancelled = cancelled; } }, {
    invoke: async cmd => {
      invokedCommand = cmd;
      return null;
    },
  });
  runtime.coordinator.begin("remux");

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
  const elements = remuxElements();
  installDocument(t, elements);

  const signals = [];
  const invokes = [];
  const telemetry = [];
  let removed = 0;
  const tracker = {
    state: { items: new Map([["ep", { progress: 70 }]]), batchSuccess: false },
    handleEvent: event => signals.push(event),
    finalize(code, cancelled) { this.state.batchSuccess = code === 0 && !cancelled; },
  };
  const runtime = makeRuntime(tracker, {
    updateWorkerChips: active => { telemetry.push(active); },
    telemetryLoop: { setJobActive: active => { telemetry.push(active); } },
    invoke: async (command, args) => { invokes.push([command, args]); },
  });
  const { coordinator } = runtime;
  const controller = new JobController("remux", runtime);
  controller.restoreActiveSnapshot({ job_id: 42, status: "running" }, () => removed++, () => removed++);
  assert.equal(controller.lifecycle, "running");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(elements.get("remux-stop").disabled, false);
  // Asserted as a sequence, not a final state: it also pins the call count, so a
  // controller that re-pings telemetry on every transition fails here.
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
  controller.handleSnapshotSignal("done", { job_id: 42, mode: "remux", code: -1 });
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(controller.jobId, null);
  assert.equal(coordinator.activeMode, null);
  assert.equal(removed, 2);
  assert.deepEqual(telemetry, [true, true, false, false]);
  controller.handleSnapshotSignal("done", { job_id: 42, mode: "remux", code: -1 });
  assert.equal(removed, 2);
});

test("Stop logs the shutdown wait before stop_job resolves", async t => {
  installDocument(t, new Map());
  const logs = [];
  let resolveStop;
  const tracker = { state: { items: new Map() }, finalize() {} };
  const runtime = makeRuntime(tracker, {
    appendLog: (_el, line) => logs.push(line),
    invoke: () => new Promise(resolve => { resolveStop = resolve; }),
  });
  const controller = new JobController("remux", runtime);
  controller.restoreActiveSnapshot({ job_id: 24, status: "running" }, () => {}, () => {});

  const stopping = controller.stop();
  assert.equal(typeof resolveStop, "function", "stop_job must be pending");
  assert.deepEqual(logs, ["[INFO] Stop requested; waiting for shutdown."]);

  resolveStop();
  await stopping;
  assert.equal(logs.length, 1);
});

/** A controller restored onto running job 24 whose `stop_job` call rejects on demand. */
function restoredControllerWithPendingStop(t) {
  installDocument(t, new Map());
  let rejectStop;
  const stopPromise = new Promise((_, reject) => { rejectStop = reject; });
  const tracker = {
    state: { items: new Map(), batchSuccess: false },
    finalize(code, cancelled) { this.state.batchSuccess = code === 0 && !cancelled; },
  };
  const runtime = makeRuntime(tracker, { invoke: async () => stopPromise });
  const controller = new JobController("remux", runtime);
  controller.restoreActiveSnapshot({ job_id: 24, status: "running" }, () => {}, () => {});
  return { controller, tracker, coordinator: runtime.coordinator, stopping: controller.stop(), rejectStop };
}

test("a rejected Stop with no terminal yet reports the job as still running, then settles on job-done", async t => {
  const { controller, coordinator, stopping, rejectStop } = restoredControllerWithPendingStop(t);

  // stop_job only rejects while the Engine Bridge still owns the job slot, so the
  // job really is still running and the controller must say so rather than leave the
  // controls frozen in `stopping`. The supervisor's own job-done still settles it.
  rejectStop(new Error("Timed out waiting for job slot release"));
  await stopping;
  assert.equal(controller.lifecycle, "running");
  assert.equal(coordinator.isStopping, false);
  assert.equal(coordinator.activeMode, "remux");

  controller.handleSnapshotSignal("done", { job_id: 24, mode: "remux", code: -1 });
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(coordinator.activeMode, null);
});

test("completion during a pending Stop preserves the terminal state if Stop rejects", async t => {
  const { controller, tracker, coordinator, stopping, rejectStop } = restoredControllerWithPendingStop(t);
  controller.handleSnapshotSignal("done", { job_id: 24, mode: "remux", code: -1 });
  rejectStop(new Error("already finished"));
  await stopping;
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(tracker.state.batchSuccess, false);
  assert.equal(coordinator.activeMode, null);
});
