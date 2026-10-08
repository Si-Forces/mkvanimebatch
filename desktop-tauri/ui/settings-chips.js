/** Inputs whose empty state means the engine's default, as [input id, Effective Settings key]. */
const AUTO_INPUTS = ["remux", "extract"].flatMap(mode => [
  [`${mode}-workers`, "workers"],
  [`${mode}-io-jobs`, "io_jobs"],
]);

/** A chip's text: `count` is shown when known, and otherwise left out rather than guessed. */
function chipText(icon, isJobActive, idleWord, count, noun) {
  const counted = count == null ? null : `${count} ${noun}`;
  if (isJobActive) return `${icon} ${counted ?? noun} Active`;
  return counted ? `${icon} ${idleWord} (${counted})` : `${icon} ${idleWord}`;
}

/**
 * The worker and I/O job chips and the `auto (N)` hints on the workers and Disk I/O inputs, which
 * show Effective Settings: the engine's defaults, or a running job's own. `isJobActive()` says
 * whether a job owns the chips right now.
 */
export function createSettingsChips({ invoke, doc, isJobActive }) {
  /**
   * The Effective Settings of a request that sets nothing: `null` until `get_default_settings`
   * answers, or when it fails. An empty workers field sends `workers: null`, which the engine
   * resolves to these.
   */
  let defaultSettings = null;

  /**
   * Shows `settings` (Effective Settings) on the chips: the running job's while `jobActive`,
   * otherwise the defaults. `null` settings, not yet known, leave the counts out.
   */
  function update(jobActive = false, settings = defaultSettings) {
    doc.querySelectorAll(".metric-workers-chip").forEach(chip => {
      chip.innerText = chipText("⚡", jobActive, "Standby", settings?.workers, "Workers");
    });
    doc.querySelectorAll(".metric-iojobs-chip").forEach(chip => {
      chip.innerText = chipText("🔀", jobActive, "Idle", settings?.io_jobs, "IO Jobs");
    });
  }

  async function loadDefaults() {
    try {
      const settings = await invoke("get_default_settings");
      if (!Number.isInteger(settings?.workers) || settings.workers < 1) {
        throw new Error(`unexpected value ${JSON.stringify(settings)}`);
      }
      defaultSettings = settings;
      for (const [id, key] of AUTO_INPUTS) {
        const input = doc.getElementById(id);
        if (!input) continue;
        input.placeholder = `auto (${settings[key]})`;
        input.dataset.autoValue = String(settings[key]);
      }
      // Leave the chips alone while a job owns them; they show that job's own settings.
      if (!isJobActive()) update(false);
    } catch (err) {
      // The placeholder stays "auto" and the chips name their state without a count.
      console.error("[get_default_settings] failed:", err);
    }
  }

  return { loadDefaults, update };
}
