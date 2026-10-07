import { buildTrackAssignmentsForManifest, isSameDirectory } from "./queue-state.js";

/** Webview event names emitted by the engine bridge; must match `commands.rs`. */
export const JOB_EVENT = "job-event";
export const JOB_LOG = "job-log";
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

/**
 * Converts the Remux view's seconds entry to the engine's `default_sub_offset_ms`, rounding
 * half away from zero like the CLI's `--sub-offset`. An empty entry is `null`; an explicit
 * "0" stays `0` (ADR-0012). Returns `NaN` when the entry is not a number or its milliseconds
 * are not a safe integer, which serde could not read as an `i64`.
 */
export function subOffsetSecondsToMs(raw) {
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

  /** Takes over a running job from `coordinator.restore()`, which has already attached the coordinator to it. */
  restoreActiveSnapshot(snapshot, unlistenEvent, unlistenDone) {
    this.unlistenEvent = unlistenEvent;
    this.unlistenDone = unlistenDone;
    this.jobId = snapshot.job_id;
    this.launched = true;
    this.setRunning(true);
    // `null`, not undefined: a snapshot without settings must not fall back to the idle defaults.
    this.runtime.updateWorkerChips(true, snapshot.effective_settings ?? null);
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
    const generation = ++this.startGeneration;
    const tracker = this.runtime.queueFor(this.mode);
    const capturedDir = tracker.currentDirectory || tracker.lastScannedPath;
    const selectedIds = capturedDir ? tracker.getSelectedOrderedItemIds() : null;
    const vanishedBeforeScan = capturedDir ? [...(tracker.state?.vanishedSelectedIds ?? [])] : [];
    const assignmentSnapshot = capturedDir ? new Map(tracker.assignments) : null;
    if (!this.runtime.coordinator.begin(this.mode)) return;
    this.runtime.closeSubAssignModal();

    this.setRunning(true);

    // The engine resolves the request before the scan, so a bad value fails here rather than
    // after the scan, and the chips show the values it resolved. `run_job` resolves the
    // complete request again once `selected_items` and `assignments` are set. The coordinator
    // is held across the call so a second Run cannot start meanwhile, which makes a refusal end
    // in `abortStart` (the pill reads "failed") like the other pre-scan failures; the message is
    // appended to the log as it stands, which a refused request leaves uncleared.
    let settings;
    try {
      settings = await this.runtime.invoke("resolve_batch_request", { request: batchRequest });
    } catch (err) {
      if (generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
      this.abortStart();
      return;
    }
    if (generation !== this.startGeneration) return;

    this.runtime.clearLog(this.logEl);
    this.runtime.updateWorkerChips(true, settings);
    this.runtime.telemetryLoop.setJobActive(true);

    const inspected = await this.runtime.scanInputDirectory(inputDir, this.mode, {
      force: true,
      options: { noCache: batchRequest.no_cache },
    });
    if (generation !== this.startGeneration) return;
    if (this.runtime.coordinator.phase !== "starting") {
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
      unlisteners.push(await this.runtime.listen(JOB_LOG, e => {
        if (generation === this.startGeneration || launched) this.runtime.appendLog(this.logEl, e.payload);
      }));
      unlisteners.push(await this.runtime.listen(JOB_EVENT, e => {
        if (generation !== this.startGeneration && !launched) return;
        if (this.jobId === null) {
          earlyEvents.push(e.payload);
          return;
        }
        if (this.isOwnSignal(e.payload)) tracker.handleEvent(e.payload);
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
      this.runtime.coordinator.launched();
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
    if (!this.running) return;
    if (!this.runtime.coordinator.requestStop()) return;
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
        this.runtime.appendLog(this.logEl, "[INFO] Stop requested; waiting for shutdown.");
      }
      await this.runtime.invoke("stop_job", { jobId });
      if (!this.running || generation !== this.startGeneration || this.jobId !== jobId) return;
    } catch (err) {
      if (!this.running || generation !== this.startGeneration) return;
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
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
   * The startup snapshot restore is deliberately excluded. It holds the
   * coordinator in its `restoring` phase to block a concurrent start, but no
   * job owns a batch then, and the user must still be able to adjust a
   * restored queue before running.
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
