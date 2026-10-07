// Test support, not part of the app: the fakes the suites share to drive `coordinator.restore()`.
import assert from "node:assert/strict";

export const EVENTS = { JOB_EVENT: "job-event", JOB_DONE: "job-done" };

/**
 * Everything `coordinator.restore()` is injected with, as fakes. `listen` records each handler by
 * event name and logs each unlisten; `getSnapshot` answers `get_job_snapshot` (a function, so a
 * test can hold the answer back or reject it); `queue` overrides fields of the fake queue.
 *
 * The fake controllers log what they were told in `calls`. `attachedAs` records the `coordinator`'s
 * phase and mode at the moment each `restoreActiveSnapshot` call arrives, which pins that a real
 * controller finds the coordinator already attached to its job.
 */
export function makeRestore(coordinator, { getSnapshot = async () => null, queue = {} } = {}) {
  const handlers = new Map();
  const calls = [];
  const unlistened = [];
  const restoredQueues = [];
  const attachedAs = [];
  const views = [];
  const controller = mode => ({
    restoreTerminalSnapshot: snap => calls.push(["terminal", mode, snap.status]),
    restoreActiveSnapshot: (snap, unlistenEvent, unlistenDone) => {
      attachedAs.push({ phase: coordinator.phase, activeMode: coordinator.activeMode });
      calls.push(["active", mode, snap.job_id, unlistenEvent, unlistenDone]);
    },
    handleSnapshotSignal: (type, payload, replay) => calls.push(["signal", mode, type, payload, replay]),
  });
  const deps = {
    listen: async (event, handler) => {
      handlers.set(event, handler);
      return () => unlistened.push(event);
    },
    invoke: async command => {
      assert.equal(command, "get_job_snapshot");
      return getSnapshot();
    },
    controllersByMode: { remux: controller("remux"), extract: controller("extract") },
    queueFor: mode => ({
      state: { items: new Map() },
      restoreFromSnapshot: snap => restoredQueues.push([mode, snap.job_id]),
      ...queue,
    }),
    switchView: view => views.push(view),
    events: EVENTS,
  };
  return { deps, handlers, calls, unlistened, restoredQueues, attachedAs, views };
}
