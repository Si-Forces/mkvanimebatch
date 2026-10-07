import assert from "node:assert/strict";
import test from "node:test";

import { createJobCoordinator } from "./job-coordinator.js";
import { makeRestore } from "./job-coordinator-fakes.js";

const tick = () => new Promise(resolve => setTimeout(resolve, 0));

/** The snapshot shape `get_job_snapshot` answers with; `extra` overrides its fields. */
const snapshot = (extra = {}) => ({ job_id: 42, mode: "remux", status: "running", items: {}, ...extra });

/** A finished snapshot with one item, which is what a job that got as far as `BatchStart` leaves. */
const finishedWithItems = (status, extra = {}) =>
  snapshot({ status, ordered_item_ids: ["ep"], items: { ep: { id: "ep", file: "ep.mkv", status: "completed" } }, ...extra });

test("restore with no snapshot returns to idle and releases both listeners", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, unlistened, restoredQueues } = makeRestore(coordinator);

  await coordinator.restore(deps);

  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.canStart(), true);
  assert.deepEqual(unlistened, ["job-event", "job-done"]);
  assert.deepEqual(calls, []);
  assert.deepEqual(restoredQueues, []);
});

for (const [label, answer] of [
  ["without a job id", snapshot({ job_id: null })],
  ["without items", snapshot({ items: null })],
  ["for an unknown mode", snapshot({ mode: "inspect" })],
]) {
  test(`restore ignores a snapshot ${label}`, async () => {
    const coordinator = createJobCoordinator();
    const { deps, calls, unlistened, restoredQueues } = makeRestore(coordinator, { getSnapshot: async () => answer });

    await coordinator.restore(deps);

    assert.equal(coordinator.phase, "idle");
    assert.deepEqual(unlistened, ["job-event", "job-done"]);
    assert.deepEqual(calls, []);
    assert.deepEqual(restoredQueues, []);
  });
}

test("restore leaves a queue that already has items alone for a finished snapshot, but still sets the pill", async () => {
  for (const [label, answer] of [
    ["without items", snapshot({ status: "completed" })],
    ["with items", finishedWithItems("completed")],
  ]) {
    const coordinator = createJobCoordinator();
    const { deps, calls, unlistened, restoredQueues, views } = makeRestore(coordinator, {
      getSnapshot: async () => answer,
      queue: { state: { items: new Map([["ep", {}]]) } },
    });

    await coordinator.restore(deps);

    assert.equal(coordinator.phase, "idle", label);
    assert.deepEqual(unlistened, ["job-event", "job-done"], label);
    assert.deepEqual(calls, [["terminal", "remux", "completed"]], label);
    assert.deepEqual(restoredQueues, [], label);
    assert.deepEqual(views, [], label);
  }
});

test("restore of a finished extract snapshot over a populated queue switches the view and leaves the queue alone", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, restoredQueues, views } = makeRestore(coordinator, {
    getSnapshot: async () => finishedWithItems("error", { mode: "extract" }),
    queue: { state: { items: new Map([["ep", {}]]) } },
  });

  await coordinator.restore(deps);

  assert.deepEqual(views, ["extract"]);
  assert.deepEqual(calls, [["terminal", "extract", "error"]]);
  assert.deepEqual(restoredQueues, []);
  assert.equal(coordinator.phase, "idle");
});

test("restore of a finished snapshot restores presentation only and returns to idle", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, unlistened, restoredQueues, views } = makeRestore(coordinator, {
    getSnapshot: async () => finishedWithItems("completed"),
  });

  await coordinator.restore(deps);

  assert.deepEqual(restoredQueues, [["remux", 42]]);
  assert.deepEqual(calls, [["terminal", "remux", "completed"]]);
  assert.deepEqual(views, []);
  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.activeMode, null);
  assert.deepEqual(unlistened, ["job-event", "job-done"]);
});

test("restore of a finished snapshot with no items leaves the queue alone but still sets the terminal pill", async () => {
  // A job that failed before `BatchStart` has no items, and restoring none would empty the queue.
  for (const status of ["error", "cancelled"]) {
    const coordinator = createJobCoordinator();
    const { deps, calls, unlistened, restoredQueues } = makeRestore(coordinator, {
      getSnapshot: async () => snapshot({ status, ordered_item_ids: [] }),
    });

    await coordinator.restore(deps);

    assert.deepEqual(restoredQueues, [], status);
    assert.deepEqual(calls, [["terminal", "remux", status]], status);
    assert.equal(coordinator.phase, "idle");
    assert.deepEqual(unlistened, ["job-event", "job-done"]);
  }
});

test("restore of a running snapshot with no items yet still rebuilds the queue and attaches", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, restoredQueues } = makeRestore(coordinator, {
    getSnapshot: async () => snapshot({ ordered_item_ids: [] }),
  });

  await coordinator.restore(deps);

  assert.deepEqual(restoredQueues, [["remux", 42]]);
  assert.equal(calls[0][0], "active");
  assert.equal(coordinator.phase, "running");
});

test("restore of a finished extract snapshot switches to the extract view", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, views } = makeRestore(coordinator, {
    getSnapshot: async () => snapshot({ mode: "extract", status: "cancelled" }),
  });

  await coordinator.restore(deps);

  assert.deepEqual(views, ["extract"]);
  assert.deepEqual(calls, [["terminal", "extract", "cancelled"]]);
  assert.equal(coordinator.phase, "idle");
});

test("restore of a running snapshot attaches the coordinator and hands over the listeners", async () => {
  const coordinator = createJobCoordinator();
  const { deps, handlers, calls, unlistened, restoredQueues, attachedAs } = makeRestore(coordinator, {
    getSnapshot: async () => snapshot(),
  });

  await coordinator.restore(deps);

  assert.deepEqual(restoredQueues, [["remux", 42]]);
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "remux");
  assert.equal(coordinator.canStart(), false);
  assert.deepEqual(attachedAs, [{ phase: "running", activeMode: "remux" }], "the controller finds the coordinator already attached");
  assert.deepEqual(unlistened, [], "the controller owns the listeners now");
  const [kind, mode, jobId, unlistenEvent, unlistenDone] = calls[0];
  assert.deepEqual([kind, mode, jobId], ["active", "remux", 42]);
  unlistenEvent();
  unlistenDone();
  assert.deepEqual(unlistened, ["job-event", "job-done"], "the controller received the bound unlisteners");

  // A signal that lands after the attach reaches the controller directly, not as a replay.
  handlers.get("job-event")({ payload: { job_id: 42, type: "item_start" } });
  assert.deepEqual(calls.at(-1), ["signal", "remux", "event", { job_id: 42, type: "item_start" }, undefined]);
});

test("restore of a running extract snapshot switches to the extract view and attaches the extract controller", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, views, unlistened, attachedAs } = makeRestore(coordinator, {
    getSnapshot: async () => snapshot({ mode: "extract" }),
  });

  await coordinator.restore(deps);

  assert.deepEqual(views, ["extract"]);
  assert.equal(calls[0][0], "active");
  assert.equal(calls[0][1], "extract");
  assert.deepEqual(attachedAs, [{ phase: "running", activeMode: "extract" }]);
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "extract");
  assert.deepEqual(unlistened, []);
});

test("restore still attaches a running snapshot when the queue already has items", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls, restoredQueues } = makeRestore(coordinator, {
    getSnapshot: async () => snapshot(),
    queue: { state: { items: new Map([["ep", {}]]) } },
  });

  await coordinator.restore(deps);

  assert.deepEqual(restoredQueues, [["remux", 42]], "a running job's queue is rebuilt over the items already shown");
  assert.equal(calls[0][0], "active");
  assert.equal(coordinator.phase, "running");
  assert.equal(coordinator.activeMode, "remux");
});

test("restore replays signals heard before the snapshot, in order, flagged as replays", async () => {
  const coordinator = createJobCoordinator();
  let answer;
  const { deps, handlers, calls } = makeRestore(coordinator, {
    getSnapshot: () => new Promise(resolve => { answer = resolve; }),
  });

  const restoring = coordinator.restore(deps);
  await tick();
  handlers.get("job-event")({ payload: { n: 1 } });
  handlers.get("job-done")({ payload: { n: 2 } });
  handlers.get("job-event")({ payload: { n: 3 } });
  assert.deepEqual(calls, [], "nothing reaches a controller before the snapshot says which one owns the job");
  answer(snapshot());
  await restoring;

  assert.equal(coordinator.phase, "running");
  assert.deepEqual(calls.slice(1), [
    ["signal", "remux", "event", { n: 1 }, true],
    ["signal", "remux", "done", { n: 2 }, true],
    ["signal", "remux", "event", { n: 3 }, true],
  ]);
});

test("a job that finishes during the replay leaves the coordinator idle, not restoring", async () => {
  const coordinator = createJobCoordinator();
  let answer;
  const { deps, controllers, handlers } = makeRestore(coordinator, {
    getSnapshot: () => new Promise(resolve => { answer = resolve; }),
  });
  // The restored controller ends the job on the buffered done, as `onDone` does.
  controllers.remux.handleSnapshotSignal = type => {
    if (type === "done") coordinator.finish();
  };

  const restoring = coordinator.restore(deps);
  await tick();
  handlers.get("job-done")({ payload: { job_id: 42, code: 0 } });
  answer(snapshot());
  await restoring;

  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.canStart(), true);
});

test("restore logs a rejected snapshot fetch, returns to idle and releases the listeners", async t => {
  const coordinator = createJobCoordinator();
  const failure = new Error("bridge unavailable");
  const errors = t.mock.method(console, "error", () => {});
  const { deps, calls, unlistened } = makeRestore(coordinator, { getSnapshot: async () => { throw failure; } });

  await coordinator.restore(deps);

  assert.equal(errors.mock.callCount(), 1);
  assert.deepEqual(errors.mock.calls[0].arguments, ["[get_job_snapshot] restore failed:", failure]);
  assert.equal(coordinator.phase, "idle");
  assert.deepEqual(unlistened, ["job-event", "job-done"]);
  assert.deepEqual(calls, []);
});

test("restore releases the first listener when the second cannot bind", async t => {
  const coordinator = createJobCoordinator();
  t.mock.method(console, "error", () => {});
  const { deps, unlistened } = makeRestore(coordinator);
  deps.listen = async event => {
    if (event === "job-done") throw new Error("no bridge");
    return () => unlistened.push(event);
  };

  await coordinator.restore(deps);

  assert.equal(coordinator.phase, "idle");
  assert.deepEqual(unlistened, ["job-event"]);
});

test("a start is refused while the restore is in flight and allowed once it ends", async () => {
  const coordinator = createJobCoordinator();
  let answer;
  const { deps } = makeRestore(coordinator, { getSnapshot: () => new Promise(resolve => { answer = resolve; }) });

  const restoring = coordinator.restore(deps);
  await tick();

  assert.equal(coordinator.phase, "restoring");
  assert.equal(coordinator.begin("remux"), false, "a restore in flight must block a concurrent start");
  assert.equal(coordinator.activeMode, null, "a restore does not own the batch");
  assert.equal(coordinator.phase, "restoring");

  answer(null);
  await restoring;

  assert.equal(coordinator.phase, "idle");
  assert.equal(coordinator.begin("remux"), true);
});

test("restore re-renders the controls on entering and leaving the restoring phase", async () => {
  const coordinator = createJobCoordinator();
  const phases = [];
  coordinator.register({ mode: "observer", syncGlobalControls: () => phases.push(coordinator.phase) });
  const { deps } = makeRestore(coordinator);

  await coordinator.restore(deps);

  assert.deepEqual(phases, ["restoring", "idle"]);
});

test("restore of a running snapshot re-renders the controls on the attach, as each controller sees it", async () => {
  const coordinator = createJobCoordinator();
  const seen = [];
  coordinator.register({ mode: "observer", syncGlobalControls: () => seen.push([coordinator.phase, coordinator.activeMode]) });
  const { deps } = makeRestore(coordinator, { getSnapshot: async () => snapshot() });

  await coordinator.restore(deps);

  assert.deepEqual(seen, [["restoring", null], ["running", "remux"]]);
});

test("restore is refused while a job already owns the coordinator", async () => {
  const coordinator = createJobCoordinator();
  coordinator.begin("extract");
  const { deps, handlers } = makeRestore(coordinator, { getSnapshot: async () => snapshot() });

  assert.equal(await coordinator.restore(deps), false, "a refused restore resolves false");

  assert.equal(coordinator.phase, "starting");
  assert.equal(coordinator.activeMode, "extract");
  assert.equal(handlers.size, 0, "a refused restore binds no listeners");
});

test("restore resolves true once it ran, whatever the snapshot held", async () => {
  for (const answer of [null, snapshot(), snapshot({ status: "completed" })]) {
    const coordinator = createJobCoordinator();
    const { deps } = makeRestore(coordinator, { getSnapshot: async () => answer });
    assert.equal(await coordinator.restore(deps), true);
  }
  const failing = createJobCoordinator();
  const { deps } = makeRestore(failing, { getSnapshot: async () => { throw new Error("bridge down"); } });
  const originalError = console.error;
  console.error = () => {};
  try {
    assert.equal(await failing.restore(deps), true);
  } finally {
    console.error = originalError;
  }
});

test("restore attaches the controller registered for the snapshot's mode, not another one", async () => {
  const coordinator = createJobCoordinator();
  const { deps, calls } = makeRestore(coordinator, { getSnapshot: async () => snapshot({ mode: "extract" }) });

  await coordinator.restore(deps);

  assert.deepEqual(calls.filter(([kind]) => kind === "active").map(([, mode]) => mode), ["extract"]);
});
