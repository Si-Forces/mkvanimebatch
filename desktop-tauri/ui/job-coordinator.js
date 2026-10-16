/**
 * The lifecycle of the one job the app runs at a time, across the Remux and Extract views.
 *
 * - `idle`: nothing owns the batch.
 * - `starting`: a mode took the slot and is preflighting; it owns the captured batch.
 * - `running`: the engine job was launched, or adopted after a webview (re)load.
 * - `stopping`: a stop was requested and teardown is pending.
 *
 * Each transition returns `true` when it applied and re-renders the registered controls itself.
 * From any other phase it returns `false` and changes nothing. The state is private: controllers
 * read it through the observers and move it only through the transitions.
 */
export function createJobCoordinator() {
  // The registered controllers, keyed by mode.
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

  return {
    get phase() {
      return phase;
    },
    /** The mode that owns the batch while starting, running or stopping; `null` otherwise. */
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

    /** Takes over a job that was already running when the webview (re)loaded. */
    adopt(mode) {
      return move(["idle"], "running", mode);
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

    /**
     * Registers a controller under its `mode`, re-rendered on every transition. One controller
     * per mode, so a second one for a mode throws.
     */
    register(controller) {
      if (controllers.has(controller.mode)) throw new Error(`A controller is already registered for mode "${controller.mode}"`);
      controllers.set(controller.mode, controller);
    },

    /** Re-renders every registered control, for a change the coordinator does not own, such as an inspection scan. */
    syncControls,
  };
}
