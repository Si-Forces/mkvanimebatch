/** The inputs whose empty state means the engine's default, as [input id, published hint key]. */
const AUTO_INPUTS = ["remux", "extract"].flatMap(mode => [
  [`${mode}-workers`, "workers_hint"],
  [`${mode}-io-jobs`, "io_jobs_hint"],
]);

/**
 * The worker and I/O job chips and the `auto (N)` hints on the workers and Disk I/O inputs, as
 * the View State's `chips` publishes them (`crates/mkv-desktop-core/src/presentation.rs` holds
 * their texts and which Effective Settings they show).
 */
export function createSettingsChips({ doc }) {
  // The hints the inputs show, as JSON.
  let hintsKey = null;

  function setChips(selector, text) {
    doc.querySelectorAll(selector).forEach(chip => {
      if (chip.innerText !== text) chip.innerText = text;
    });
  }

  /**
   * Renders `state.chips`. A hint is null until the core has read the defaults, which leaves the
   * input on its own plain `auto`.
   */
  function render(state) {
    const chips = state?.chips;
    if (!chips) return;
    setChips(".metric-workers-chip", chips.workers);
    setChips(".metric-iojobs-chip", chips.io_jobs);
    const key = JSON.stringify([chips.workers_hint, chips.io_jobs_hint]);
    if (key === hintsKey) return;
    hintsKey = key;
    for (const [id, hint] of AUTO_INPUTS) {
      const input = doc.getElementById(id);
      if (input && chips[hint] != null) input.placeholder = chips[hint];
    }
  }

  return { render };
}
