import { buildTrackAssignmentsForManifest, isSameDirectory } from "./queue-state.js";


export function extractInspectionOptions(argsOrRequest) {
  const req = argsOrRequest || {};
  const options = {};

  if (req.workers !== null && req.workers !== undefined) {
    const w = req.workers;
    if (typeof w !== "number" || !Number.isInteger(w) || w < 1 || w > 64) {
      return { error: `Invalid workers value '${w}': must be an integer between 1 and 64.` };
    }
    options.workers = w;
  }

  const ioJobs = req.ioJobs ?? req.io_jobs;
  if (ioJobs !== null && ioJobs !== undefined) {
    if (typeof ioJobs !== "number" || !Number.isInteger(ioJobs) || ioJobs < 0 || ioJobs > 64) {
      return { error: `Invalid io-jobs value '${ioJobs}': must be an integer between 0 and 64.` };
    }
    options.ioJobs = ioJobs;
  }

  if (req.noCache || req.no_cache) {
    options.noCache = true;
  }

  return { options };
}

export class JobController {
  constructor(mode, runtime) {
    this.mode     = mode;
    this.runtime = runtime;
    this.running  = false;
    this.unlistenLog = null;
    this.unlistenEvent = null;
    this.unlistenDone = null;

    this.logEl     = document.getElementById(`${mode}-log`);
    this.statusEl  = document.getElementById(`${mode}-status`);
    this.lastExitCode = null;
    this.lifecycle = "idle";
    this.startGeneration = 0;
    this.launched = false;
    this.jobId = null;
    this.launchPromise = null;
    this.runBtn    = document.getElementById(`${mode}-run`);
    this.stopBtn   = document.getElementById(`${mode}-stop`);
    this.formControls = document.querySelectorAll(
      `#view-${mode} input, #view-${mode} select, #view-${mode} .browse-btn, #view-${mode} .clear-cache-btn`
    );

    this.runtime.coordinator.register(this);
    const tracker = this.runtime.queueFor?.(mode);
    if (tracker) {
      tracker.onSelectionChange = () => this.syncGlobalControls();
    }


    if (this.runBtn)  this.runtime.safeAddListener(this.runBtn,  "click", () => this.start());
    if (this.stopBtn) this.runtime.safeAddListener(this.stopBtn, "click", () => this.stop());

    const clearLogBtn = document.getElementById(`${mode}-clear-log`);
    if (clearLogBtn) this.runtime.safeAddListener(clearLogBtn, "click", () => this.runtime.clearLog(this.logEl));

    const clearCacheBtn = document.getElementById(`${mode}-clear-cache`);
    if (clearCacheBtn) this.runtime.safeAddListener(clearCacheBtn, "click", () => this.clearCache());

    const inputDirEl = document.getElementById(`${mode}-input-dir`);
    if (inputDirEl) {
      inputDirEl.addEventListener("change", () => this.runtime.scanInputDirectory(inputDirEl.value.trim(), mode));
      inputDirEl.addEventListener("blur", () => this.runtime.scanInputDirectory(inputDirEl.value.trim(), mode));
    }
  }

  restoreActiveSnapshot(snapshot, unlistenEvent, unlistenDone) {
    this.unlistenEvent = unlistenEvent;
    this.unlistenDone = unlistenDone;
    this.jobId = snapshot.job_id;
    this.launched = true;
    this.lastExitCode = null;
    this.runtime.coordinator.activeMode = this.mode;
    this.runtime.coordinator.isStarting = false;
    this.runtime.coordinator.isStopping = false;
    this.setRunning(true);
    this.runtime.updateWorkerChips(true);
    this.runtime.telemetryLoop.setJobActive(true);
  }

  restoreTerminalSnapshot(snapshot) {
    this.lifecycle = snapshot.status === "completed" ? "done" : snapshot.status === "cancelled" ? "cancelled" : "failed";
    this.renderStatus();
  }

  handleSnapshotSignal(type, payload, replay = false) {
    if (!this.running || payload?.job_id !== this.jobId || (payload.mode && payload.mode !== this.mode)) return;
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
    if (this.mode !== "remux" && this.mode !== "extract") return null;

    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;

    const inputDir = val("input-dir");
    if (!inputDir) {
      this.flagInputError("input-dir", "Input directory is required.");
      return null;
    }
    const outDir = val("out-dir") || null;
    const fontsDir = val("fonts-dir") || null;
    const offset = val("sub-offset");
    const subOffset = offset && offset !== "0" ? parseFloat(offset) : null;
    const workersVal = val("workers");
    const workers = workersVal ? parseInt(workersVal, 10) : null;
    const ioJobsVal = val("io-jobs");
    const ioJobs = ioJobsVal !== "" ? parseInt(ioJobsVal, 10) : null;
    const dryRun = chk("dry-run");
    const noCache = chk("no-cache");

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

    const activePresetEl = typeof document?.querySelector === "function"
      ? document.querySelector(".profile-option-item.active")
      : null;
    const presetKey = activePresetEl?.dataset?.preset;
    let preset = null;
    if (presetKey === "default" || presetKey === "balanced") {
      preset = "balanced";
    } else if (presetKey === "hifi" || presetKey === "high_quality") {
      preset = "hifi";
    } else if (presetKey) {
      preset = presetKey;
    }

    return {
      mode: this.mode,
      input_dir: inputDir,
      out_dir: outDir,
      fonts_dir: fontsDir,
      assets_dir: assetsDir,
      preset,
      bitrate,
      complexity,
      workers,
      io_jobs: ioJobs,
      ffmpeg_threads: ffmpegThreads,
      sub_offset: subOffset,
      dry_run: dryRun,
      no_cache: noCache,
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
    const { options, error: optionsError } = extractInspectionOptions(batchRequest);
    if (optionsError) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${optionsError}`);
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
    this.lastExitCode = null;

    const workers = options.workers ?? 8;
    const ioJobs  = options.ioJobs ?? 2;
    this.runtime.updateWorkerChips(true, workers, ioJobs);
    this.runtime.telemetryLoop.setJobActive(true);

    const inspected = await this.runtime.scanInputDirectory(inputDir, this.mode, { force: true, options });
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
      const missingIds = [...new Set([
        ...effectiveSelectedIds.filter(id => !discoveredIds.has(id)),
        ...(tracker.state?.vanishedSelectedIds ?? []),
        ...vanishedBeforeScan,
      ])];
      if (missingIds.length) {
        this.runtime.appendLog(this.logEl, `[ERROR] Selected episodes disappeared from the input directory: ${missingIds.join(", ")}. Change the queue selection to continue; job was not started.`);
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
          earlyEvents.push(e);
          return;
        }
        if (e.payload?.job_id === this.jobId && (!e.payload?.mode || e.payload.mode === this.mode)) {
          tracker.handleEvent(e.payload);
        }
      }));
      unlisteners.push(await this.runtime.listen("job-done", e => {
        if (generation !== this.startGeneration && !launched) return;
        const payload = e.payload;
        if (this.jobId === null) {
          earlyDone.push(e);
          return;
        }
        if (typeof payload === "object" && payload?.mode && payload.mode !== this.mode) return;
        if (payload?.job_id !== this.jobId) return;
        this.onDone(typeof payload === "object" ? payload.code : payload);
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
      this.launchPromise = this.runtime.invoke("run_job", {
        request: batchRequest,
      });
      const jobId = await this.launchPromise;
      if (this.running) {
        this.jobId = jobId;
        for (const e of earlyEvents) {
          if (e.payload?.job_id === this.jobId && (!e.payload?.mode || e.payload.mode === this.mode)) {
            tracker.handleEvent(e.payload);
          }
        }
        for (const e of earlyDone) {
          const payload = e.payload;
          if (typeof payload === "object" && payload?.job_id === this.jobId && (!payload?.mode || payload.mode === this.mode)) {
            this.onDone(typeof payload === "object" ? payload.code : payload);
            break;
          }
        }
      }
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
      tracker.finalize(-1);
      this.lastExitCode = -1;
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
      `Are you sure you want to clear the persistent inspection cache for ${this.mode}?`,
      { confirmLabel: "Clear Cache" }
    );
    if (!confirmed) return;

    try {
      const msg = await this.runtime.invoke("clear_cache", { mode: this.mode });
      this.runtime.appendLog(this.logEl, `[CACHE] ${msg || "Cache cleared."}`);
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
    }
  }

  onDone(code) {
    if (!this.running || this.jobId === null) return;
    const tracker = this.runtime.queueFor(this.mode);
    const cancelled = this.runtime.coordinator.isStopping || tracker.state?.batchCancelled === true || code === 130;
    tracker.finalize(code, cancelled);
    this.lastExitCode = tracker.state?.batchSuccess ? code : -1;
    this.lifecycle = cancelled ? "cancelled" : (this.lastExitCode === 0 ? "done" : "failed");
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
    this.lifecycle = running ? "running" : this.lifecycle;
    this.runtime.coordinator.syncControls();
    this.renderStatus();
  }

  syncGlobalControls() {
    const busy = this.runtime.coordinator.activeMode !== null;
    const tracker = this.runtime.queueFor?.(this.mode);
    const hasItems = (tracker?.state?.totalCount ?? 0) > 0;
    const selectedCount = tracker?.getSelectedCount?.() ?? 0;
    const selectionDisabled = hasItems && selectedCount === 0;
    const isScanning = Boolean(tracker?.isScanning);
    if (this.runBtn) this.runBtn.disabled = busy || this.runtime.coordinator.isStarting || isScanning || selectionDisabled;
    for (const control of this.formControls) control.disabled = busy;
    document.querySelectorAll(`#view-${this.mode} .sub-assign-btn`).forEach(control => {
      control.disabled = busy;
    });
    document.querySelectorAll(`#view-${this.mode} .empty-state-action`).forEach(control => {
      control.disabled = busy;
    });
    document.querySelectorAll(`#view-${this.mode} .ep-select-check, #view-${this.mode} .ep-reorder-btn`).forEach(control => {
      control.disabled = busy;
    });
    if (this.stopBtn) {
      this.stopBtn.disabled = this.runtime.coordinator.activeMode !== this.mode || this.runtime.coordinator.isStopping;
      this.stopBtn.textContent = this.runtime.coordinator.isStopping && this.runtime.coordinator.activeMode === this.mode
        ? "Stopping..."
        : "■ Stop";
    }
  }

  renderStatus() {
    const el = this.statusEl;
    if (!el) return;
    let statusClass = "status-idle";
    let statusLabel = "Idle";
    if (this.lifecycle === "running") {
      statusClass = "status-running";
      statusLabel = "Running";
    } else if (this.lifecycle === "stopping") {
      statusClass = "status-running";
      statusLabel = "Stopping";
    } else if (this.lifecycle === "cancelled") {
      statusClass = "status-cancelled";
      statusLabel = "Cancelled";
    } else if (this.lifecycle === "done") {
      statusClass = "status-success";
      statusLabel = "Done";
    } else if (this.lifecycle === "failed") {
      statusClass = "status-error";
      statusLabel = "Failed";
    }
    el.className = "status-pill " + statusClass;
    el.innerHTML = `<span class="status-dot"></span> ${statusLabel}`;
  }
}
