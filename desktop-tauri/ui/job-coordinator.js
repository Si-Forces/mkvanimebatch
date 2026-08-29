export function createJobCoordinator(onChange = () => {}) {
  const controllers = new Set();

  return {
    activeMode: null,
    isStarting: false,
    isStopping: false,

    canStart() {
      return this.activeMode === null && !this.isStarting && !this.isStopping;
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
      this.syncControls();
    },
  };
}
