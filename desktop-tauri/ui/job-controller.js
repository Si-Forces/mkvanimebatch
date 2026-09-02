import { serializeAssignmentsForManifest } from "./queue-state.js";


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

  buildArgs() {
    if (this.mode !== "remux" && this.mode !== "extract") return [];

    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;
    const args = [];

    const inputDir = val("input-dir");
    if (!inputDir) { this.flagInputError("input-dir", "Input directory is required."); return null; }
    args.push("--input-dir", inputDir);
    const outDir = val("out-dir");     if (outDir)    args.push("--out-dir",   outDir);
    const fontsDir = val("fonts-dir"); if (fontsDir)  args.push("--fonts-dir", fontsDir);
    const offset  = val("sub-offset"); if (offset && offset !== "0") args.push("--sub-offset", offset);
    const workers = val("workers");    if (workers)   args.push("--workers",   workers);
    const ioJobs  = val("io-jobs");    if (ioJobs)    args.push("--io-jobs",   ioJobs);
    if (chk("dry-run"))  args.push("--dry-run");
    if (chk("no-cache")) args.push("--no-cache");

    if (this.mode === "remux") {
      const assetsDir = val("assets-dir"); if (assetsDir) args.push("--assets-dir", assetsDir);
      const bitrate = val("bitrate");    if (bitrate)   args.push("--bitrate",   bitrate);
      const cplx    = val("complexity") || val("cplx-val"); if (cplx) args.push("--complexity", cplx);
      const ffmpegThreads = val("ffmpeg-threads"); if (ffmpegThreads) args.push("--ffmpeg-threads", ffmpegThreads);
      if (chk("keep-fonts")) args.push("--keep-fonts");
    }

    return args;
  }

  abortStart(cancelled = false) {
    if (cancelled) this.runtime.queueFor(this.mode).finalize(-1, true);
    this.running = false;
    this.launched = false;
    this.lifecycle = cancelled ? "cancelled" : "failed";
    this.runtime.updateWorkerChips(false);
    this.runtime.telemetryLoop.setJobActive(false);
    this.cleanupListeners();
    this.runtime.coordinator.finish();
    this.renderStatus();
  }

  async start() {
    if (this.running || !this.runtime.coordinator.canStart()) return;
    const args = this.buildArgs();
    if (!args) return;
    const inputDir = args[args.indexOf("--input-dir") + 1];
    const generation = ++this.startGeneration;
    const assignmentSnapshot = new Map(this.runtime.queueFor(this.mode).assignments);
    if (!this.runtime.coordinator.begin(this.mode)) return;
    this.runtime.closeSubAssignModal();

    this.setRunning(true);
    this.runtime.clearLog(this.logEl);
    this.lastExitCode = null;

    const workers = parseInt(document.getElementById(`${this.mode}-workers`)?.value || '8');
    const ioJobs  = parseInt(document.getElementById(`${this.mode}-io-jobs`)?.value || '2');
    this.runtime.updateWorkerChips(true, workers, ioJobs);
    this.runtime.telemetryLoop.setJobActive(true);

    const inspected = await this.runtime.scanInputDirectory(inputDir, this.mode, { force: true });
    if (generation !== this.startGeneration || !this.runtime.coordinator.isStarting) {
      this.runtime.appendLog(this.logEl, "[INFO] Start aborted by stop request.");
      this.abortStart(true);
      return;
    }
    if (!inspected) {
      this.runtime.appendLog(this.logEl, "[ERROR] Input inspection failed; job was not started.");
      this.abortStart();
      return;
    }

    const tracker = this.runtime.queueFor(this.mode);
    const assignmentsJson = this.mode === "remux"
      ? serializeAssignmentsForManifest(assignmentSnapshot, tracker.lastManifest)
      : null;
    this.cleanupListeners();

    const earlyEvents = [];
    const earlyDone = [];

    try {
      this.unlistenLog = await this.runtime.listen("job-log", e => this.runtime.appendLog(this.logEl, e.payload));
      this.unlistenEvent = await this.runtime.listen("job-event", e => {
        if (this.jobId === null) {
          earlyEvents.push(e);
          return;
        }
        if (e.payload?.job_id === this.jobId && (!e.payload?.mode || e.payload.mode === this.mode)) {
          tracker.handleEvent(e.payload);
        }
      });
      this.unlistenDone = await this.runtime.listen("job-done", e => {
        const payload = e.payload;
        if (this.jobId === null) {
          earlyDone.push(e);
          return;
        }
        if (typeof payload === "object" && payload?.mode && payload.mode !== this.mode) return;
        if (payload?.job_id !== this.jobId) return;
        this.onDone(typeof payload === "object" ? payload.code : payload);
      });
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] Listeners failed: ${err}`);
      this.abortStart();
      return;
    }

    if (generation !== this.startGeneration || this.runtime.coordinator.isStopping) {
      this.runtime.appendLog(this.logEl, "[INFO] Start aborted by stop request.");
      this.abortStart(true);
      return;
    }

    try {
      this.launched = true;
      this.runtime.coordinator.isStarting = false;
      this.launchPromise = this.runtime.invoke("run_job", { mode: this.mode, args, assignmentsJson });
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
    ++this.startGeneration;
    this.runtime.coordinator.isStarting = false;
    this.runtime.coordinator.isStopping = true;
    this.lifecycle = "stopping";
    this.renderStatus();
    this.syncGlobalControls();

    try {
      if (this.launched && this.jobId === null && this.launchPromise) {
        this.jobId = await this.launchPromise;
      }
      await this.runtime.invoke("stop_job", { jobId: this.jobId });
      if (this.launched) {
        this.runtime.appendLog(this.logEl, "[INFO] Stop requested; waiting for shutdown.");
      }
    } catch (err) {
      this.runtime.appendLog(this.logEl, `[ERROR] ${err}`);
      this.runtime.coordinator.isStopping = false;
      this.lifecycle = "running";
      this.renderStatus();
      this.runtime.coordinator.syncControls();
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
    if (this.runBtn) this.runBtn.disabled = busy;
    for (const control of this.formControls) control.disabled = busy;
    document.querySelectorAll(`#view-${this.mode} .sub-assign-btn`).forEach(control => {
      control.disabled = busy;
    });
    document.querySelectorAll(`#view-${this.mode} .empty-state-action`).forEach(control => {
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
