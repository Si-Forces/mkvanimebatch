/* ─── MKV Anime Batch — Frontend App (Tokyo Glass + OLED Pro Hybrid) ───────
   Uses window.__TAURI__ (withGlobalTauri: true in tauri.conf.json)
──────────────────────────────────────────────────────────────────────────── */

"use strict";

// ─── Preset Configurations ──────────────────────────────────────────────────

const PRESETS = {
  default: {
    name: "Opus 192k + Keep Fonts",
    bitrate: "192k",
    complexity: "10",
    workers: "8",
    ioJobs: "2",
    subOffset: "0",
    keepFonts: true,
    dryRun: false,
    noCache: false,
  },
  fast: {
    name: "Fast Muxing (128k)",
    bitrate: "128k",
    complexity: "5",
    workers: "16",
    ioJobs: "4",
    subOffset: "0",
    keepFonts: false,
    dryRun: false,
    noCache: false,
  },
  high_quality: {
    name: "Hi-Fi Master (320k)",
    bitrate: "320k",
    complexity: "10",
    workers: "8",
    ioJobs: "2",
    subOffset: "0",
    keepFonts: true,
    dryRun: false,
    noCache: false,
  }
};

function applyPreset(presetKey) {
  const p = PRESETS[presetKey];
  const nameEl = document.getElementById("active-profile-name");

  if (!p) {
    if (nameEl) nameEl.textContent = "Custom Configuration";
    document.querySelectorAll(".profile-option-item").forEach(item => {
      item.classList.toggle("active", item.dataset.preset === "custom");
    });
    return;
  }

  if (nameEl) nameEl.textContent = p.name;

  document.querySelectorAll(".profile-option-item").forEach(item => {
    item.classList.toggle("active", item.dataset.preset === presetKey);
  });

  const setVal = (id, val) => {
    const el = document.getElementById(id);
    if (el) el.value = val;
  };
  const setChk = (id, val) => {
    const el = document.getElementById(id);
    if (el) el.checked = val;
  };

  setVal("remux-bitrate", p.bitrate);
  setVal("remux-complexity", p.complexity);
  setVal("remux-cplx-val", p.complexity);
  setVal("remux-workers", p.workers);
  setVal("remux-io-jobs", p.ioJobs);
  setVal("remux-sub-offset", p.subOffset);
  setChk("remux-keep-fonts", p.keepFonts);
  setChk("remux-dry-run", p.dryRun);
  setChk("remux-no-cache", p.noCache);
}

// ─── Tauri API helpers ──────────────────────────────────────────────────────

async function invoke(cmd, args = {}) {
  const fn = window.__TAURI__?.core?.invoke
    || window.__TAURI__?.tauri?.invoke
    || window.__TAURI__?.invoke
    || window.__TAURI_INTERNALS__?.invoke;

  if (fn) {
    return await fn(cmd, args);
  }
  console.log("[mock invoke]", cmd, args);
  return null;
}

async function listen(event, handler) {
  try {
    const fn = window.__TAURI__?.event?.listen || window.__TAURI__?.listen;
    if (fn) {
      return await fn(event, handler);
    }
    return () => {};
  } catch (err) {
    console.error(`[Listen Error] ${event}:`, err);
    return () => {};
  }
}

function safeAddListener(idOrEl, event, handler) {
  const el = typeof idOrEl === "string" ? document.getElementById(idOrEl) : idOrEl;
  if (el) {
    el.addEventListener(event, handler);
  }
}

// ─── Number Stepper Helper ──────────────────────────────────────────────────
window.stepNumber = function(btn, dir) {
  const input = btn.parentElement.querySelector('input[type="number"]');
  if (!input) return;
  
  const step = parseFloat(input.getAttribute('step') || '1');
  const min = parseFloat(input.getAttribute('min'));
  const max = parseFloat(input.getAttribute('max'));
  let val = parseFloat(input.value) || 0;
  
  val += dir * step;
  if (!isNaN(min) && val < min) val = min;
  if (!isNaN(max) && val > max) val = max;
  
  input.value = parseFloat(val.toPrecision(10));
  input.dispatchEvent(new Event('input'));
};

document.addEventListener('DOMContentLoaded', () => {
  let stepInterval = null;
  let stepTimeout = null;

  document.querySelectorAll('.stepper-btn').forEach(btn => {
    const dir = btn.classList.contains('minus') ? -1 : 1;
    
    const startStepping = () => {
      window.stepNumber(btn, dir);
      stepTimeout = setTimeout(() => {
        stepInterval = setInterval(() => window.stepNumber(btn, dir), 80);
      }, 400);
    };
    
    const stopStepping = () => {
      clearTimeout(stepTimeout);
      clearInterval(stepInterval);
    };
    
    btn.addEventListener('mousedown', startStepping);
    btn.addEventListener('mouseup', stopStepping);
    btn.addEventListener('mouseleave', stopStepping);
  });
});

// ─── Theme ──────────────────────────────────────────────────────────────────

const THEME_KEY = "mkv-theme";

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch (e) {}
  invoke("set_window_theme", { theme: theme });
}

function initTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem(THEME_KEY);
  } catch (e) {}

  if (saved) {
    applyTheme(saved);
    return;
  }
  const prefersDark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  applyTheme(prefersDark ? "dark" : "light");
}

// ─── Navigation ──────────────────────────────────────────────────────────────

function switchView(viewId) {
  document.querySelectorAll(".view").forEach(v => v.classList.remove("active"));
  document.querySelectorAll(".pipeline-tab-btn, .nav-item, .top-tab-item").forEach(n => n.classList.remove("active"));
  const view = document.getElementById(`view-${viewId}`);
  const nav  = document.getElementById(`nav-${viewId}`);
  if (view) {
    view.classList.add("active");
  }
  if (nav) {
    nav.classList.add("active");
  }
}

// ─── Real System Hardware Telemetry & Monitor ─────────────────────────────

let telemetryTimer = null;

function updateWorkerChips(isJobActive = false, workersCount = 8, ioJobsCount = 2) {
  const workerChips = document.querySelectorAll(".metric-workers-chip, #workers-telemetry-chip");
  const ioJobChips = document.querySelectorAll(".metric-iojobs-chip, #iojobs-telemetry-chip");

  workerChips.forEach(chip => {
    chip.innerText = isJobActive ? `⚡ ${workersCount} Workers Active` : `⚡ Standby (${workersCount} Workers)`;
  });
  ioJobChips.forEach(chip => {
    chip.innerText = isJobActive ? `🔀 ${ioJobsCount} IO Jobs Active` : `🔀 Idle (${ioJobsCount} IO Jobs)`;
  });
}

function startTelemetryMonitor() {
  if (telemetryTimer) return;

  const cpuFills = document.querySelectorAll(".metric-cpu-fill, #cpu-telemetry-fill");
  const cpuVals = document.querySelectorAll(".metric-cpu-val, #cpu-telemetry-val");
  const ioFills = document.querySelectorAll(".metric-io-fill, #io-telemetry-fill");
  const ioVals = document.querySelectorAll(".metric-io-val, #io-telemetry-val");
  const ramFills = document.querySelectorAll(".metric-ram-fill, #ram-telemetry-fill");
  const ramVals = document.querySelectorAll(".metric-ram-val, #ram-telemetry-val");

  updateWorkerChips(false);

  const updateMetrics = async () => {
    try {
      const data = await invoke("get_system_telemetry");
      if (data) {
        const cpu = Math.round(data.cpu_pct || 0);
        const ramUsed = data.ram_used_mb || 0;
        const ramTotal = data.ram_total_mb || 16384;
        const ramPct = Math.round(data.ram_pct || 0);
        const ioMb = typeof data.io_mb_s === "number" ? data.io_mb_s : 0;

        cpuFills.forEach(fill => fill.style.width = `${cpu}%`);
        cpuVals.forEach(val => val.innerText = `${cpu}%`);

        ramFills.forEach(fill => fill.style.width = `${ramPct}%`);
        ramVals.forEach(val => val.innerText = `${ramUsed} MB / ${(ramTotal/1024).toFixed(0)} GB`);

        ioFills.forEach(fill => fill.style.width = `${Math.min(100, (ioMb / 300) * 100)}%`);
        ioVals.forEach(val => val.innerText = `${ioMb.toFixed(1)} MB/s`);
      }
    } catch (e) {
      console.log("[Telemetry Error]", e);
    }
  };

  updateMetrics();
  telemetryTimer = setInterval(updateMetrics, 1000);
}

// ─── Real-Time Episode Queue & Progress/Duration Tracker ───────────────────

class QueueTracker {
  constructor(mode) {
    this.mode = mode;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.items = new Map();
    this.completedCount = 0;
    this.totalCount = 0;
  }

  reset() {
    this.items.forEach(item => {
      if (item.timerId) clearInterval(item.timerId);
    });
    this.items.clear();
    this.completedCount = 0;
    this.totalCount = 0;
    if (this.listEl) {
      this.listEl.innerHTML = `
        <div class="empty-state-card">
          <div class="empty-icon-glow">
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path>
              <line x1="12" y1="11" x2="12" y2="17"></line>
              <line x1="9" y1="14" x2="15" y2="14"></line>
            </svg>
          </div>
          <div class="empty-state-title">No Episodes Queued</div>
          <div class="empty-state-desc">Select an input directory on the left to auto-scan MKV files into the batch queue.</div>
        </div>`;
    }
    if (this.countEl) this.countEl.textContent = "Ready";
  }

  populateDirectoryFiles(files, dirPath = "") {
    this.items.forEach(item => {
      if (item.timerId) clearInterval(item.timerId);
    });
    this.items.clear();
    this.completedCount = 0;
    if (this.listEl) this.listEl.innerHTML = "";

    if (!files || files.length === 0) {
      this.reset();
      return;
    }
    this.totalCount = files.length;
    if (this.countEl) this.countEl.textContent = `${files.length} Episodes Queued`;

    const bitrate = document.getElementById(`${this.mode}-bitrate`)?.value?.trim() || "192k";

    files.forEach((filename, idx) => {
      const epCard = this.createCardElement(filename, idx + 1);
      this.listEl.appendChild(epCard.el);
      this.items.set(filename, epCard);

      if (dirPath) {
        const fullPath = `${dirPath}/${filename}`;
        invoke("inspect_mkv_file", { filePath: fullPath }).then(meta => {
          if (meta && epCard && epCard.el) {
            const codecEl = epCard.el.querySelector(".ep-codec");
            const tag1El = epCard.el.querySelector(".track-tag.cyan");
            const tag2El = epCard.el.querySelector(".track-tag.magenta");

            if (codecEl) codecEl.textContent = meta.video_tag;

            if (this.mode === "remux") {
              if (tag1El) {
                tag1El.textContent = meta.has_flac
                  ? `${meta.audio_summary} → Opus ${bitrate}`
                  : `${meta.audio_summary} (Passthrough)`;
              }
              if (tag2El) {
                tag2El.textContent = meta.fonts_count > 0
                  ? `${meta.sub_summary} + ${meta.fonts_count} Fonts`
                  : meta.sub_summary;
              }
            } else {
              if (tag1El) tag1El.textContent = meta.sub_summary;
              if (tag2El) tag2El.textContent = `${meta.fonts_count} Attached Fonts`;
            }
          }
        }).catch(err => {
          console.log("[Inspection Error]", filename, err);
          if (epCard && epCard.el) {
            const codecEl = epCard.el.querySelector(".ep-codec");
            const tag1El = epCard.el.querySelector(".track-tag.cyan");
            const tag2El = epCard.el.querySelector(".track-tag.magenta");

            if (codecEl) codecEl.textContent = this.mode === "extract" ? "SUB & FONTS" : "MKV BATCH";
            if (tag1El) tag1El.textContent = this.mode === "extract" ? "ASS Subtitles (.ass)" : `Audio → Opus ${bitrate}`;
            if (tag2El) tag2El.textContent = this.mode === "extract" ? "Attachment Fonts" : "Subtitles";
          }
        });
      }
    });
  }

  createCardElement(filename, epNum = 1) {
    const el = document.createElement("div");
    el.className = "tokyo-ep-card";
    const numStr = String(epNum).padStart(2, "0");

    const codecTag = "PROBING MKV";
    const trackTag1 = "Inspecting tracks...";
    const trackTag2 = "Reading headers...";

    el.innerHTML = `
      <div class="ep-card-top">
        <div class="ep-badge">EPISODE ${numStr}</div>
        <span class="ep-codec">${codecTag}</span>
        <span class="ep-duration-tag status-tag">Queued</span>
      </div>
      <div class="ep-title"></div>
      <div class="ep-tracks">
        <span class="track-tag cyan">${trackTag1}</span>
        <span class="track-tag magenta">${trackTag2}</span>
      </div>
      <div class="ep-progress-bar">
        <div class="ep-progress-fill" style="width: 0%;"></div>
      </div>
    `;

    const titleEl = el.querySelector(".ep-title");
    if (titleEl) titleEl.textContent = filename;

    return {
      el,
      filename,
      status: "queued",
      statusTag: el.querySelector(".status-tag"),
      badge: el.querySelector(".ep-badge"),
      fill: el.querySelector(".ep-progress-fill"),
      startTime: null,
      timerId: null,
      pct: 0
    };
  }

  getItem(filename) {
    if (!filename) return null;
    const cleanFn = filename.trim();

    if (this.items.has(cleanFn)) return this.items.get(cleanFn);

    for (const [key, item] of this.items.entries()) {
      if (key.toLowerCase() === cleanFn.toLowerCase()) {
        return item;
      }
    }

    for (const [key, item] of this.items.entries()) {
      if (key.toLowerCase().includes(cleanFn.toLowerCase()) || cleanFn.toLowerCase().includes(key.toLowerCase())) {
        return item;
      }
    }

    for (const [key, item] of this.items.entries()) {
      if (item.status === "queued" || item.status === "processing") {
        return item;
      }
    }

    const placeholder = this.listEl.querySelector(".empty-state-card, .empty-log");
    if (placeholder) placeholder.remove();

    const epNum = this.items.size + 1;
    const card = this.createCardElement(cleanFn, epNum);
    this.listEl.appendChild(card.el);
    this.items.set(cleanFn, card);
    this.totalCount++;
    if (this.countEl) this.countEl.textContent = `${this.completedCount} / ${this.totalCount} Completed`;
    return card;
  }

  markProcessing(filename) {
    const item = this.getItem(filename);
    if (!item) return;

    item.status = "processing";
    item.el.className = "tokyo-ep-card processing";
    item.badge.className = "ep-badge purple";
    item.badge.textContent = "PROCESSING";
    item.statusTag.className = "ep-duration-tag yellow status-tag";
    item.statusTag.textContent = "Muxing (0%)";
    item.startTime = Date.now();
    item.pct = 0;
    item.fill.style.width = "0%";

    if (item.timerId) clearInterval(item.timerId);
    item.timerId = setInterval(() => {
      if (item.status === "processing" && item.pct < 92) {
        item.pct += Math.floor(Math.random() * 6) + 2;
        if (item.pct > 92) item.pct = 92;
        item.fill.style.width = `${item.pct}%`;
        const elapsedSec = ((Date.now() - item.startTime) / 1000).toFixed(1);
        item.statusTag.textContent = `${item.pct}% (${elapsedSec}s)`;
      }
    }, 300);
  }

  updateProgress(filename, pct, speedStr = "") {
    const item = this.getItem(filename);
    if (!item) return;
    item.pct = Math.min(100, Math.max(0, pct));
    item.fill.style.width = `${item.pct}%`;
    const elapsedSec = item.startTime ? ((Date.now() - item.startTime) / 1000).toFixed(1) : "0.0";
    const speed = speedStr ? ` • ${speedStr}` : "";
    item.statusTag.textContent = `${item.pct}% (${elapsedSec}s${speed})`;
  }

  markCompleted(filename, durationSec = null) {
    const item = this.getItem(filename);
    if (!item) return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "completed";
    item.pct = 100;
    const dur = durationSec !== null ? parseFloat(durationSec).toFixed(1) : (item.startTime ? ((Date.now() - item.startTime) / 1000).toFixed(1) : "8.5");

    item.el.className = "tokyo-ep-card completed";
    item.badge.className = "ep-badge green";
    item.badge.textContent = "COMPLETED";
    item.statusTag.className = "ep-duration-tag green status-tag";
    item.statusTag.textContent = `Done in ${dur}s`;
    item.fill.className = "ep-progress-fill green";
    item.fill.style.width = "100%";

    this.completedCount++;
    if (this.countEl) {
      this.countEl.textContent = `${this.completedCount} / ${Math.max(this.totalCount, this.completedCount)} Completed`;
    }
  }

  markDryRun(filename) {
    const item = this.getItem(filename);
    if (!item) return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "dry_run";
    item.pct = 100;
    item.el.className = "tokyo-ep-card dry-run";
    item.badge.className = "ep-badge cyan";
    item.badge.textContent = "DRY RUN";
    item.statusTag.className = "ep-duration-tag cyan status-tag";
    item.statusTag.textContent = "Planned (Dry Run)";
    item.fill.className = "ep-progress-fill cyan";
    item.fill.style.width = "100%";

    this.completedCount++;
    if (this.countEl) {
      this.countEl.textContent = `${this.completedCount} / ${Math.max(this.totalCount, this.completedCount)} Planned (Dry Run)`;
    }
  }

  markFailed(filename, errReason = "Failed") {
    const item = this.getItem(filename);
    if (!item) return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "failed";
    item.el.className = "tokyo-ep-card failed";
    item.badge.className = "ep-badge red";
    item.badge.textContent = "FAILED";
    item.statusTag.className = "ep-duration-tag red status-tag";
    item.statusTag.textContent = errReason;
  }
}

const remuxQueue = new QueueTracker("remux");
const extractQueue = new QueueTracker("extract");

async function scanInputDirectory(path, mode) {
  if (!path) return;
  try {
    const files = await invoke("scan_directory", { path });
    if (files && Array.isArray(files)) {
      const tracker = mode === "extract" ? extractQueue : remuxQueue;
      tracker.populateDirectoryFiles(files, path);
    }
  } catch (err) {
    console.log("[Scan Dir] Error:", err);
  }
}

// ─── Log Console & Episode Parser ──────────────────────────────────────────

function classifyLine(text) {
  const t = text.toLowerCase();
  if (t.includes("[+]") || t.includes("success") || t.includes("finished") || t.includes("completed") || t.includes("remuxed cleanly") || t.includes("extracted cleanly") || t.includes("[job_done]") || t.includes("[job_dry_run]")) return "line-ok";
  if (t.includes("[-]") || t.includes("[err]") || t.includes("error") || t.includes("failed") || t.includes("[error]") || t.includes("[job_fail]")) return "line-err";
  if (t.includes("warn") || t.includes("[warn]")) return "line-warn";
  if (t.includes("skip") || t.includes("already complete")) return "line-skip";
  if (t.includes("folder:") || t.includes("dry run") || t.startsWith("extracting") || t.startsWith("processing") || t.includes("[job_start]") || t.includes("[job_progress]")) return "line-info";
  if (t.includes("persistent inspection cache") || t.includes("cache cleared")) return "line-done";
  return "";
}

function appendLog(logEl, text, mode = "remux") {
  if (!logEl) return;
  const placeholder = logEl.querySelector(".empty-state-card, .empty-log, .console-empty-prompt");
  if (placeholder) placeholder.remove();

  const div = document.createElement("div");
  div.className = `log-line ${classifyLine(text)}`;
  div.textContent = text;
  logEl.appendChild(div);
  logEl.scrollTop = logEl.scrollHeight;

  parseLogStream(text, mode);
}

function parseLogStream(text, mode) {
  const tracker = mode === "extract" ? extractQueue : remuxQueue;

  const dryMatch = text.match(/\[JOB_DRY_RUN\]\s*([^|]+)/i);
  if (dryMatch) {
    tracker.markDryRun(dryMatch[1].trim());
    return;
  }

  const startMatch = text.match(/\[JOB_START\]\s*(.+)$/i);
  if (startMatch) {
    tracker.markProcessing(startMatch[1].trim());
    return;
  }

  const progressMatch = text.match(/\[JOB_PROGRESS\]\s*([^|]+)\|\s*(\d+)%/i);
  if (progressMatch) {
    tracker.updateProgress(progressMatch[1].trim(), parseInt(progressMatch[2]));
    return;
  }

  const doneMatch = text.match(/\[JOB_DONE\]\s*([^|]+)\|(?:.*?([\d\.]+)\s*s)?/i);
  if (doneMatch) {
    const fn = doneMatch[1].trim();
    const dur = doneMatch[2] ? doneMatch[2].trim() : null;
    tracker.markCompleted(fn, dur);
    return;
  }

  const failMatch = text.match(/\[JOB_FAIL\]\s*([^|]+)\|\s*(.+)$/i);
  if (failMatch) {
    const fn = failMatch[1].trim();
    const reason = failMatch[2].trim();
    tracker.markFailed(fn, reason);
    return;
  }

  const altStartMatch = text.match(/(?:Processing|Remuxing|Extracting|\bJob\b).*?[:\s]+([\w-]+\.mkv)/i)
    || text.match(/\b([\w-]+\.mkv)\b\s*->/i);
  if (altStartMatch) {
    tracker.markProcessing(altStartMatch[1].trim());
    return;
  }

  const altDoneMatch = text.match(/(?:Remuxed|Extracted|Completed).*?[:\s]+([\w-]+\.mkv)(?:.*?in\s+([\d\.]+)\s*s)?/i);
  if (altDoneMatch) {
    tracker.markCompleted(altDoneMatch[1].trim(), altDoneMatch[2] ? altDoneMatch[2].trim() : null);
  }
}

function clearLog(logEl) {
  if (!logEl) return;
  logEl.innerHTML = `
    <div class="console-empty-prompt">
      <span class="prompt-arrow">&gt;_</span>
      <span class="prompt-text">Console stream output ready. Waiting for job execution...</span>
    </div>`;
}

// ─── Modal Dialog ────────────────────────────────────────────────────────────

function showConfirmModal(title, message) {
  return new Promise(resolve => {
    const modal = document.getElementById("confirm-modal");
    const titleEl = document.getElementById("modal-title");
    const descEl = document.getElementById("modal-desc");
    const cancelBtn = document.getElementById("modal-cancel");
    const confirmBtn = document.getElementById("modal-confirm");

    if (!modal || !cancelBtn || !confirmBtn) {
      resolve(confirm(message));
      return;
    }

    if (titleEl) titleEl.textContent = title;
    if (descEl) descEl.textContent = message;

    modal.classList.add("active");

    const cleanup = (result) => {
      modal.classList.remove("active");
      cancelBtn.removeEventListener("click", onCancel);
      confirmBtn.removeEventListener("click", onConfirm);
      resolve(result);
    };

    const onCancel = () => cleanup(false);
    const onConfirm = () => cleanup(true);

    cancelBtn.addEventListener("click", onCancel);
    confirmBtn.addEventListener("click", onConfirm);
  });
}

class JobController {
  constructor(mode) {
    this.mode     = mode;
    this.running  = false;
    this.unlisten = null;

    this.logEl     = document.getElementById(`${mode}-log`);
    this.statusEl  = document.getElementById(`${mode}-status`);
    this.exitCodeEl= document.getElementById(`${mode}-exit-code`);
    this.runBtn    = document.getElementById(`${mode}-run`);
    this.stopBtn   = document.getElementById(`${mode}-stop`);

    if (this.runBtn)  safeAddListener(this.runBtn,  "click", () => this.start());
    if (this.stopBtn) safeAddListener(this.stopBtn, "click", () => this.stop());

    const clearLogBtn = document.getElementById(`${mode}-clear-log`);
    if (clearLogBtn) safeAddListener(clearLogBtn, "click", () => clearLog(this.logEl));

    const clearCacheBtn = document.getElementById(`${mode}-clear-cache`);
    if (clearCacheBtn) safeAddListener(clearCacheBtn, "click", () => this.clearCache());

    const inputDirEl = document.getElementById(`${mode}-input-dir`);
    if (inputDirEl) {
      inputDirEl.addEventListener("change", () => scanInputDirectory(inputDirEl.value.trim(), mode));
      inputDirEl.addEventListener("blur", () => scanInputDirectory(inputDirEl.value.trim(), mode));
    }
  }

  buildArgs() {
    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;
    const args = [];

    if (this.mode === "remux") {
      const inputDir = val("input-dir");
      if (!inputDir) { alert("Input directory is required."); return null; }
      args.push("--input-dir", inputDir);
      const outDir = val("out-dir");     if (outDir)    args.push("--out-dir",   outDir);
      const fontsDir = val("fonts-dir"); if (fontsDir)  args.push("--fonts-dir", fontsDir);
      const assetsDir = val("assets-dir"); if (assetsDir) args.push("--assets-dir", assetsDir);
      const bitrate = val("bitrate");    if (bitrate)   args.push("--bitrate",   bitrate);
      const offset  = val("sub-offset"); if (offset && offset !== "0") args.push("--sub-offset", offset);
      const workers = val("workers");    if (workers)   args.push("--workers",   workers);
      const ioJobs  = val("io-jobs");    if (ioJobs)    args.push("--io-jobs",   ioJobs);
      const cplx    = val("complexity"); if (cplx)      args.push("--complexity",cplx);
      if (chk("dry-run"))   args.push("--dry-run");
      if (chk("keep-fonts"))args.push("--keep-fonts");
      if (chk("no-cache"))  args.push("--no-cache");
    }

    if (this.mode === "extract") {
      const inputDir = val("input-dir");
      if (!inputDir) { alert("Input directory is required."); return null; }
      args.push("--input-dir", inputDir);
      const outDir = val("out-dir");     if (outDir)    args.push("--out-dir",   outDir);
      const fontsDir = val("fonts-dir"); if (fontsDir)  args.push("--fonts-dir", fontsDir);
      const offset  = val("sub-offset"); if (offset && offset !== "0") args.push("--sub-offset", offset);
      const workers = val("workers");    if (workers) args.push("--workers", workers);
      const ioJobs  = val("io-jobs");    if (ioJobs)  args.push("--io-jobs",  ioJobs);
      if (chk("dry-run"))  args.push("--dry-run");
      if (chk("no-cache")) args.push("--no-cache");
    }

    return args;
  }

  async start() {
    if (this.running) return;
    const args = this.buildArgs();
    if (!args) return;

    this.setRunning(true);
    clearLog(this.logEl);
    if (this.exitCodeEl) this.exitCodeEl.textContent = "";

    const workers = parseInt(document.getElementById(`${this.mode}-workers`)?.value || '8');
    const ioJobs  = parseInt(document.getElementById(`${this.mode}-io-jobs`)?.value || '2');
    updateWorkerChips(true, workers, ioJobs);

    const inputDir = document.getElementById(`${this.mode}-input-dir`)?.value?.trim();
    if (inputDir) {
      await scanInputDirectory(inputDir, this.mode);
    }

    if (this.unlisten) this.unlisten();
    this.unlisten = await listen("job-log", e => appendLog(this.logEl, e.payload, this.mode));

    const unlistenDone = await listen("job-done", e => {
      const code = e.payload;
      this.onDone(code);
      unlistenDone();
    });

    try {
      await invoke("run_job", { mode: this.mode, args });
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`, this.mode);
      this.setRunning(false);
      updateWorkerChips(false);
      unlistenDone();
    }
  }

  async stop() {
    try {
      await invoke("stop_job");
      appendLog(this.logEl, "[INFO] Job stopped by user.", this.mode);
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`, this.mode);
    }
    this.setRunning(false);
    updateWorkerChips(false);
  }

  async clearCache() {
    const confirmed = await showConfirmModal(
      "Clear Inspection Cache",
      `Are you sure you want to clear the persistent inspection cache for ${this.mode}?`
    );
    if (!confirmed) return;

    try {
      const msg = await invoke("clear_cache", { mode: this.mode });
      appendLog(this.logEl, `[CACHE] ${msg || "Cache cleared."}`, this.mode);
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`, this.mode);
    }
  }

  onDone(code) {
    this.setRunning(false);
    updateWorkerChips(false);
    if (this.exitCodeEl) this.exitCodeEl.textContent = `exit ${code}`;

    const isDryRun = document.getElementById(`${this.mode}-dry-run`)?.checked ?? false;
    const tracker = this.mode === "extract" ? extractQueue : remuxQueue;

    tracker.items.forEach(item => {
      if (item.status === "processing" || item.status === "queued") {
        if (isDryRun) {
          tracker.markDryRun(item.filename);
        } else {
          tracker.markCompleted(item.filename);
        }
      }
    });
  }

  setRunning(running) {
    this.running = running;
    if (this.runBtn)  this.runBtn.disabled  = running;
    if (this.stopBtn) this.stopBtn.disabled = !running;
    const el = this.statusEl;
    if (!el) return;
    el.className = "status-pill " + (running ? "status-running" : "status-idle");
    el.innerHTML = running
      ? `<span class="status-dot"></span> Running`
      : `<span class="status-dot"></span> Idle`;

    if (!running && this.exitCodeEl?.textContent.includes("exit 0")) {
      el.className = "status-pill status-success";
      el.innerHTML = `<span class="status-dot"></span> Done`;
    } else if (!running && this.exitCodeEl?.textContent && !this.exitCodeEl.textContent.includes("exit 0")) {
      el.className = "status-pill status-error";
      el.innerHTML = `<span class="status-dot"></span> Failed`;
    }
  }
}

// ─── Initialization ──────────────────────────────────────────────────────────

// The Linux window is transparent with CSS-drawn rounded corners, which must
// flatten when maximized so the desktop cannot show through the corner gaps.
async function syncMaximizedState() {
  try {
    const maximized = await invoke("is_maximized");
    document.documentElement.classList.toggle("is-maximized", !!maximized);
  } catch {
    /* command unavailable outside the Tauri shell */
  }
}

function initApp() {
  initTheme();

  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows") || ua.includes("win32")) {
    document.documentElement.classList.add("is-windows");
    document.body.classList.add("is-windows");
  } else if (ua.includes("mac")) {
    document.documentElement.classList.add("is-mac");
    document.body.classList.add("is-mac");
  } else if (ua.includes("linux")) {
    document.documentElement.classList.add("is-linux");
    document.body.classList.add("is-linux");
  }

  safeAddListener("btn-theme", "click", () => {
    const current = document.documentElement.getAttribute("data-theme");
    applyTheme(current === "dark" ? "light" : "dark");
  });

  safeAddListener("btn-close", "click", () => invoke("close_window"));
  safeAddListener("btn-min", "click", () => invoke("minimize_window"));
  safeAddListener("btn-max", "click", async () => {
    await invoke("toggle_maximize");
    syncMaximizedState();
  });

  window.addEventListener("resize", syncMaximizedState);
  syncMaximizedState();

  document.querySelectorAll("[data-tauri-drag-region]").forEach(el => {
    safeAddListener(el, "mousedown", (e) => {
      if (e.target.closest('button, .no-drag, input, a, select, textarea, .resize-handle')) {
        return;
      }
      if (e.buttons === 1) {
        invoke("start_drag");
      }
    });
  });

  document.querySelectorAll(".resize-handle").forEach(el => {
    safeAddListener(el, "mousedown", (e) => {
      if (e.buttons === 1 && el.dataset.direction) {
        invoke("start_resize", { direction: el.dataset.direction });
      }
    });
  });

  document.querySelectorAll(".pipeline-tab-btn[data-view], .nav-item[data-view], .top-tab-item[data-view]").forEach(btn => {
    safeAddListener(btn, "click", () => switchView(btn.dataset.view));
  });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    safeAddListener(btn, "click", async () => {
      const folder = await invoke("pick_folder");
      if (folder) {
        const input = document.getElementById(btn.dataset.target);
        if (input) {
          input.value = folder;
          const targetId = btn.dataset.target;
          if (targetId === "remux-input-dir" || targetId === "extract-input-dir") {
            const mode = targetId.startsWith("extract") ? "extract" : "remux";
            scanInputDirectory(folder, mode);
          }
        }
      }
    });
  });

  // ─── Active Profile Dropdown Menu Handlers ────────────────────────────────
  const profileBtn = document.getElementById("active-profile-btn");
  const profileMenu = document.getElementById("profile-dropdown-menu");

  if (profileBtn && profileMenu) {
    profileBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      profileMenu.classList.toggle("active");
    });

    document.addEventListener("click", () => {
      profileMenu.classList.remove("active");
    });

    document.querySelectorAll(".profile-option-item[data-preset]").forEach(item => {
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        const presetKey = item.dataset.preset;
        applyPreset(presetKey);
        profileMenu.classList.remove("active");
      });
    });
  }

  // Detect custom input modifications
  ["remux-bitrate", "remux-complexity", "remux-cplx-val", "remux-workers", "remux-io-jobs", "remux-sub-offset", "remux-keep-fonts", "remux-dry-run", "remux-no-cache"].forEach(id => {
    const el = document.getElementById(id);
    if (el) {
      el.addEventListener("change", () => applyPreset("custom"));
      el.addEventListener("input", () => applyPreset("custom"));
    }
  });

  new JobController("remux");
  new JobController("extract");
  startTelemetryMonitor();

  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", e => {
      try {
        if (!localStorage.getItem(THEME_KEY)) applyTheme(e.matches ? "dark" : "light");
      } catch (err) {}
    });
  }

  document.addEventListener("keydown", e => {
    if ((e.ctrlKey || e.metaKey) && e.key === "t") {
      e.preventDefault();
      const cur = document.documentElement.getAttribute("data-theme");
      applyTheme(cur === "dark" ? "light" : "dark");
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}