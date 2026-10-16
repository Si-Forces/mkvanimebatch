import { isBusy } from "./presentation.js";
import { DRAFT_FIELDS } from "./settings-draft.js";

/** Webview event names emitted by the engine bridge; must match `src-tauri/src/publisher.rs` (`cleanup-error`: `commands.rs`). */
export const JOB_LOG = "job-log";
export const CLEANUP_ERROR = "cleanup-error";

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

const STATUS_PILLS = {
  idle: ["status-idle", "Idle"],
  running: ["status-running", "Running"],
  stopping: ["status-running", "Stopping"],
  cancelled: ["status-cancelled", "Cancelled"],
  done: ["status-success", "Done"],
  failed: ["status-error", "Failed"],
};

/** The Job Coordinator of a View State that has none yet (before the boot's `get_state`). */
const IDLE_RUN = { phase: "idle", mode: null, settings: null };

/**
 * The Run and Stop buttons, the status pill and the controls a busy batch locks, for one mode.
 * The Job Coordinator is the Desktop Core's (`crates/mkv-desktop-core/src/coordinator.rs`): Run
 * and Stop are Intents, and every control here renders the published `run`, the mode's `outcome`
 * and scan, and its Settings Draft's `invalid` field. The log is still the page's (S4).
 */
export class JobController {
  constructor(mode, runtime) {
    this.mode = mode;
    this.runtime = runtime;
    // The run settings last rendered for this mode: the log is cleared when they appear.
    this.shownSettings = null;

    this.logEl = document.getElementById(`${mode}-log`);
    this.statusEl = document.getElementById(`${mode}-status`);
    this.runBtn = document.getElementById(`${mode}-run`);
    this.stopBtn = document.getElementById(`${mode}-stop`);
    this.formControls = document.querySelectorAll(
      `#${mode}-settings input, #${mode}-settings select, #${mode}-settings button`
    );
    this.runtime.clearLog(this.logEl);

    this.runtime.safeAddListener(this.runBtn, "click", () => this.start());
    this.runtime.safeAddListener(this.stopBtn, "click", () => this.stop());
    this.runtime.safeAddListener(document.getElementById(`${mode}-clear-cache`), "click", () => this.clearCache());
    // The Run button reads "Plan" for a dry run.
    this.runtime.safeAddListener(document.getElementById(`${mode}-dry-run`), "change", () => this.syncGlobalControls());

    const inputDirEl = document.getElementById(`${mode}-input-dir`);
    if (inputDirEl) {
      const rescan = () => this.runtime.scanInputDirectory(inputDirEl.value.trim(), mode);
      inputDirEl.addEventListener("change", rescan);
      inputDirEl.addEventListener("blur", rescan);
    }
  }

  /** The published Job Coordinator, or an idle one before the first View State. */
  get run() {
    return this.runtime.state()?.run ?? IDLE_RUN;
  }

  /** The mode that owns the batch: the Job Coordinator's, else that of a job still active. */
  get owner() {
    const state = this.runtime.state();
    return state?.run?.mode ?? (state?.job?.active ? state.job.mode : null);
  }

  appendError(text) {
    this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${text}` });
  }

  /**
   * Sends Run, which the core answers once the start settles. A refusal or a failed start is
   * logged; a draft that built no request also opens the settings sheet on its flagged field.
   */
  async start() {
    try {
      await this.runtime.send({ type: "run", mode: this.mode });
    } catch (err) {
      this.appendError(err);
      // The refusal is answered after the state that flagged the field was applied.
      const invalid = this.runtime.state()?.[this.mode]?.draft?.invalid;
      const el = invalid && document.getElementById(`${this.mode}-${DRAFT_FIELDS[invalid]?.at(-1)}`);
      if (el) {
        // Every flagged field lives in the closed settings sheet; open it so the error shows.
        this.runtime.openSettings?.(this.mode);
        el.focus();
      }
    }
  }

  /** Sends Stop, which the core answers once the teardown is over. */
  async stop() {
    // A launched job's teardown can take a while, so announce the wait first.
    if (this.owner === this.mode && this.run.phase === "running") {
      this.runtime.appendLog(this.logEl, { kind: "note", text: "Stop requested; waiting for shutdown." });
    }
    try {
      await this.runtime.send({ type: "stop", mode: this.mode });
    } catch (err) {
      this.appendError(err);
    }
  }

  async clearCache() {
    const confirmed = await this.runtime.showModal(
      "Clear inspection cache",
      "Are you sure you want to clear the persistent inspection cache?",
      { confirmLabel: "Clear cache" }
    );
    if (!confirmed) return;

    try {
      const msg = await this.runtime.invoke("clear_cache");
      this.runtime.appendLog(this.logEl, { kind: "cache", text: msg || "Cache cleared." });
    } catch (err) {
      this.appendError(err);
    }
  }

  /**
   * True while the user may change `mode`'s selection: the core refuses a change while a start or
   * a job holds the batch, and while the mode's own inspection runs. A rescan of one tab leaves
   * the other tab's selection alone.
   */
  canMutateQueueSelection(mode) {
    if (isBusy(this.runtime.state())) return false;
    return !this.runtime.queueFor(mode).isScanning;
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
    const run = this.run;
    const owner = this.owner;
    const busy = isBusy(state);
    const tracker = this.runtime.queueFor(this.mode);
    const hasItems = (tracker.state?.totalCount ?? 0) > 0;
    const selectionDisabled = hasItems && (tracker.getSelectedCount?.() ?? 0) === 0;

    // A start of this mode clears the log once its request resolved, as its settings show.
    const settings = run.mode === this.mode ? run.settings : null;
    if (settings && !this.shownSettings) this.runtime.clearLog(this.logEl);
    this.shownSettings = settings;

    if (this.runBtn) {
      this.runBtn.disabled = busy || Boolean(tracker.isScanning) || selectionDisabled;
      const verb = document.getElementById(`${this.mode}-dry-run`)?.checked ? "Plan" : "Run";
      const label = hasItems ? `${verb} ${this.mode} · ${tracker.getSelectedCount?.() ?? 0}` : `${verb} ${this.mode}`;
      if (this.runBtn.textContent !== label) this.runBtn.textContent = label;
      this.runBtn.title = this.runBlockedReason(owner, selectionDisabled);
    }
    for (const control of this.formControls) control.disabled = busy;
    // Selection stays frozen through the start and the run: the core captured it at Run and
    // refuses a change, so a click could only desynchronize the UI from the batch. The tracker
    // checks the same predicate on every mutation, so a card re-rendered after this sync is
    // guarded too.
    for (const { id, mode } of BULK_SELECTION_CONTROLS) {
      const control = document.getElementById(id);
      if (control) control.disabled = !this.canMutateQueueSelection(mode);
    }
    for (const mode of QUEUE_MODES) {
      this.runtime.queueFor(mode).selectionLocked = !this.canMutateQueueSelection(mode);
    }
    // Controls outside the settings sheet that a busy batch freezes (the presets, the inspector's
    // Track Assignment controls, Open folder), and the root class the locked notes and labels key off.
    document.querySelectorAll(`.lock-while-job, #${this.mode}-episode-list .empty-state-action`).forEach(control => {
      control.disabled = busy;
    });
    document.documentElement?.classList?.toggle("job-locked", busy);
    const selectionFrozen = !this.canMutateQueueSelection(this.mode);
    document.querySelectorAll(`#${this.mode}-episode-list .ep-select-check, #${this.mode}-episode-list .ep-reorder-btn`).forEach(control => {
      control.disabled = selectionFrozen;
    });
    if (this.stopBtn) {
      const owns = owner === this.mode;
      const stopping = run.phase === "stopping";
      this.stopBtn.disabled = !owns || stopping;
      this.stopBtn.textContent = owns && stopping ? "Stopping…" : "Stop";
      // The bottom bar shows this mode's Stop while it owns the batch, and its Run otherwise.
      this.stopBtn.hidden = !owns;
      if (this.runBtn) this.runBtn.hidden = owns;
    }
    // The field a Run could not build a request from, until it is edited or Run is pressed again.
    const invalid = state?.[this.mode]?.draft?.invalid ?? null;
    for (const [key, suffixes] of Object.entries(DRAFT_FIELDS)) {
      for (const suffix of suffixes) {
        document.getElementById(`${this.mode}-${suffix}`)?.classList?.toggle?.("input-error", key === invalid);
      }
    }
    // The owner's phase (a start shows as running), else how the mode's last batch ended.
    const lifecycle = owner === this.mode ? (run.phase === "stopping" ? "stopping" : "running") : state?.[this.mode]?.outcome;
    this.renderStatus(lifecycle ?? "idle");
  }

  /** Why Run is disabled, for its tooltip; "" when it is not, or when nothing explains it. */
  runBlockedReason(owner, selectionDisabled) {
    if (owner && owner !== this.mode) return `${owner === "remux" ? "Remux" : "Extract"} batch is running`;
    if (this.runtime.queueFor(this.mode).isScanning) return "Scanning the folder";
    if (selectionDisabled) return "Tick at least one episode";
    return "";
  }

  renderStatus(lifecycle) {
    if (!this.statusEl) return;
    const [statusClass, statusLabel] = STATUS_PILLS[lifecycle] ?? STATUS_PILLS.idle;
    const html = `<span class="status-dot"></span> ${statusLabel}`;
    if (this.statusEl.innerHTML === html) return;
    this.statusEl.className = `status-pill ${statusClass}`;
    this.statusEl.innerHTML = html;
  }
}
