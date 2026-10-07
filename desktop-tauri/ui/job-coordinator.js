import { JOB_DONE, JOB_EVENT } from "./job-controller.js";

/**
 * The lifecycle of the one job the app runs at a time, across the Remux and Extract views.
 *
 * - `idle`: nothing owns the batch.
 * - `restoring`: the startup snapshot is being fetched. It blocks `begin()` but owns no batch, so
 *   `activeMode` stays `null` and the user can still adjust a restored queue.
 * - `starting`: a mode took the slot and is preflighting; it owns the captured batch.
 * - `running`: the engine job was launched, or reattached by `restore()`.
 * - `stopping`: a stop was requested and teardown is pending.
 *
 * Each transition returns `true` when it applied and re-renders the registered controls itself.
 * From any other phase it returns `false` and changes nothing. The state is private: controllers
 * read it through the observers and move it only through the transitions. `restore()` likewise
 * resolves `false` when refused, and `true` once it ran, whatever the snapshot held.
 */
export function createJobCoordinator() {
  // The registered controllers, keyed by mode, so `restore()` can find the one a snapshot names.
  const controllers = new Map();
  let phase = "idle";
  // The mode that owns the batch while starting, running or stopping; `null` otherwise.
  let owner = null;

  function syncControls() {
    for (const controller of controllers.values()) controller.syncGlobalControls();
  }

  /** Moves from one of the `from` phases to `to`, keeping the owner unless `mode` replaces it. */
  function move(from, to, mode = owner) {
    if (!from.includes(phase)) return false;
    phase = to;
    owner = mode;
    syncControls();
    return true;
  }

  /**
   * The startup snapshot restore: rebuilds the queue of a job that outlived a webview reload.
   * A finished job only has its presentation restored, and leaves the queue alone if it ended
   * before `BatchStart` and so has no items; a running one is reattached, so the coordinator
   * moves to `running` and the controller of its mode takes over the two listeners.
   *
   * The listeners bind before the snapshot is fetched and buffer what they hear, so a signal that
   * lands in between is replayed (flagged `replay`) once the job is attached rather than lost.
   * The controller is the one registered for the snapshot's mode. Resolves `false` when refused
   * because the coordinator is not idle, else `true` once the restore has settled.
   */
  async function restore({ listen, invoke, queueFor, switchView }) {
    if (!move(["idle"], "restoring")) return false;
    const buffered = [];
    let active = null;
    let unlistenEvent = null;
    let unlistenDone = null;
    const deliver = (type, payload) => {
      if (active) active.handleSnapshotSignal(type, payload);
      else buffered.push([type, payload]);
    };

    try {
      unlistenEvent = await listen(JOB_EVENT, e => deliver("event", e.payload));
      unlistenDone = await listen(JOB_DONE, e => deliver("done", e.payload));
      const snapshot = await invoke("get_job_snapshot");
      const controller = controllers.get(snapshot?.mode);
      if (!snapshot || snapshot.job_id == null || !snapshot.items || !controller) return true;
      const queue = queueFor(snapshot.mode);
      const finished = snapshot.status !== "running";
      if (finished && queue.state.items.size > 0) return true;

      // A job that failed before `BatchStart` has no items, and restoring none would empty the queue.
      if (!finished || Object.keys(snapshot.items).length > 0) queue.restoreFromSnapshot(snapshot);
      if (snapshot.mode === "extract") switchView("extract");
      if (finished) {
        controller.restoreTerminalSnapshot(snapshot);
        return true;
      }

      active = controller;
      move(["restoring"], "running", snapshot.mode);
      active.restoreActiveSnapshot(snapshot, unlistenEvent, unlistenDone);
      unlistenEvent = null;
      unlistenDone = null;
      for (const [type, payload] of buffered) {
        active.handleSnapshotSignal(type, payload, true);
      }
    } catch (err) {
      console.error("[get_job_snapshot] restore failed:", err);
    } finally {
      unlistenEvent?.();
      unlistenDone?.();
      // An attached job has already left `restoring`; this releases only the paths that did not attach.
      move(["restoring"], "idle");
    }
    return true;
  }

  return {
    get phase() {
      return phase;
    },
    /** The mode that owns the batch while starting, running or stopping; `null` otherwise, restoring included. */
    get activeMode() {
      return owner;
    },
    get isStopping() {
      return phase === "stopping";
    },

    canStart() {
      return phase === "idle";
    },

    begin(mode) {
      return move(["idle"], "starting", mode);
    },

    launched() {
      return move(["starting"], "running");
    },

    requestStop() {
      return move(["starting", "running"], "stopping");
    },

    stopFailed() {
      return move(["stopping"], "running");
    },

    /** Releases the slot from any phase, so it always applies and a repeat is harmless. */
    finish() {
      phase = "idle";
      owner = null;
      syncControls();
      return true;
    },

    restore,

    /**
     * Registers a controller under its `mode`: re-rendered on every transition, and attached by
     * `restore()` for that mode. One controller per mode, so a second one for a mode throws.
     */
    register(controller) {
      if (controllers.has(controller.mode)) throw new Error(`A controller is already registered for mode "${controller.mode}"`);
      controllers.set(controller.mode, controller);
    },

    /** Re-renders every registered control, for a change the coordinator does not own, such as an inspection scan. */
    syncControls,
  };
}
