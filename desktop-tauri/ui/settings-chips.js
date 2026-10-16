import { isBusy } from "./presentation.js";

/** Inputs whose empty state means the engine's default, as [input id, Effective Settings key]. */
const AUTO_INPUTS = ["remux", "extract"].flatMap(mode => [
  [`${mode}-workers`, "workers"],
  [`${mode}-io-jobs`, "io_jobs"],
]);

/** A chip's text: `count` is shown when known, and otherwise left out rather than guessed. */
function chipText(isJobActive, idleWord, count, noun) {
  const counted = count == null ? null : `${count} ${noun}`;
  if (isJobActive) return `${counted ?? noun} Active`;
  return counted ? `${idleWord} (${counted})` : idleWord;
}

/**
 * The worker and I/O job chips and the `auto (N)` hints on the workers and Disk I/O inputs, which
 * show Effective Settings, all read from the View State: the published `defaults` while nothing
 * holds the batch, else the start's or the job's own (`run.settings`, else the Job View's).
 */
export function createSettingsChips({ doc }) {
  // The `defaults` the inputs show, as JSON.
  let defaultsKey = null;

  function setChips(selector, text) {
    doc.querySelectorAll(selector).forEach(chip => {
      if (chip.innerText !== text) chip.innerText = text;
    });
  }

  /**
   * Renders `state`. Its `defaults` are `null` until the core has read them, which leaves the
   * inputs on plain `auto`; an empty workers field sends no value, which the engine resolves to
   * these. A busy batch with no settings known yet (a start still resolving) shows no counts.
   */
  function render(state) {
    const defaults = state?.defaults ?? null;
    const key = JSON.stringify(defaults);
    if (defaults && key !== defaultsKey) {
      defaultsKey = key;
      for (const [id, field] of AUTO_INPUTS) {
        const input = doc.getElementById(id);
        if (!input) continue;
        input.placeholder = `auto (${defaults[field]})`;
        input.dataset.autoValue = String(defaults[field]);
      }
    }
    const busy = isBusy(state);
    const settings = busy ? state.run?.settings ?? (state.job?.active ? state.job.effective_settings : null) ?? null : defaults;
    setChips(".metric-workers-chip", chipText(busy, "Standby", settings?.workers, "Workers"));
    setChips(".metric-iojobs-chip", chipText(busy, "Idle", settings?.io_jobs, "IO Jobs"));
  }

  return { render };
}
