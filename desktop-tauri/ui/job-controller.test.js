import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import { createJobCoordinator } from "./job-coordinator.js";
import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";

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
    manifestItemIds: () => ["ep1.mkv"],
    // What the Desktop Core publishes for a finished job before its `job-done`.
    state: { batchCancelled: false, batchSuccess: true },
    getSelectedOrderedItemIds: () => ["ep1.mkv"],
    ...overrides,
  };
}

/** The Effective Settings `resolve_batch_request` answers with unless a test says otherwise. */
const SETTINGS = { workers: 8, io_jobs: 2, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };

/**
 * A runtime of fakes. `resolve` answers `resolve_batch_request` (the request in, the Effective
 * Settings out, or a rejection) so `invoke` only sees the other commands.
 */
function makeRuntime(tracker, { invoke = async () => null, resolve = async () => SETTINGS, ...overrides } = {}) {
  return {
    coordinator: createJobCoordinator(),
    appendLog() {},
    clearLog() {},
    queueFor: () => tracker,
    safeAddListener: (el, event, handler) => el?.addEventListener(event, handler),
    scanInputDirectory: async () => true,
    showModal: async () => true,
    syncState: async () => null,
    telemetryLoop: { setJobActive() {} },
    updateWorkerChips() {},
    listen: async () => () => {},
    invoke: (command, args) => (command === "resolve_batch_request" ? resolve(args.request) : invoke(command, args)),
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
  const runtime = makeRuntime(makeTracker(), {
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
  assert.deepEqual([...handlers.keys()], ["job-log", "job-done"], "the queue follows view-state, not job events");

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 7, code: 0 } });
  assert.equal(controller.lifecycle, "done");
  assert.equal(runtime.coordinator.activeMode, null);
  assert.equal(elements.get("remux-run").disabled, false);
  assert.equal(removedListeners, 2);
});

test("a stale job-done heard while run_job is in flight is ignored, and the job's own one ends it", async t => {
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

  assert.equal(controller.lifecycle, "running");
  assert.equal(controller.jobId, null);

  // The job-done of an earlier job arrives while run_job is in flight.
  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });
  assert.equal(controller.lifecycle, "running");

  resolveRunJob(7);
  await startPromise;

  assert.equal(controller.jobId, 7);
  assert.equal(controller.lifecycle, "running");

  handlers.get("job-done")({ payload: { mode: "remux", job_id: 6, code: 0 } });
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

test("job controller forwards snapshotted execution args incl zero io-jobs and no-cache, ignoring subsequent DOM mutations", async t => {
  const elements = remuxElements({ "input-dir": "/my-anime", workers: "6", "io-jobs": "0", "no-cache": checkedBox() });
  installDocument(t, elements);

  const scanCalls = [];
  const workerChipCalls = [];
  const invocations = [];
  // The engine expands an explicit io-jobs of 0 to the worker count.
  const resolved = { ...SETTINGS, workers: 6, io_jobs: 6 };
  const runtime = makeRuntime(makeTracker(), {
    resolve: async () => resolved,
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
  assert.deepEqual(scanCalls[0].opts, { force: true, noCache: true });

  // The chips show what the engine resolved, not the request's own 0.
  assert.deepEqual(workerChipCalls[0], [true, resolved]);

  const runJobReq = invocations.find(([cmd]) => cmd === "run_job")[1].request;
  assert.equal(runJobReq.input_dir, "/my-anime");
  assert.equal(runJobReq.workers, 6);
  assert.equal(runJobReq.io_jobs, 0);
  assert.equal(runJobReq.no_cache, true);
});

/** Starts a Remux run with the given workers and Disk I/O fields; returns the requests sent to the engine, in order, and the chip calls. */
async function startWithWorkersField(t, { workers, ioJobs = "2", resolved }) {
  installDocument(t, remuxElements({ "input-dir": "/input", workers, "io-jobs": ioJobs }));

  const sent = [];
  const chipCalls = [];
  const runtime = makeRuntime(makeTracker(), {
    updateWorkerChips: (...args) => chipCalls.push(args),
    async resolve(request) {
      sent.push(["resolve_batch_request", request]);
      return resolved;
    },
    async invoke(command, args) {
      if (command === "run_job") sent.push([command, args.request]);
      return command === "run_job" ? 11 : null;
    },
  });
  await new JobController("remux", runtime).start();
  return { sent, chipCalls };
}

test("an empty workers field sends workers null and the chips show the Effective Settings the engine resolved", async t => {
  const resolved = { ...SETTINGS, workers: 6, io_jobs: 2 };
  const { sent, chipCalls } = await startWithWorkersField(t, { workers: "", resolved });
  assert.deepEqual(sent.map(([command]) => command), ["resolve_batch_request", "run_job"]);
  assert.equal(sent[0][1].workers, null);
  assert.equal(sent[1][1].workers, null);
  assert.deepEqual(chipCalls[0], [true, resolved]);
});

test("an empty Disk I/O field sends io_jobs null so the engine's default applies", async t => {
  const resolved = { ...SETTINGS, workers: 4, io_jobs: 2 };
  const { sent, chipCalls } = await startWithWorkersField(t, { workers: "4", ioJobs: "", resolved });
  assert.equal(sent[0][1].io_jobs, null);
  assert.equal(sent[1][1].io_jobs, null);
  assert.deepEqual(chipCalls[0], [true, resolved]);
});

test("a typed workers value is sent as is to both the resolve and the run", async t => {
  const resolved = { ...SETTINGS, workers: 3 };
  const { sent, chipCalls } = await startWithWorkersField(t, { workers: "3", resolved });
  assert.equal(sent[0][1].workers, 3);
  assert.equal(sent[1][1].workers, 3);
  assert.deepEqual(chipCalls[0], [true, resolved]);
});

test("start resolves the request before the forced scan, while selected_items and assignments are still unset", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input", workers: "4", "io-jobs": "2" }));

  const order = [];
  let resolvedRequest = null;
  const runtime = makeRuntime(makeTracker(), {
    async resolve(request) {
      order.push("resolve");
      resolvedRequest = structuredClone(request);
      return SETTINGS;
    },
    clearLog: () => order.push("clear-log"),
    async scanInputDirectory() {
      order.push("scan");
      return true;
    },
    async invoke(command) {
      order.push(command);
      return command === "run_job" ? 12 : null;
    },
  });

  const controller = new JobController("remux", runtime);
  order.length = 0; // the constructor clears the log once
  await controller.start();

  // The log is cleared once the request is accepted, not before.
  assert.deepEqual(order, ["resolve", "clear-log", "scan", "run_job"]);
  assert.equal(resolvedRequest.input_dir, "/input");
  assert.equal(resolvedRequest.selected_items, null);
  assert.equal(resolvedRequest.assignments, null);
});

test("a request the engine refuses is appended to the log, uncleared, and aborts the start before the scan", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input", workers: "0" }));
  const logs = [];
  const cleared = [];
  const chipCalls = [];
  const invocations = [];
  let scanCalled = false;
  const runtime = makeRuntime(makeTracker(), {
    resolve: async () => { throw "Worker thread count must be between 1 and 64"; },
    appendLog: (_el, msg) => logs.push(msg),
    clearLog: () => cleared.push("clear"),
    updateWorkerChips: (...args) => chipCalls.push(args),
    scanInputDirectory: async () => {
      scanCalled = true;
      return true;
    },
    async invoke(command) {
      invocations.push(command);
      return null;
    },
  });

  const controller = new JobController("remux", runtime);
  cleared.length = 0; // the constructor clears the log once
  await controller.start();

  // The log of an earlier run stays, with the refusal appended to it.
  assert.deepEqual(cleared, []);
  assert.equal(scanCalled, false);
  assert.deepEqual(invocations, []);
  assert.equal(controller.running, false);
  assert.equal(controller.lifecycle, "failed");
  assert.equal(runtime.coordinator.activeMode, null);
  assert.equal(runtime.coordinator.canStart(), true);
  assert.deepEqual(logs, [{ kind: "error", text: "ERROR: Worker thread count must be between 1 and 64" }]);
  // The chips were never switched to a running job.
  assert.deepEqual(chipCalls, [[false]]);
});

test("Stop while the engine resolves the request ends the start without a scan or a run", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input" }));
  let answer;
  const answered = new Promise(resolve => { answer = resolve; });
  const invocations = [];
  let scanCalled = false;
  const runtime = makeRuntime(makeTracker(), {
    resolve: () => answered,
    scanInputDirectory: async () => {
      scanCalled = true;
      return true;
    },
    async invoke(command) {
      invocations.push(command);
      return null;
    },
  });

  const controller = new JobController("remux", runtime);
  const starting = controller.start();
  await tick();
  assert.equal(runtime.coordinator.activeMode, "remux");

  await controller.stop();
  answer(SETTINGS);
  await starting;

  assert.equal(scanCalled, false);
  assert.deepEqual(invocations, ["stop_job"]);
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(runtime.coordinator.activeMode, null);
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
    assert.doesNotMatch(tag, /\svalue=/, `${id} must start empty so the request sends null and the engine's default applies`);
  }
});

test("deferred preflight stop releases ownership, leaves the queue untouched, and avoids run_job for option-bearing preflight", async t => {
  const bulkButtons = BULK_SELECTION_CONTROLS.map(({ id }) => [id, new FakeElement()]);
  const elements = remuxElements({ "input-dir": "/my-anime", workers: "4", "io-jobs": "2", "no-cache": checkedBox() });
  for (const [id, button] of bulkButtons) elements.set(id, button);
  installDocument(t, elements);

  let resolveScan;
  const scanPromise = new Promise(resolve => { resolveScan = resolve; });
  const scanCalls = [];
  const invocations = [];
  const tracker = makeTracker({ manifestItemIds: () => [] });
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
  assert.equal(scanCalls[0].opts.noCache, true);
  for (const [, button] of bulkButtons) assert.equal(button.disabled, true, "bulk control must be disabled while preflight is pending");
  assert.equal(controller.applyBulkSelection("remux", false), false);

  await controller.stop();

  resolveScan(true);
  await startPromise;

  assert.equal(invocations.filter(([cmd]) => cmd === "run_job").length, 0);
  assert.equal(coordinator.activeMode, null);
  assert.equal(coordinator.canStart(), true);
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(controller.running, false);
  for (const [, button] of bulkButtons) assert.equal(button.disabled, false, "bulk control must return after abort");
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
  assert.equal(req.default_sub_offset_ms, 1500);
  assert.equal("sub_offset" in req, false);
  assert.equal(req.dry_run, false);
  assert.equal(req.no_cache, true);
  assert.equal(req.preset, "balanced");
  assert.deepEqual(req.assignments.map(a => [a.target_video, a.asset_path]), [["ep1.mkv", "/subs/ep1.ass"]]);
});

test("job controller sends the seconds offset as whole milliseconds, keeping an explicit zero", async t => {
  for (const [seconds, expected] of [
    ["1.5", 1500],
    ["-0.5", -500],
    ["0", 0],
    ["-0", 0],
    ["", null],
    // Half a millisecond rounds away from zero in both directions, matching the CLI.
    ["0.0005", 1],
    ["-0.0005", -1],
  ]) {
    await t.test(JSON.stringify(seconds), async t => {
      installDocument(t, remuxElements({ "input-dir": "/anime/series", "sub-offset": seconds }));
      const invocations = [];
      const runtime = makeRuntime(makeTracker(), {
        async invoke(command, args) {
          invocations.push([command, args]);
          return command === "run_job" ? 1 : null;
        },
      });

      await new JobController("remux", runtime).start();

      const req = invocations.find(([cmd]) => cmd === "run_job")[1].request;
      assert.ok(Object.is(req.default_sub_offset_ms, expected), `got ${req.default_sub_offset_ms}`);
    });
  }
});

test("job controller rejects a sub-offset that is not a number of seconds", async t => {
  // 1e15 s is 1e18 ms: it fits i64, so the CLI accepts it, but it is not a safe JS integer.
  for (const value of ["abc", "1e400", "1e300", "1e15", "-1e15"]) {
    await t.test(value, async t => {
      installDocument(t, remuxElements({ "input-dir": "/input", "sub-offset": value }));
      const logs = [];
      const opened = [];
      let scanCalled = false;
      const runtime = makeRuntime(makeTracker(), {
        appendLog: (_el, msg) => logs.push(msg),
        openSettings: mode => opened.push(mode),
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
      assert.ok(logs.some(msg => msg.kind === "error" && msg.text.includes("Invalid sub-offset value")));
      // The field sits in the settings sheet, which opens so the error shows.
      assert.deepEqual(opened, ["remux"]);
    });
  }
});

test("job controller aborts start if episodes exist in queue but zero are selected", async t => {
  installDocument(t, remuxElements({ "input-dir": "/anime/series", "sub-offset": "0" }));

  const logs = [];
  const queue = makeTracker({ state: { totalCount: 1 }, getSelectedOrderedItemIds: () => [] });
  const runtime = makeRuntime(queue, { appendLog: (_el, msg) => logs.push(msg) });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.running, false);
  assert.ok(logs.some(l => l.text.includes("No episodes selected in queue")));
});

test("a refused run_job reports failed and leaves the queue as it was", async t => {
  installDocument(t, remuxElements({ "input-dir": "/input" }));
  const logs = [];
  const runtime = makeRuntime(makeTracker(), {
    appendLog: (_el, msg) => logs.push(msg),
    invoke: async command => {
      if (command === "run_job") throw new Error("Another job is already running");
      return null;
    },
  });

  const controller = new JobController("remux", runtime);
  await controller.start();

  assert.equal(controller.running, false);
  assert.equal(controller.lifecycle, "failed");
  assert.equal(runtime.coordinator.activeMode, null);
  assert.ok(logs.some(msg => msg.text.includes("Another job is already running")));
});

test("job controller disables Run while its queue scans or another mode's job runs", async t => {
  const dryRun = new FakeElement();
  const elements = remuxElements({ "dry-run": dryRun });
  installDocument(t, elements);
  const tracker = { isScanning: true, state: { totalCount: 2 }, getSelectedCount: () => 2 };
  const runtime = makeRuntime(tracker);
  const runBtn = elements.get("remux-run");

  const controller = new JobController("remux", runtime);
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

  // The other mode's job blocks this Run, and its tooltip says so.
  runtime.coordinator.begin("extract");
  assert.equal(runBtn.disabled, true);
  assert.equal(runBtn.title, "Extract batch is running");
});

test("job controller onDone is ignored when jobId is null or not running", t => {
  installDocument(t, remuxElements());

  const controller = new JobController("remux", makeRuntime(makeTracker()));
  controller.running = false;
  controller.jobId = null;
  controller.onDone(0);
  assert.equal(controller.lifecycle, "idle");

  controller.running = true;
  controller.onDone(0);
  assert.equal(controller.lifecycle, "idle");
});

test("job controller stop transitions lifecycle to cancelled when aborted before launch", async t => {
  installDocument(t, remuxElements());

  let invokedCommand = null;
  const runtime = makeRuntime(makeTracker(), {
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
  assert.equal(controller.running, false);
});


/** A Job View of running remux job `job_id`, as `get_state` answers on a webview (re)load. */
const runningJob = (job_id, fields = {}) => ({ job_id, mode: "remux", status: "running", active: true, effective_settings: null, ...fields });

/**
 * Adopts running job `jobId` into a remux controller whose runtime records `job-done` listeners
 * in `handlers`. `current` is the Job View the post-listen `get_state` answers with (still running
 * by default).
 */
async function adopted(t, jobId, { tracker = makeTracker(), current, ...runtimeOverrides } = {}) {
  const elements = remuxElements();
  installDocument(t, elements);
  const handlers = new Map();
  let removed = 0;
  const runtime = makeRuntime(tracker, {
    async listen(event, handler) {
      handlers.set(event, handler);
      return () => { removed++; };
    },
    syncState: async () => ({ job: current ?? runningJob(jobId) }),
    ...runtimeOverrides,
  });
  const controller = new JobController("remux", runtime);
  await controller.adopt(runningJob(jobId));
  const done = (job_id, code) => handlers.get("job-done")({ payload: { job_id, mode: "remux", code } });
  return { elements, handlers, runtime, controller, done, removed: () => removed };
}

test("an adopted job runs its mode, targets Stop with its ID, and only its own job-done releases the controls", async t => {
  const invokes = [];
  const telemetry = [];
  const { elements, runtime, controller, done, removed } = await adopted(t, 42, {
    tracker: makeTracker({ state: { batchCancelled: true, batchSuccess: false } }),
    updateWorkerChips: active => { telemetry.push(active); },
    telemetryLoop: { setJobActive: active => { telemetry.push(active); } },
    invoke: async (command, args) => { invokes.push([command, args]); },
  });
  const { coordinator } = runtime;
  assert.equal(controller.lifecycle, "running");
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(elements.get("remux-stop").disabled, false);
  assert.equal(elements.get("remux-run").disabled, true);
  // Asserted as a sequence, not a final state: it also pins the call count, so a
  // controller that re-pings telemetry on every transition fails here.
  assert.deepEqual(telemetry, [true, true]);

  await elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(invokes, [["stop_job", { jobId: 42 }]]);
  assert.equal(elements.get("remux-stop").disabled, true);
  done(41, 0);
  assert.equal(controller.lifecycle, "stopping");
  done(42, -1);
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(controller.jobId, null);
  assert.equal(coordinator.activeMode, null);
  assert.equal(elements.get("remux-run").disabled, false);
  assert.equal(removed(), 1);
  assert.deepEqual(telemetry, [true, true, false, false]);
  done(42, -1);
  assert.equal(removed(), 1);
});

test("a job that ended before its job-done listener bound ends on the state read after it", async t => {
  for (const [status, lifecycle] of [["completed", "done"], ["cancelled", "cancelled"], ["error", "failed"]]) {
    const tracker = makeTracker({ state: { batchCancelled: status === "cancelled", batchSuccess: status === "completed" } });
    const { controller, runtime } = await adopted(t, 42, { tracker, current: runningJob(42, { status, active: false }) });
    assert.equal(controller.lifecycle, lifecycle, status);
    assert.equal(runtime.coordinator.activeMode, null, status);
  }
});

test("a finished job only sets the status pill, and binds nothing", async t => {
  installDocument(t, remuxElements());
  for (const [status, lifecycle] of [["completed", "done"], ["cancelled", "cancelled"], ["error", "failed"]]) {
    const listened = [];
    const runtime = makeRuntime(makeTracker(), { listen: async event => { listened.push(event); return () => {}; } });
    const controller = new JobController("remux", runtime);
    await controller.adopt(runningJob(9, { status, active: false }));
    assert.equal(controller.lifecycle, lifecycle);
    assert.equal(runtime.coordinator.phase, "idle");
    assert.deepEqual(listened, []);
  }
});

test("an adopted job shows its Effective Settings on the chips, or none when it has none", async t => {
  const chipCalls = [];
  const settings = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };
  for (const effective_settings of [settings, null, undefined]) {
    installDocument(t, remuxElements());
    const runtime = makeRuntime(makeTracker(), { updateWorkerChips: (...args) => chipCalls.push(args) });
    await new JobController("remux", runtime).adopt(runningJob(5, { effective_settings }));
  }
  assert.deepEqual(chipCalls, [[true, settings], [true, null], [true, null]]);
});

/** A controller adopted onto running job 24 whose `stop_job` call settles on demand. */
async function adoptedWithPendingStop(t, runtimeOverrides = {}) {
  let resolveStop;
  let rejectStop;
  const stopPromise = new Promise((resolve, reject) => {
    resolveStop = resolve;
    rejectStop = reject;
  });
  const tracker = makeTracker({ state: { batchSuccess: false } });
  const { controller, runtime, done } = await adopted(t, 24, { tracker, invoke: async () => stopPromise, ...runtimeOverrides });
  return {
    controller,
    tracker,
    done,
    coordinator: runtime.coordinator,
    stopping: controller.stop(),
    resolveStop,
    rejectStop,
  };
}

test("Stop logs the shutdown wait before stop_job resolves", async t => {
  const logs = [];
  const { stopping, resolveStop } = await adoptedWithPendingStop(t, {
    appendLog: (_el, line) => logs.push(line),
  });

  assert.deepEqual(logs, [{ kind: "note", text: "Stop requested; waiting for shutdown." }]);

  resolveStop();
  await stopping;
  assert.equal(logs.length, 1);
});

test("a rejected Stop with no terminal yet reports the job as still running, then settles on job-done", async t => {
  const { controller, coordinator, stopping, rejectStop, done } = await adoptedWithPendingStop(t);

  // stop_job only rejects while the Engine Bridge still owns the job slot, so the
  // job really is still running and the controller must say so rather than leave the
  // controls frozen in `stopping`. The supervisor's own job-done still settles it.
  rejectStop(new Error("Timed out waiting for job slot release"));
  await stopping;
  assert.equal(controller.lifecycle, "running");
  assert.equal(coordinator.isStopping, false);
  assert.equal(coordinator.activeMode, "remux");

  done(24, -1);
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(coordinator.activeMode, null);
});

test("completion during a pending Stop preserves the terminal state if Stop rejects", async t => {
  const { controller, coordinator, stopping, rejectStop, done } = await adoptedWithPendingStop(t);
  done(24, -1);
  rejectStop(new Error("already finished"));
  await stopping;
  assert.equal(controller.lifecycle, "cancelled");
  assert.equal(coordinator.activeMode, null);
});

test("a Stop still pending when a job completes ends as done: only the terminal says cancelled", async t => {
  const { controller, tracker, coordinator, stopping, rejectStop, done } = await adoptedWithPendingStop(t);
  // The published state of a job that finished before the Stop took effect.
  tracker.state.batchSuccess = true;
  assert.equal(coordinator.isStopping, true);

  done(24, 0);

  assert.equal(controller.lifecycle, "done");
  rejectStop(new Error("already finished"));
  await stopping;
  assert.equal(controller.lifecycle, "done");
  assert.equal(coordinator.activeMode, null);
});

test("job-done ends as cancelled on the published status or code -1, as done only on success with code 0, else failed", async t => {
  for (const [state, code, lifecycle] of [
    [{ batchCancelled: true, batchSuccess: false }, 1, "cancelled"],
    [{ batchCancelled: false, batchSuccess: false }, -1, "cancelled"],
    [{ batchCancelled: false, batchSuccess: true }, 0, "done"],
    [{ batchCancelled: false, batchSuccess: true }, 1, "failed"],
    // A request the engine refused: the bridge's failed terminal, then code 1.
    [{ batchCancelled: false, batchSuccess: false }, 1, "failed"],
  ]) {
    const { controller, done } = await adopted(t, 9, { tracker: makeTracker({ state }) });
    done(9, code);
    assert.equal(controller.lifecycle, lifecycle, JSON.stringify({ state, code }));
  }
});
