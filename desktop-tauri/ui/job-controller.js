import { isSameDirectory } from "./queue-view.js";

/** Webview event names emitted by the engine bridge; must match `src-tauri/src/publisher.rs` (`cleanup-error`: `commands.rs`). */
export const VIEW_STATE = "view-state";
const JOB_LOG = "job-log";
export const JOB_DONE = "job-done";
export const CLEANUP_ERROR = "cleanup-error";

/**
 * The four queue bulk-selection buttons, each with the selection it applies.
 *
 * Every controller touches all of them because ownership of *any* mode freezes
 * selection in *both* queues, and `app.js` wires the click handlers from this
 * same list — so a button cannot end up disabled with no handler behind it, nor
 * enabled with a handler that silently refuses.
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

const TERMINAL_LIFECYCLE = { completed: "done", cancelled: "cancelled" };

/** The `job-done` code of a finished Job View's status (architecture.md §5.1). */
const DONE_CODES = { completed: 0, cancelled: -1 };

/**
 * Converts the Remux view's seconds entry to the engine's `default_sub_offset_ms`, rounding
 * half away from zero like the CLI's `--sub-offset`. An empty entry is `null`; an explicit
 * "0" stays `0` (ADR-0012). Returns `NaN` when the entry is not a number or its milliseconds
 * are not a safe integer, which serde could not read as an `i64`.
 *
 * TODO(S3, #113): this bounds with Number.isSafeInteger (±2^53) while
 * mkv_core::request::seconds_to_ms bounds with i64; S3 deletes this copy for the Rust one.
 */
function subOffsetSecondsToMs(raw) {
  if (raw === "") return null;
  const seconds = Number(raw);
  // Math.round sends -x.5 toward +Infinity, so round the magnitude and restore the sign.
  const ms = Math.sign(seconds) * Math.round(Math.abs(seconds) * 1000);
  // `+ 0` turns -0 into 0.
  return Number.isSafeInteger(ms) ? ms + 0 : NaN;
}

export class JobController {
  constructor(mode, runtime) {
    this.mode = mode;
    this.runtime = runtime;
    this.running = false;
    this.unlistenLog = null;
    this.unlistenDone = null;
    this.lifecycle = "idle";
    this.startGeneration = 0;
    this.launched = false;
    this.jobId = null;
    this.launchPromise = null;

    this.logEl = document.getElementById(`${mode}-log`);
    this.statusEl = document.getElementById(`${mode}-status`);
    this.runBtn = document.getElementById(`${mode}-run`);
    this.stopBtn = document.getElementById(`${mode}-stop`);
    this.formControls = document.querySelectorAll(
      `#${mode}-settings input, #${mode}-settings select, #${mode}-settings button`
    );
    this.runtime.clearLog(this.logEl);

    this.runtime.coordinator.register(this);

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

  /** True when a `job-done` payload belongs to this controller's current job. */
  isOwnSignal(payload) {
    return payload?.job_id === this.jobId && (!payload?.mode || payload.mode === this.mode);
  }

  /**
   * Takes over `job`, the Job View a webview (re)load found: a finished one only sets the status
   * pill; a running one moves the coordinator to `running` for this mode and waits for its
   * `job-done`. The View State is read again once that listener is bound, so a job that ended in
   * between, whose `job-done` nobody heard, still ends here.
   */
  async adopt(job) {
    if (!job.active) {
      this.lifecycle = TERMINAL_LIFECYCLE[job.status] ?? "failed";
      this.renderStatus();
      return;
    }
    if (!this.runtime.coordinator.adopt(this.mode)) return;
    this.jobId = job.job_id;
    this.launched = true;
    this.setRunning(true);
    // `null`, not undefined: a job without settings must not fall back to the idle defaults.
    this.runtime.updateWorkerChips(true, job.effective_settings ?? null);
    this.runtime.telemetryLoop.setJobActive(true);
    try {
      const unlisten = await this.runtime.listen(JOB_DONE, e => {
        if (this.isOwnSignal(e.payload)) this.onDone(e.payload.code);
      });
      if (this.jobId === job.job_id) this.unlistenDone = unlisten;
      else unlisten();
    } catch (err) {
      console.error("[job-done] listener failed:", err);
    }
    const current = (await this.runtime.syncState())?.job;
    if (this.jobId === job.job_id && current?.job_id === job.job_id && !current.active) {
      this.onDone(DONE_CODES[current.status] ?? 1);
    }
  }

  cleanupListeners() {
    for (const key of ["unlistenLog", "unlistenDone"]) {
      if (this[key]) {
        this[key]();
        this[key] = null;
      }
    }
  }

  flagInputError(inputId, message) {
    const el = document.getElementById(`${this.mode}-${inputId}`);
    if (el) {
      // Every flagged field lives in the closed settings sheet; open it so the error shows.
      this.runtime.openSettings?.(this.mode);
      el.classList.add("input-error");
      el.focus();
      const clear = () => el.classList.remove("input-error");
      el.addEventListener("input", clear, { once: true });
      el.addEventListener("change", clear, { once: true });
    }
    this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${message}` });
  }

  buildBatchRequest() {
    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;

    const inputDir = val("input-dir");
    if (!inputDir) {
      this.flagInputError("input-dir", "Input directory is required.");
      return null;
    }
    const offsetRaw = val("sub-offset");
    const subOffsetMs = subOffsetSecondsToMs(offsetRaw);
    if (Number.isNaN(subOffsetMs)) {
      this.flagInputError("sub-offset", `Invalid sub-offset value '${offsetRaw}': must be a number of seconds within range.`);
      return null;
    }
    const workersVal = val("workers");
    const ioJobsVal = val("io-jobs");

    let assetsDir = null;
    let bitrate = null;
    let complexity = null;
    let ffmpegThreads = null;

    if (this.mode === "remux") {
      assetsDir = val("assets-dir") || null;
      bitrate = val("bitrate") || null;
      const cplx = val("complexity") || val("cplx-val");
      complexity = cplx ? parseInt(cplx, 10) : null;
      const ffmpegVal = val("ffmpeg-threads");
      ffmpegThreads = ffmpegVal ? Number(ffmpegVal) : null;
    }

    return {
      mode: this.mode,
      input_dir: inputDir,
      out_dir: val("out-dir") || null,
      fonts_dir: val("fonts-dir") || null,
      assets_dir: assetsDir,
      preset: document.querySelector(".profile-option-item.active")?.dataset.preset || null,
      bitrate,
      complexity,
      workers: workersVal ? Number(workersVal) : null,
      io_jobs: ioJobsVal !== "" ? Number(ioJobsVal) : null,
      ffmpeg_threads: ffmpegThreads,
      default_sub_offset_ms: subOffsetMs,
      dry_run: chk("dry-run"),
      no_cache: chk("no-cache"),
      assignments: null,
      selected_items: null,
    };
  }

  /** Ends a start that ran no job. No batch started, so no item has an outcome and the queue stays as it was. */
  abortStart(cancelled = false) {
    this.running = false;
    this.launched = false;
    this.jobId = null;
    this.launchPromise = null;
    this.lifecycle = cancelled ? "cancelled" : "failed";
    this.runtime.updateWorkerChips(false);
    this.runtime.telemetryLoop.setJobActive(false);
    this.cleanupListeners();
    this.runtime.coordinator.finish();
    this.renderStatus();
  }

  async start() {
    if (this.running || !this.runtime.coordinator.canStart()) return;
    const batchRequest = this.buildBatchRequest();
    if (!batchRequest) return;
    const inputDir = batchRequest.input_dir;
    const generation = ++this.startGeneration;
    const tracker = this.runtime.queueFor(this.mode);
    const capturedDir = tracker.currentDirectory;
    const selectedIds = capturedDir ? tracker.getSelectedOrderedItemIds() : null;
    const vanishedBeforeScan = capturedDir ? [...(tracker.state?.vanishedSelectedIds ?? [])] : [];
    const assignmentSnapshot = capturedDir ? new Map(tracker.assignments) : null;
    if (!this.runtime.coordinator.begin(this.mode)) return;

    this.setRunning(true);

    // Resolve the request before the scan, so a bad value fails before it and the chips show the
    // resolved values. `run_job` resolves the complete request again once `selected_items` and
    // `assignments` are set. The coordinator stays held across the call so a second Run cannot
    // start; a refusal ends in `abortStart` (the pill reads "failed") like the other pre-scan
    // failures, and its message is appended to the log, which is not cleared yet.
    let settings;
    try {
      settings = await this.runtime.invoke("resolve_batch_request", { request: batchRequest });
    } catch (err) {
      if (generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${err}` });
      this.abortStart();
      return;
    }
    if (generation !== this.startGeneration) return;

    this.runtime.clearLog(this.logEl);
    this.runtime.updateWorkerChips(true, settings);
    this.runtime.telemetryLoop.setJobActive(true);

    const inspected = await this.runtime.scanInputDirectory(inputDir, this.mode, {
      force: true,
      noCache: batchRequest.no_cache,
    });
    if (generation !== this.startGeneration) return;
    if (!inspected) {
      this.runtime.appendLog(this.logEl, { kind: "error", text: "ERROR: Input inspection failed; job was not started." });
      this.abortStart();
      return;
    }
    const isSameDir = Boolean(capturedDir) && isSameDirectory(capturedDir, tracker.currentDirectory);

    const effectiveSelectedIds = isSameDir ? selectedIds : tracker.getSelectedOrderedItemIds();
    if (!effectiveSelectedIds || effectiveSelectedIds.length === 0) {
      this.runtime.appendLog(this.logEl, { kind: "error", text: "ERROR: No episodes selected in queue." });
      this.abortStart();
      return;
    }
    const discoveredIds = new Set(tracker.manifestItemIds());
    if (isSameDir) {
      const reappearedIds = vanishedBeforeScan.filter(id => discoveredIds.has(id));
      const missingIds = [...new Set([
        ...effectiveSelectedIds.filter(id => !discoveredIds.has(id)),
        ...(tracker.state?.vanishedSelectedIds ?? []),
        ...vanishedBeforeScan,
      ])];
      if (missingIds.length) {
        const message = reappearedIds.length
          ? `Queue selection changed during inspection; previously missing selected episodes reappeared: ${reappearedIds.join(", ")}. Review the queue and press Run again; job was not started.`
          : `Selected episodes disappeared from the input directory: ${missingIds.join(", ")}. Change the queue selection to continue; job was not started.`;
        this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${message}` });
        this.abortStart();
        return;
      }
    }
    batchRequest.selected_items = effectiveSelectedIds;

    if (this.mode === "remux") {
      const effectiveAssignments = isSameDir ? assignmentSnapshot : tracker.assignments;
      // Only the assignments of items the forced scan still found, and of those the selected ones.
      const surviving = [...effectiveAssignments.values()].filter(a => discoveredIds.has(a.target_video));
      const selectedSet = new Set(effectiveSelectedIds);
      batchRequest.assignments = surviving.length ? surviving.filter(a => selectedSet.has(a.target_video)) : null;
    }
    this.cleanupListeners();

    const earlyDone = [];
    const unlisteners = [];
    let launched = false;

    try {
      unlisteners.push(await this.runtime.listen(JOB_LOG, e => {
        if (generation === this.startGeneration || launched) this.runtime.appendLog(this.logEl, e.payload);
      }));
      unlisteners.push(await this.runtime.listen(JOB_DONE, e => {
        if (generation !== this.startGeneration && !launched) return;
        if (this.jobId === null) {
          earlyDone.push(e.payload);
          return;
        }
        if (this.isOwnSignal(e.payload)) this.onDone(e.payload.code);
      }));
    } catch (err) {
      unlisteners.forEach(unlisten => unlisten());
      if (generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: Listeners failed: ${err}` });
      this.abortStart();
      return;
    }

    if (generation !== this.startGeneration) {
      unlisteners.forEach(unlisten => unlisten());
      return;
    }
    [this.unlistenLog, this.unlistenDone] = unlisteners;

    try {
      this.launched = true;
      launched = true;
      this.runtime.coordinator.launched();
      this.launchPromise = this.runtime.invoke("run_job", { request: batchRequest });
      const jobId = await this.launchPromise;
      if (this.running) {
        this.jobId = jobId;
        const done = earlyDone.find(payload => this.isOwnSignal(payload));
        if (done) this.onDone(done.code);
      }
    } catch (err) {
      this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${err}` });
      this.abortStart();
    }
  }

  async stop() {
    if (!this.running) return;
    if (!this.runtime.coordinator.requestStop()) return;
    // Bumped in the same tick as `requestStop`, so a `start()` still before `run_job` sees a stale
    // generation at its next checkpoint and returns; this Stop ends it (`abortStart` below). Once
    // `run_job` is launched, `job-done` ends the job.
    const generation = ++this.startGeneration;
    this.lifecycle = "stopping";
    this.renderStatus();

    try {
      if (this.launched && this.jobId === null && this.launchPromise) {
        const jobId = await this.launchPromise;
        if (!this.running || generation !== this.startGeneration) return;
        this.jobId = jobId;
      }
      const jobId = this.jobId;
      // stop_job resolves only once teardown is over, so announce the wait first.
      if (this.launched) {
        this.runtime.appendLog(this.logEl, { kind: "note", text: "Stop requested; waiting for shutdown." });
      }
      await this.runtime.invoke("stop_job", { jobId });
      if (!this.running || generation !== this.startGeneration || this.jobId !== jobId) return;
    } catch (err) {
      if (!this.running || generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${err}` });
      if (this.jobId !== null) {
        this.runtime.coordinator.stopFailed();
        this.lifecycle = "running";
        this.renderStatus();
        return;
      }
    }
    if (this.jobId === null) {
      this.abortStart(true);
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
      this.runtime.appendLog(this.logEl, { kind: "error", text: `ERROR: ${err}` });
    }
  }

  onDone(code) {
    if (!this.running || this.jobId === null) return;
    const tracker = this.runtime.queueFor(this.mode);
    // `job-done` codes: 0 completed, -1 cancelled, 1 otherwise (architecture.md §5.1). A Stop
    // that lost the race to a completed job is no cancellation: only the terminal says so. The
    // core publishes the state a job ended in before its `job-done`, so the queue holds it.
    const cancelled = tracker.state?.batchCancelled === true || code === -1;
    const succeeded = Boolean(tracker.state?.batchSuccess) && code === 0;
    this.lifecycle = cancelled ? "cancelled" : (succeeded ? "done" : "failed");
    this.launched = false;
    this.jobId = null;
    this.launchPromise = null;
    this.setRunning(false);
    this.runtime.updateWorkerChips(false);
    this.runtime.telemetryLoop.setJobActive(false);
    this.runtime.coordinator.finish();
    this.cleanupListeners();
  }

  setRunning(running) {
    this.running = running;
    if (running) this.lifecycle = "running";
    this.renderStatus();
  }

  /**
   * True while a job owns the batch: starting, running, or stopping.
   *
   * `start()` captures the ordered selected IDs before the forced inspection, so a selection
   * change during preflight would change what the user believes they run without changing the
   * request. The freeze covers both modes' queues.
   */
  isJobActive() {
    return this.runtime.coordinator.activeMode !== null;
  }

  /**
   * True while the user may change `mode`'s selection. Beyond an active job, a queue is locked
   * only by its own inspection: a rescan of one tab leaves the other tab's selection alone.
   */
  canMutateQueueSelection(mode) {
    if (this.isJobActive()) return false;
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
    const coordinator = this.runtime.coordinator;
    const busy = this.isJobActive();
    const tracker = this.runtime.queueFor(this.mode);
    const hasItems = (tracker.state?.totalCount ?? 0) > 0;
    const selectionDisabled = hasItems && (tracker.getSelectedCount?.() ?? 0) === 0;
    if (this.runBtn) {
      this.runBtn.disabled = !coordinator.canStart() || Boolean(tracker.isScanning) || selectionDisabled;
      const verb = document.getElementById(`${this.mode}-dry-run`)?.checked ? "Plan" : "Run";
      const label = hasItems ? `${verb} ${this.mode} · ${tracker.getSelectedCount?.() ?? 0}` : `${verb} ${this.mode}`;
      if (this.runBtn.textContent !== label) this.runBtn.textContent = label;
      this.runBtn.title = this.runBlockedReason(selectionDisabled);
    }
    for (const control of this.formControls) control.disabled = busy;
    // Selection stays frozen through preflight and the run: the request captured it before the
    // forced scan, so a click could only desynchronize the UI from the batch. The tracker checks
    // the same predicate on every mutation, so a card re-rendered after this sync is guarded too.
    for (const { id, mode } of BULK_SELECTION_CONTROLS) {
      const control = document.getElementById(id);
      if (control) control.disabled = !this.canMutateQueueSelection(mode);
    }
    for (const mode of QUEUE_MODES) {
      this.runtime.queueFor(mode).selectionLocked = !this.canMutateQueueSelection(mode);
    }
    // Controls outside the settings sheet that a job freezes (the presets, the inspector's Track
    // Assignment controls, Open folder), and the root class the locked notes and labels key off.
    document.querySelectorAll(`.lock-while-job, #${this.mode}-episode-list .empty-state-action`).forEach(control => {
      control.disabled = busy;
    });
    document.documentElement?.classList?.toggle("job-locked", busy);
    const selectionFrozen = !this.canMutateQueueSelection(this.mode);
    document.querySelectorAll(`#${this.mode}-episode-list .ep-select-check, #${this.mode}-episode-list .ep-reorder-btn`).forEach(control => {
      control.disabled = selectionFrozen;
    });
    if (this.stopBtn) {
      const owns = coordinator.activeMode === this.mode;
      const stopping = coordinator.isStopping && owns;
      this.stopBtn.disabled = !owns || coordinator.isStopping;
      this.stopBtn.textContent = stopping ? "Stopping…" : "Stop";
      // The bottom bar shows this mode's Stop while it owns the job, and its Run otherwise.
      this.stopBtn.hidden = !owns;
      if (this.runBtn) this.runBtn.hidden = owns;
    }
  }

  /** Why Run is disabled, for its tooltip; "" when it is not, or when nothing explains it. */
  runBlockedReason(selectionDisabled) {
    const owner = this.runtime.coordinator.activeMode;
    if (owner && owner !== this.mode) return `${owner === "remux" ? "Remux" : "Extract"} batch is running`;
    if (this.runtime.queueFor(this.mode).isScanning) return "Scanning the folder";
    if (selectionDisabled) return "Tick at least one episode";
    return "";
  }

  renderStatus() {
    if (!this.statusEl) return;
    const [statusClass, statusLabel] = STATUS_PILLS[this.lifecycle] ?? STATUS_PILLS.idle;
    this.statusEl.className = `status-pill ${statusClass}`;
    this.statusEl.innerHTML = `<span class="status-dot"></span> ${statusLabel}`;
  }
}
