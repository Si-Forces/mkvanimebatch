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
    lastManifest: { version: 1, items: [] },
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
