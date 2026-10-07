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
 * read it through the observers and move it only through the transitions.
 */
export function createJobCoordinator() {
  const controllers = new Set();
  let phase = "idle";
  // The mode that owns the batch while starting, running or stopping; `null` otherwise.
  let owner = null;

  function syncControls() {
    for (const controller of controllers) controller.syncGlobalControls();
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
   * A finished job only has its presentation restored; a running one is reattached, so the
   * coordinator moves to `running` and the controller of its mode takes over the two listeners.
   *
   * The listeners bind before the snapshot is fetched and buffer what they hear, so a signal that
   * lands in between is replayed (flagged `replay`) once the job is attached rather than lost.
   * `controllersByMode` maps each mode to its `JobController` (the ones registered for re-rendering);
   * `events` names the two bridge events.
   */
  async function restore({ listen, invoke, controllersByMode, queueFor, switchView, events }) {
    if (!move(["idle"], "restoring")) return;
    const buffered = [];
    let active = null;
    let unlistenEvent = null;
    let unlistenDone = null;
    const deliver = (type, payload) => {
      if (active) active.handleSnapshotSignal(type, payload);
      else buffered.push([type, payload]);
    };

    try {
      unlistenEvent = await listen(events.JOB_EVENT, e => deliver("event", e.payload));
      unlistenDone = await listen(events.JOB_DONE, e => deliver("done", e.payload));
      const snapshot = await invoke("get_job_snapshot");
      if (!snapshot || snapshot.job_id == null || !snapshot.items || !controllersByMode[snapshot.mode]) return;
      const queue = queueFor(snapshot.mode);
      if (snapshot.status !== "running" && queue.state.items.size > 0) return;

      queue.restoreFromSnapshot(snapshot);
      if (snapshot.mode === "extract") switchView("extract");
      if (snapshot.status !== "running") {
        controllersByMode[snapshot.mode].restoreTerminalSnapshot(snapshot);
        return;
      }

      active = controllersByMode[snapshot.mode];
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

    register(controller) {
      controllers.add(controller);
    },

    /** Re-renders every registered control, for a change the coordinator does not own, such as a selection. */
    syncControls,
  };
}
