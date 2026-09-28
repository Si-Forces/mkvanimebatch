export const IDLE_POLL_MS = 30000;
export const ACTIVE_POLL_MS = 1000;

/**
 * Polls `get_system_telemetry` at a cadence that follows job activity, never
 * overlaps an in-flight request, and pauses while `doc` is hidden.
 */
export function createTelemetryLoop({
  invoke,
  onData,
  onError,
  doc = null,
  setIntervalFn = setInterval.bind(globalThis),
  clearIntervalFn = clearInterval.bind(globalThis),
}) {
  let timer = null;
  let started = false;
  let jobActive = false;
  let inFlight = false;

  const isHidden = () => doc?.hidden === true;

  async function tick() {
    if (inFlight || isHidden()) return;
    inFlight = true;
    try {
      const data = await invoke("get_system_telemetry");
      if (data) onData(data);
    } catch (err) {
      onError(err);
    } finally {
      inFlight = false;
    }
  }

  function restartTimer() {
    if (timer !== null) {
      clearIntervalFn(timer);
      timer = null;
    }
    if (isHidden()) return;
    void tick();
    timer = setIntervalFn(tick, jobActive ? ACTIVE_POLL_MS : IDLE_POLL_MS);
  }

  return {
    start() {
      if (started) return;
      started = true;
      doc?.addEventListener("visibilitychange", restartTimer);
      restartTimer();
    },
    setJobActive(active) {
      const next = Boolean(active);
      if (jobActive === next) return;
      jobActive = next;
      if (started && !isHidden()) restartTimer();
    },
  };
}
