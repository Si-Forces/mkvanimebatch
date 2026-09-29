import { buildTrackAssignmentsForManifest, isSameDirectory } from "./queue-state.js";

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

const isIntegerInRange = (value, min, max) => Number.isInteger(value) && value >= min && value <= max;

/**
 * Rejects out-of-range concurrency values before the forced preflight
 * inspection starts. Returns the error message, or null when the request is valid.
 */
export function validateConcurrency(request) {
  if (request.workers != null && !isIntegerInRange(request.workers, 1, 64)) {
    return `Invalid workers value '${request.workers}': must be an integer between 1 and 64.`;
  }
  if (request.io_jobs != null && !isIntegerInRange(request.io_jobs, 0, 64)) {
    return `Invalid io-jobs value '${request.io_jobs}': must be an integer between 0 and 64.`;
  }
  if (request.ffmpeg_threads != null && !isIntegerInRange(request.ffmpeg_threads, 1, 32)) {
    return `Invalid ffmpeg-threads value '${request.ffmpeg_threads}': must be an integer between 1 and 32.`;
  }
  return null;
}

export class JobController {
  constructor(mode, runtime) {
    this.mode = mode;
    this.runtime = runtime;
    this.running = false;
    this.unlistenLog = null;
    this.unlistenEvent = null;
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
      `#view-${mode} input, #view-${mode} select, #view-${mode} .browse-btn, #view-${mode} .clear-cache-btn`
    );
    this.runtime.clearLog(this.logEl);

    this.runtime.coordinator.register(this);
    this.runtime.queueFor(mode).onSelectionChange = () => this.syncGlobalControls();

    this.runtime.safeAddListener(this.runBtn, "click", () => this.start());
    this.runtime.safeAddListener(this.stopBtn, "click", () => this.stop());
    this.runtime.safeAddListener(
      document.getElementById(`${mode}-clear-log`),
      "click",
      () => this.runtime.clearLog(this.logEl)
    );
    this.runtime.safeAddListener(document.getElementById(`${mode}-clear-cache`), "click", () => this.clearCache());

    const inputDirEl = document.getElementById(`${mode}-input-dir`);
    if (inputDirEl) {
      const rescan = () => this.runtime.scanInputDirectory(inputDirEl.value.trim(), mode);
      inputDirEl.addEventListener("change", rescan);
      inputDirEl.addEventListener("blur", rescan);
    }
  }

  /** True when a `job-event` / `job-done` payload belongs to this controller's current job. */
  isOwnSignal(payload) {
    return payload?.job_id === this.jobId && (!payload?.mode || payload.mode === this.mode);
  }

  restoreActiveSnapshot(snapshot, unlistenEvent, unlistenDone) {
    this.unlistenEvent = unlistenEvent;
    this.unlistenDone = unlistenDone;
    this.jobId = snapshot.job_id;
    this.launched = true;
    this.runtime.coordinator.activeMode = this.mode;
    this.runtime.coordinator.isStarting = false;
    this.runtime.coordinator.isStopping = false;
    this.setRunning(true);
    this.runtime.updateWorkerChips(true);
    this.runtime.telemetryLoop.setJobActive(true);
  }

  restoreTerminalSnapshot(snapshot) {
    this.lifecycle = TERMINAL_LIFECYCLE[snapshot.status] ?? "failed";
    this.renderStatus();
  }

  handleSnapshotSignal(type, payload, replay = false) {
    if (!this.running || !this.isOwnSignal(payload)) return;
    if (type === "done") {
      this.onDone(payload.code);
    } else if (type === "event") {
      const queue = this.runtime.queueFor(this.mode);
      // A pre-fetch progress event may already be reflected in the snapshot.
      if (replay && payload.type === "item_progress" && typeof payload.progress === "number" &&
          payload.progress < (queue.state?.items?.get(payload.id)?.progress ?? -Infinity)) return;
      queue.handleEvent(payload);
    }
  }

  cleanupListeners() {
    for (const key of ["unlistenLog", "unlistenEvent", "unlistenDone"]) {
      if (this[key]) {
        this[key]();
        this[key] = null;
      }
    }
  }

  flagInputError(inputId, message) {
    const el = document.getElementById(`${this.mode}-${inputId}`);
    if (el) {
      el.classList.add("input-error");
      el.focus();
      const clear = () => el.classList.remove("input-error");
      el.addEventListener("input", clear, { once: true });
      el.addEventListener("change", clear, { once: true });
    }
    this.runtime.appendLog(this.logEl, `[ERROR] ${message}`);
  }

  buildBatchRequest() {
    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;

    const inputDir = val("input-dir");
    if (!inputDir) {
      this.flagInputError("input-dir", "Input directory is required.");
      return null;
    }
    const offset = val("sub-offset");
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
      ffmpegThreads = ffmpegVal ? parseInt(ffmpegVal, 10) : null;
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
      workers: workersVal ? parseInt(workersVal, 10) : null,
      io_jobs: ioJobsVal !== "" ? parseInt(ioJobsVal, 10) : null,
      ffmpeg_threads: ffmpegThreads,
      sub_offset: offset && offset !== "0" ? parseFloat(offset) : null,
      dry_run: chk("dry-run"),
      no_cache: chk("no-cache"),
      assignments: null,
      selected_items: null,
    };
  }

  abortStart(cancelled = false) {
    if (cancelled) this.runtime.queueFor(this.mode).finalize(-1, true);
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
    const concurrencyError = validateConcurrency(batchRequest);
    if (concurrencyError) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${concurrencyError}`);
      return;
    }
    const generation = ++this.startGeneration;
    const tracker = this.runtime.queueFor(this.mode);
    const capturedDir = tracker.currentDirectory || tracker.lastScannedPath;
    const selectedIds = capturedDir ? tracker.getSelectedOrderedItemIds() : null;
    const vanishedBeforeScan = capturedDir ? [...(tracker.state?.vanishedSelectedIds ?? [])] : [];
    const assignmentSnapshot = capturedDir ? new Map(tracker.assignments) : null;
    if (!this.runtime.coordinator.begin(this.mode)) return;
    this.runtime.closeSubAssignModal();

    this.setRunning(true);
    this.runtime.clearLog(this.logEl);

    this.runtime.updateWorkerChips(true, batchRequest.workers ?? 8, batchRequest.io_jobs ?? 2);
    this.runtime.telemetryLoop.setJobActive(true);

    const inspected = await this.runtime.scanInputDirectory(inputDir, this.mode, {
      force: true,
      options: { noCache: batchRequest.no_cache },
    });
    if (generation !== this.startGeneration) return;
    if (!this.runtime.coordinator.isStarting) {
      this.runtime.appendLog(this.logEl, "[INFO] Start aborted by stop request.");
      this.abortStart(true);
      return;
    }
    if (!inspected) {
      this.runtime.appendLog(this.logEl, "[ERROR] Input inspection failed; job was not started.");
      this.abortStart();
      return;
    }
    const isSameDir = Boolean(capturedDir) && isSameDirectory(capturedDir, tracker.currentDirectory || tracker.lastScannedPath);

    const effectiveSelectedIds = isSameDir ? selectedIds : tracker.getSelectedOrderedItemIds();
    if (!effectiveSelectedIds || effectiveSelectedIds.length === 0) {
      this.runtime.appendLog(this.logEl, "[ERROR] No episodes selected in queue.");
      this.abortStart();
      return;
    }
    const discoveredIds = new Set(tracker.lastManifest?.items?.map(item => item.id) ?? []);
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
        this.runtime.appendLog(this.logEl, `[ERROR] ${message}`);
        this.abortStart();
        return;
      }
    }
    batchRequest.selected_items = effectiveSelectedIds;

    if (this.mode === "remux") {
      const effectiveAssignments = isSameDir ? assignmentSnapshot : tracker.assignments;
      const fullAssignments = buildTrackAssignmentsForManifest(effectiveAssignments, tracker.lastManifest);
      const selectedSet = new Set(effectiveSelectedIds);
      batchRequest.assignments = fullAssignments?.filter(a => selectedSet.has(a.target_video)) ?? null;
    }
    this.cleanupListeners();

    const earlyEvents = [];
    const earlyDone = [];
    const unlisteners = [];
    let launched = false;

    try {
      unlisteners.push(await this.runtime.listen("job-log", e => {
        if (generation === this.startGeneration || launched) this.runtime.appendLog(this.logEl, e.payload);
      }));
      unlisteners.push(await this.runtime.listen("job-event", e => {
        if (generation !== this.startGeneration && !launched) return;
        if (this.jobId === null) {
          earlyEvents.push(e.payload);
          return;
        }
        if (this.isOwnSignal(e.payload)) tracker.handleEvent(e.payload);
      }));
      unlisteners.push(await this.runtime.listen("job-done", e => {
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
      this.runtime.appendLog(this.logEl, `[ERROR] Listeners failed: ${err}`);
      this.abortStart();
      return;
    }

    if (generation !== this.startGeneration || this.runtime.coordinator.isStopping) {
      unlisteners.forEach(unlisten => unlisten());
      if (generation === this.startGeneration) this.abortStart(true);
      return;
    }
    [this.unlistenLog, this.unlistenEvent, this.unlistenDone] = unlisteners;

    try {
      this.launched = true;
      launched = true;
      this.runtime.coordinator.isStarting = false;
      this.launchPromise = this.runtime.invoke("run_job", { request: batchRequest });
      const jobId = await this.launchPromise;
      if (this.running) {
        this.jobId = jobId;
        for (const payload of earlyEvents) {
          if (this.isOwnSignal(payload)) tracker.handleEvent(payload);
        }
        const done = earlyDone.find(payload => this.isOwnSignal(payload));
        if (done) this.onDone(done.code);
      }
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
      tracker.finalize(-1);
      this.abortStart();
    }
  }

  async stop() {
    if (!this.running || this.runtime.coordinator.isStopping) return;
    const generation = ++this.startGeneration;
    this.runtime.coordinator.isStarting = false;
    this.runtime.coordinator.isStopping = true;
    this.lifecycle = "stopping";
    this.renderStatus();
    this.syncGlobalControls();

    try {
      if (this.launched && this.jobId === null && this.launchPromise) {
        const jobId = await this.launchPromise;
        if (!this.running || generation !== this.startGeneration) return;
        this.jobId = jobId;
      }
      const jobId = this.jobId;
      await this.runtime.invoke("stop_job", { jobId });
      if (!this.running || generation !== this.startGeneration || this.jobId !== jobId) return;
      if (this.launched) {
        this.runtime.appendLog(this.logEl, "[INFO] Stop requested; waiting for shutdown.");
      }
    } catch (err) {
      if (!this.running || generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
      if (this.jobId !== null) {
        this.runtime.coordinator.isStopping = false;
        this.lifecycle = "running";
        this.renderStatus();
        this.runtime.coordinator.syncControls();
        return;
      }
    }
    if (this.jobId === null) {
      this.abortStart(true);
    }
  }

  async clearCache() {
    const confirmed = await this.runtime.showModal(
      "Clear Inspection Cache",
      "Are you sure you want to clear the persistent inspection cache?",
      { confirmLabel: "Clear Cache" }
    );
    if (!confirmed) return;

    try {
      const msg = await this.runtime.invoke("clear_cache");
      this.runtime.appendLog(this.logEl, `[CACHE] ${msg || "Cache cleared."}`);
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
    }
  }

  onDone(code) {
    if (!this.running || this.jobId === null) return;
    const tracker = this.runtime.queueFor(this.mode);
    // `job-done` codes: 0 completed, -1 cancelled, 1 otherwise (architecture.md §5.1).
    const cancelled = this.runtime.coordinator.isStopping || tracker.state?.batchCancelled === true || code === -1;
    tracker.finalize(code, cancelled);
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
    this.runtime.coordinator.syncControls();
    this.renderStatus();
  }

  /**
   * True while a job owns the batch: starting, running, or stopping.
   *
   * `start()` captures the ordered selected IDs *before* the forced inspection,
   * so a selection action landing during preflight would change what the user
   * believes they are running without changing the request. That freeze covers
   * every mode: the captured request belongs to the owning mode, and the other
   * queue is not what is being run.
   *
   * The startup snapshot restore is deliberately excluded. It raises
   * `isRestoring` to block a concurrent start, but no job owns a batch then,
   * and the user must still be able to adjust a restored queue before running.
   */
  isJobActive() {
    return this.runtime.coordinator.activeMode !== null;
  }

  /**
   * True while `mode`'s selection may still be mutated by the user.
   *
   * Beyond an active job, a queue is only locked against *its own* inspection:
   * a background rescan of one tab must not disable the other tab's selection
   * controls while the coordinator is idle.
   */
  canMutateQueueSelection(mode) {
    if (this.isJobActive()) return false;
    return !this.runtime.queueFor(mode).isScanning;
  }

  /**
   * Select All / Deselect All action behind the bulk buttons.
   * Returns true only when the selection was actually mutated.
   */
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
    if (this.runBtn) this.runBtn.disabled = !coordinator.canStart() || Boolean(tracker.isScanning) || selectionDisabled;
    for (const control of this.formControls) control.disabled = busy;
    // Selection stays frozen for the whole preflight/run window: the request
    // captured this queue's selection before the forced scan, so a click now
    // could only desynchronize the UI from the batch. The tracker enforces the
    // same predicate on every mutation, so a card re-rendered after this sync
    // cannot slip an unguarded change through either.
    for (const { id, mode } of BULK_SELECTION_CONTROLS) {
      const control = document.getElementById(id);
      if (control) control.disabled = !this.canMutateQueueSelection(mode);
    }
    for (const mode of QUEUE_MODES) {
      this.runtime.queueFor(mode).selectionLocked = !this.canMutateQueueSelection(mode);
    }
    document.querySelectorAll(`#view-${this.mode} .sub-assign-btn, #view-${this.mode} .empty-state-action`).forEach(control => {
      control.disabled = busy;
    });
    const selectionFrozen = !this.canMutateQueueSelection(this.mode);
    document.querySelectorAll(`#view-${this.mode} .ep-select-check, #view-${this.mode} .ep-reorder-btn`).forEach(control => {
      control.disabled = selectionFrozen;
    });
    if (this.stopBtn) {
      const stopping = coordinator.isStopping && coordinator.activeMode === this.mode;
      this.stopBtn.disabled = coordinator.activeMode !== this.mode || coordinator.isStopping;
      this.stopBtn.textContent = stopping ? "Stopping..." : "■ Stop";
    }
  }

  renderStatus() {
    if (!this.statusEl) return;
    const [statusClass, statusLabel] = STATUS_PILLS[this.lifecycle] ?? STATUS_PILLS.idle;
    this.statusEl.className = `status-pill ${statusClass}`;
    this.statusEl.innerHTML = `<span class="status-dot"></span> ${statusLabel}`;
  }
}
