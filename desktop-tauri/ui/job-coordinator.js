export function createJobCoordinator(onChange = () => {}) {
  const controllers = new Set();

  return {
    activeMode: null,
    isStarting: false,
    isStopping: false,
    // Raised only while the startup snapshot is being fetched, to block a
    // concurrent `begin()`. Distinct from `isStarting`, which means a job is
    // actually starting and therefore owns the captured batch.
    isRestoring: false,

    canStart() {
      return this.activeMode === null && !this.isStarting && !this.isStopping && !this.isRestoring;
    },

    begin(mode) {
      if (!this.canStart()) return false;
      this.activeMode = mode;
      this.isStarting = true;
      this.syncControls();
      return true;
    },

    register(controller) {
      controllers.add(controller);
    },

    syncControls() {
      for (const controller of controllers) controller.syncGlobalControls();
      onChange(this);
    },

    finish() {
      this.activeMode = null;
      this.isStarting = false;
      this.isStopping = false;
      this.isRestoring = false;
      this.syncControls();
    },
  };
}
