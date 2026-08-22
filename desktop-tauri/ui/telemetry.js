export const IDLE_POLL_MS = 30000;
export const ACTIVE_POLL_MS = 1000;

export function createTelemetryLoop({
  invoke,
  onData,
  onError,
  doc = null,
  setIntervalFn = setInterval.bind(globalThis),
  clearIntervalFn = clearInterval.bind(globalThis),
} = {}) {
  let timer = null;
  let started = false;
  let jobActive = false;
  let inFlight = false;

  const isHidden = () => (doc ? doc.hidden === true : false);

  const currentInterval = () => (jobActive ? ACTIVE_POLL_MS : IDLE_POLL_MS);

  async function tick() {
    if (inFlight || isHidden()) return;
    inFlight = true;
    try {
      const data = await invoke("get_system_telemetry");
      if (started && data && onData) onData(data);
    } catch (err) {
      if (started && onError) onError(err);
    } finally {
      inFlight = false;
    }
  }

  function restartTimer() {
    if (timer !== null) {
      clearIntervalFn(timer);
      timer = null;
    }
    if (!started || isHidden()) return;
    void tick();
    timer = setIntervalFn(tick, currentInterval());
  }

  function onVisibilityChange() {
    restartTimer();
  }

  return {
    start() {
      if (started) return;
      started = true;
      if (doc && doc.addEventListener) {
        doc.addEventListener("visibilitychange", onVisibilityChange);
      }
      restartTimer();
    },
    stop() {
      if (!started) return;
      started = false;
      if (doc && doc.removeEventListener) {
        doc.removeEventListener("visibilitychange", onVisibilityChange);
      }
      restartTimer();
    },
    setJobActive(active) {
      const next = Boolean(active);
      if (jobActive === next) return;
      jobActive = next;
      if (started && !isHidden()) {
        restartTimer();
      }
    },
  };
}
