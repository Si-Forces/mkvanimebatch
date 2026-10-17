import { DRAFT_FIELDS } from "./settings-draft.js";

// The modes with a Run sent and not answered yet. Module-wide because both controllers sync the
// shared `.lock-while-job` controls on every publication, and the last sync wins.
const pendingRuns = new Set();

/**
 * The four queue bulk-selection buttons, each with the selection it applies.
 *
 * Every controller touches all of them because a busy batch in *any* mode freezes selection in
 * *both* queues, and `app.js` wires the click handlers from this same list — so a button cannot
 * end up disabled with no handler behind it, nor enabled with a handler that silently refuses.
 */
export const BULK_SELECTION_CONTROLS = [
  { id: "remux-select-all", mode: "remux", select: true },
  { id: "remux-deselect-all", mode: "remux", select: false },
  { id: "extract-select-all", mode: "extract", select: true },
  { id: "extract-deselect-all", mode: "extract", select: false },
];

const QUEUE_MODES = [...new Set(BULK_SELECTION_CONTROLS.map(control => control.mode))];

/**
 * The Run and Stop buttons, the status pill and the controls a busy batch locks, for one mode.
 * The Job Coordinator is the Desktop Core's (`crates/mkv-desktop-core/src/coordinator.rs`): Run
 * and Stop are Intents, and every control here renders the published `busy`, the mode's
 * `controls` (their texts and rules are in `presentation.rs`) and its Settings Draft's `invalid`
 * field. Only a Run sent and not answered yet is the page's own: it locks Run and the settings
 * from the click on. The core writes the Run Log lines a Run, a Stop or a cache clear adds
 * (`actor/start.rs`, `actor.rs`); nothing here writes to the log.
 */
export class JobController {
  constructor(mode, runtime) {
    this.mode = mode;
    this.runtime = runtime;

    this.statusEl = document.getElementById(`${mode}-status`);
    this.runBtn = document.getElementById(`${mode}-run`);
    this.stopBtn = document.getElementById(`${mode}-stop`);
    this.formControls = document.querySelectorAll(
      `#${mode}-settings input, #${mode}-settings select, #${mode}-settings button`
    );

    this.runtime.safeAddListener(this.runBtn, "click", () => this.start());
    this.runtime.safeAddListener(this.stopBtn, "click", () => this.stop());
    this.runtime.safeAddListener(document.getElementById(`${mode}-clear-cache`), "click", () => this.clearCache());

    const inputDirEl = document.getElementById(`${mode}-input-dir`);
    if (inputDirEl) {
      const rescan = () => this.runtime.scanInputDirectory(inputDirEl.value.trim(), mode);
      inputDirEl.addEventListener("change", rescan);
      inputDirEl.addEventListener("blur", rescan);
    }
  }

  /** A Run of this mode sent and not answered yet: Run and the settings stay locked from the click on. */
  get runPending() {
    return pendingRuns.has(this.mode);
  }

  /**
   * Sends Run, which the core answers once the start settles. Run and the mode's settings lock at
   * the click, before the core publishes the start, so a second click sends no second Run and an
   * edit or a preset picked meanwhile is not refused and reverted under the caret; after the
   * answer the published state rules again. The core logs a refusal or a failed start; a draft that
   * built no request also opens the settings sheet on its flagged field.
   */
  async start() {
    if (this.runPending) return;
    pendingRuns.add(this.mode);
    let failed = false;
    try {
      this.syncGlobalControls();
      await this.runtime.send({ type: "run", mode: this.mode });
    } catch {
      failed = true;
    } finally {
      // Unlocked before the error handling, so the flagged field below can take focus.
      pendingRuns.delete(this.mode);
      this.syncGlobalControls();
    }
    if (!failed) return;
    // The refusal is answered after the state that flagged the field was applied.
    const invalid = this.runtime.state()?.[this.mode]?.draft?.invalid;
    const el = invalid && document.getElementById(`${this.mode}-${DRAFT_FIELDS[invalid]?.at(-1)}`);
    if (el) {
      // Every flagged field lives in the closed settings sheet; open it so the error shows.
      this.runtime.openSettings?.(this.mode);
      el.focus();
    }
  }

  /** Sends Stop, which the core answers once the teardown is over; it logs the wait and a failed teardown. */
  async stop() {
    try {
      await this.runtime.send({ type: "stop", mode: this.mode });
    } catch {
      // Already in the Run Log.
    }
  }

  async clearCache() {
    const confirmed = await this.runtime.showModal(
      "Clear inspection cache",
      "Are you sure you want to clear the persistent inspection cache?",
      { confirmLabel: "Clear cache" }
    );
    if (!confirmed) return;
    // The core writes the cache line, or its error, into this mode's Run Log.
    await this.runtime.send({ type: "clear_cache", mode: this.mode }).catch(() => {});
  }

  /**
   * True while the user may change `mode`'s selection: the core refuses a change while a start or
   * a job holds the batch, and while the mode's own inspection runs (its published
   * `selection_locked`). A rescan of one tab leaves the other tab's selection alone.
   */
  canMutateQueueSelection(mode) {
    return this.runtime.state()?.[mode]?.controls?.selection_locked !== true;
  }

  /** Select All / Deselect All. Returns false, changing nothing, while the selection is locked. */
  applyBulkSelection(mode, select) {
    if (!this.canMutateQueueSelection(mode)) return false;
    const tracker = this.runtime.queueFor(mode);
    if (select) tracker.selectAll();
    else tracker.deselectAll();
    return true;
  }

  syncGlobalControls() {
    const state = this.runtime.state();
    const busy = state?.busy === true;
    // None before the first View State: the markup's own labels stand.
    const controls = state?.[this.mode]?.controls;

    if (this.runBtn) {
      this.runBtn.disabled = Boolean(controls?.run_disabled) || this.runPending;
      if (controls) {
        if (this.runBtn.textContent !== controls.run_label) this.runBtn.textContent = controls.run_label;
        this.runBtn.title = controls.run_title;
        // The bottom bar shows this mode's Stop while it owns the batch, and its Run otherwise.
        this.runBtn.hidden = controls.owns;
      }
    }
    if (this.stopBtn && controls) {
      this.stopBtn.disabled = controls.stop_disabled;
      this.stopBtn.textContent = controls.stop_label;
      this.stopBtn.hidden = !controls.owns;
    }
    for (const control of this.formControls) control.disabled = busy || this.runPending;
    // Selection stays frozen through the start and the run: the core captured it at Run and
    // refuses a change, so a click could only desynchronize the UI from the batch. The tracker
    // checks the same flag on every mutation, so a card re-rendered after this sync is guarded too.
    for (const { id, mode } of BULK_SELECTION_CONTROLS) {
      const control = document.getElementById(id);
      if (control) control.disabled = !this.canMutateQueueSelection(mode);
    }
    for (const mode of QUEUE_MODES) {
      this.runtime.queueFor(mode).selectionLocked = !this.canMutateQueueSelection(mode);
    }
    // Controls outside the settings sheet that a busy batch freezes (the presets, the inspector's
    // Track Assignment controls, Open folder), and the root class the locked notes and labels key off.
    // A pending Run of either mode locks them too, as its start will.
    const locked = busy || pendingRuns.size > 0;
    document.querySelectorAll(`.lock-while-job, #${this.mode}-episode-list .empty-state-action`).forEach(control => {
      control.disabled = locked;
    });
    document.documentElement?.classList?.toggle("job-locked", busy);
    const selectionFrozen = !this.canMutateQueueSelection(this.mode);
    document.querySelectorAll(`#${this.mode}-episode-list .ep-select-check, #${this.mode}-episode-list .ep-reorder-btn`).forEach(control => {
      control.disabled = selectionFrozen;
    });
    // The field a Run could not build a request from, until it is edited or Run is pressed again.
    const invalid = state?.[this.mode]?.draft?.invalid ?? null;
    for (const [key, suffixes] of Object.entries(DRAFT_FIELDS)) {
      for (const suffix of suffixes) {
        document.getElementById(`${this.mode}-${suffix}`)?.classList?.toggle?.("input-error", key === invalid);
      }
    }
    if (controls) this.renderStatus(controls.status);
  }

  /**
   * Draws the published status pill, `{class, label}`. The `.status-dot` stays from the markup;
   * the label goes into the text node after it, as text, never as markup.
   */
  renderStatus(status) {
    if (!this.statusEl) return;
    this.statusEl.className = `status-pill ${status.class}`;
    this.statusEl.lastChild.textContent = ` ${status.label}`;
  }
}
