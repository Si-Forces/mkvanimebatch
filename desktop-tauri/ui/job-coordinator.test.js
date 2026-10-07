import assert from "node:assert/strict";
import test from "node:test";

import { createJobCoordinator } from "./job-coordinator.js";
import { makeRestore } from "./job-coordinator-fakes.js";

/** A coordinator that counts how often it re-renders its controls. */
function countedCoordinator() {
  const coordinator = createJobCoordinator();
  const counter = { syncs: 0 };
  coordinator.register({ mode: "observer", syncGlobalControls: () => counter.syncs++ });
  return { coordinator, counter };
}

/** Moves a fresh coordinator into `phase`, owned by "remux" where a mode owns the batch. */
function inPhase(phase) {
  const coordinator = createJobCoordinator();
  if (phase === "idle") return coordinator;
  if (phase === "restoring") {
    // The restore holds `restoring` until the snapshot answers, which this fake never does.
    coordinator.restore(makeRestore(coordinator, { getSnapshot: () => new Promise(() => {}) }).deps);
    return coordinator;
  }
  coordinator.begin("remux");
  if (phase !== "starting") coordinator.launched();
  if (phase === "stopping") coordinator.requestStop();
  return coordinator;
}

const PHASES = ["idle", "restoring", "starting", "running", "stopping"];

/** The transitions that must refuse from every phase but those listed, with how to call each. */
const GUARDED_TRANSITIONS = [
  { name: "begin", call: c => c.begin("extract"), legalFrom: ["idle"] },
  { name: "launched", call: c => c.launched(), legalFrom: ["starting"] },
  { name: "requestStop", call: c => c.requestStop(), legalFrom: ["starting", "running"] },
  { name: "stopFailed", call: c => c.stopFailed(), legalFrom: ["stopping"] },
];

test("coordinator permits only one active pipeline", () => {
  const coordinator = createJobCoordinator();

  assert.equal(coordinator.begin("remux"), true);
  assert.equal(coordinator.begin("extract"), false);
  assert.equal(coordinator.activeMode, "remux");

  coordinator.finish();
  assert.equal(coordinator.begin("extract"), true);
});

test("a job walks idle, starting, running, stopping and back to idle", () => {
  const coordinator = createJobCoordinator();
  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.canStart(), true);

  assert.equal(coordinator.begin("remux"), true);
  assert.equal(coordinator.phase, "starting");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(coordinator.canStart(), false);

  assert.equal(coordinator.launched(), true);
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "remux");

  assert.equal(coordinator.requestStop(), true);
  assert.equal(coordinator.phase, "stopping");
  assert.equal(coordinator.isStopping, true);
  assert.equal(coordinator.activeMode, "remux", "the mode keeps the batch until the job ends");

  assert.equal(coordinator.finish(), true);
  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.activeMode, null);
  assert.equal(coordinator.isStopping, false);
  assert.equal(coordinator.canStart(), true);
});

test("a stop during starting moves straight to stopping", () => {
  const coordinator = inPhase("starting");

  assert.equal(coordinator.requestStop(), true);
  assert.equal(coordinator.phase, "stopping");
  assert.equal(coordinator.launched(), false, "a stopped start cannot launch");
  assert.equal(coordinator.phase, "stopping");
});

test("a failed stop returns to running and keeps the mode", () => {
  const coordinator = inPhase("running");

  coordinator.requestStop();
  assert.equal(coordinator.stopFailed(), true);
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.isStopping, false);
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(coordinator.requestStop(), true, "the stop can be asked for again");
});

for (const { name, call, legalFrom } of GUARDED_TRANSITIONS) {
  for (const phase of PHASES.filter(p => !legalFrom.includes(p))) {
    test(`${name}() is refused from ${phase} and changes nothing`, () => {
      const coordinator = inPhase(phase);
      const counter = { syncs: 0 };
      coordinator.register({ mode: "observer", syncGlobalControls: () => counter.syncs++ });
      const mode = coordinator.activeMode;

      assert.equal(call(coordinator), false);
      assert.equal(coordinator.phase, phase);
      assert.equal(coordinator.activeMode, mode);
      assert.equal(counter.syncs, 0, "a refused transition must not re-render");
    });
  }
}

for (const phase of PHASES) {
  test(`finish() from ${phase} ends in idle with no mode, and is idempotent`, () => {
    const coordinator = inPhase(phase);

    assert.equal(coordinator.finish(), true);
    assert.equal(coordinator.phase, "idle");
    assert.equal(coordinator.activeMode, null);
    assert.equal(coordinator.canStart(), true);
    assert.equal(coordinator.finish(), true);
    assert.equal(coordinator.phase, "idle");
  });
}

test("the observers cannot be assigned", () => {
  const coordinator = inPhase("running");

  assert.throws(() => { coordinator.activeMode = null; }, TypeError);
  assert.throws(() => { coordinator.isStopping = true; }, TypeError);
  assert.throws(() => { coordinator.phase = "idle"; }, TypeError);
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(coordinator.isStopping, false);
});

test("every successful transition re-renders the controls once, and a refused one not at all", () => {
  const { coordinator, counter } = countedCoordinator();
  const sync = transition => {
    const before = counter.syncs;
    const applied = transition();
    return [applied, counter.syncs - before];
  };

  assert.deepEqual(sync(() => coordinator.begin("remux")), [true, 1]);
  assert.deepEqual(sync(() => coordinator.begin("extract")), [false, 0]);
  assert.deepEqual(sync(() => coordinator.launched()), [true, 1]);
  assert.deepEqual(sync(() => coordinator.stopFailed()), [false, 0]);
  assert.deepEqual(sync(() => coordinator.requestStop()), [true, 1]);
  assert.deepEqual(sync(() => coordinator.requestStop()), [false, 0]);
  assert.deepEqual(sync(() => coordinator.stopFailed()), [true, 1]);
  assert.deepEqual(sync(() => coordinator.finish()), [true, 1]);
});

test("syncControls re-renders without a transition, for changes the coordinator does not own", () => {
  const { coordinator, counter } = countedCoordinator();

  coordinator.syncControls();

  assert.equal(counter.syncs, 1);
  assert.equal(coordinator.phase, "idle");
});

test("a second controller for one mode is refused, so restore() never attaches the wrong one", () => {
  const coordinator = createJobCoordinator();
  coordinator.register({ mode: "remux", syncGlobalControls() {} });
  assert.throws(() => coordinator.register({ mode: "remux", syncGlobalControls() {} }), /already registered for mode "remux"/);
});
