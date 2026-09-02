import test from "node:test";
import assert from "node:assert/strict";

import { createJobCoordinator } from "./job-coordinator.js";
import { JobController } from "./job-controller.js";

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
