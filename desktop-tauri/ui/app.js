/* ─── MKV Anime Batch — Frontend App (Tokyo Glass + OLED Pro Hybrid) ───────
   Uses window.__TAURI__ (withGlobalTauri: true in tauri.conf.json)
──────────────────────────────────────────────────────────────────────────── */

"use strict";

// ─── Preset Configurations ──────────────────────────────────────────────────

const PRESETS = {
  default: {
    name: "🌸 Opus 192k + Keep Fonts",
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
    name: "⚡ Fast Muxing (128k)",
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
    name: "🎧 Hi-Fi Master (320k)",
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
    if (nameEl) nameEl.textContent = "🛠️ Custom Configuration";
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
  try {
    const fn = window.__TAURI__?.core?.invoke
      || window.__TAURI__?.tauri?.invoke
      || window.__TAURI__?.invoke
      || window.__TAURI_INTERNALS__?.invoke;

    if (fn) {
      return await fn(cmd, args);
    }
    console.log("[mock invoke]", cmd, args);
    return null;
  } catch (err) {
    console.error(`[IPC Error] ${cmd}:`, err);
    return null;
  }
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
  const btn = document.getElementById("btn-theme");
  if (btn) btn.textContent = theme === "dark" ? "🌙" : "☀️";
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch (e) {}
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

function startTelemetryMonitor(workersCount = 8, ioJobsCount = 2) {
  stopTelemetryMonitor();
  const cpuFills = document.querySelectorAll(".metric-cpu-fill, #cpu-telemetry-fill");
  const cpuVals = document.querySelectorAll(".metric-cpu-val, #cpu-telemetry-val");
  const ioFills = document.querySelectorAll(".metric-io-fill, #io-telemetry-fill");
  const ioVals = document.querySelectorAll(".metric-io-val, #io-telemetry-val");
  const ramFills = document.querySelectorAll(".metric-ram-fill, #ram-telemetry-fill");
  const ramVals = document.querySelectorAll(".metric-ram-val, #ram-telemetry-val");
  const workerChips = document.querySelectorAll(".metric-workers-chip, #workers-telemetry-chip");
  const ioJobChips = document.querySelectorAll(".metric-iojobs-chip, #iojobs-telemetry-chip");

  workerChips.forEach(chip => { chip.innerText = `⚡ ${workersCount} Workers Active`; });
  ioJobChips.forEach(chip => { chip.innerText = `🔀 ${ioJobsCount} IO Jobs Active`; });

  const updateMetrics = async () => {
    try {
      const data = await invoke("get_system_telemetry");
      if (data) {
        const cpu = Math.round(data.cpu_pct || 0);
        const ramUsed = data.ram_used_mb || 0;
        const ramTotal = data.ram_total_mb || 16384;
        const ramPct = Math.round(data.ram_pct || 0);

        cpuFills.forEach(fill => fill.style.width = `${cpu}%`);
        cpuVals.forEach(val => val.innerText = `${cpu}%`);

        ramFills.forEach(fill => fill.style.width = `${ramPct}%`);
        ramVals.forEach(val => val.innerText = `${ramUsed} MB / ${(ramTotal/1024).toFixed(0)} GB`);

        ioFills.forEach(fill => fill.style.width = `0%`);
        ioVals.forEach(val => val.innerText = `-- MB/s`);
        return;
      }
    } catch (e) {
      console.log("[Telemetry Error]", e);
    }
  };

  updateMetrics();
  telemetryTimer = setInterval(updateMetrics, 1000);
}

function stopTelemetryMonitor() {
  if (telemetryTimer) {
    clearInterval(telemetryTimer);
    telemetryTimer = null;
  }

  const cpuFills = document.querySelectorAll(".metric-cpu-fill, #cpu-telemetry-fill");
  const cpuVals = document.querySelectorAll(".metric-cpu-val, #cpu-telemetry-val");
  const ioFills = document.querySelectorAll(".metric-io-fill, #io-telemetry-fill");
  const ioVals = document.querySelectorAll(".metric-io-val, #io-telemetry-val");
  const ramFills = document.querySelectorAll(".metric-ram-fill, #ram-telemetry-fill");
  const ramVals = document.querySelectorAll(".metric-ram-val, #ram-telemetry-val");

  cpuFills.forEach(fill => fill.style.width = `0%`);
  cpuVals.forEach(val => val.innerText = `0%`);
  ioFills.forEach(fill => fill.style.width = `0%`);
  ioVals.forEach(val => val.innerText = `0 MB/s`);
  ramFills.forEach(fill => fill.style.width = `0%`);
  ramVals.forEach(val => val.innerText = `0 MB`);
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
        <div class="empty-log">
          <span class="empty-icon">📁</span>
          <span>No directory selected — browse an input folder to view episodes.</span>
        </div>`;
    }
    if (this.countEl) this.countEl.textContent = "Ready";
  }

  populateDirectoryFiles(files) {
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

    files.forEach((filename, idx) => {
      const epCard = this.createCardElement(filename, idx + 1);
      this.listEl.appendChild(epCard.el);
      this.items.set(filename, epCard);
    });
  }

  createCardElement(filename, epNum = 1) {
    const el = document.createElement("div");
    el.className = "tokyo-ep-card";
    const numStr = String(epNum).padStart(2, "0");

    let codecTag = "MKV SOURCE";
    const fnUpper = filename.toUpperCase();
    if (fnUpper.includes("1080P")) codecTag = "1080p MKV";
    else if (fnUpper.includes("2160P") || fnUpper.includes("4K")) codecTag = "2160p 4K";
    else if (fnUpper.includes("720P")) codecTag = "720p MKV";
    else if (filename.toLowerCase().endsWith(".mp4")) codecTag = "MP4 CONTAINER";

    let trackTag1 = this.mode === "extract" ? "ASS/SRT Subtitles" : "Audio Remux (Opus)";
    let trackTag2 = this.mode === "extract" ? "Font Attachments" : "Subtitle Track";

    if (fnUpper.includes("HEVC") || fnUpper.includes("H.265") || fnUpper.includes("X265")) {
      trackTag2 += " • HEVC";
    } else if (fnUpper.includes("AVC") || fnUpper.includes("H.264") || fnUpper.includes("X264")) {
      trackTag2 += " • AVC";
    } else if (fnUpper.includes("AV1")) {
      trackTag2 += " • AV1";
    }

    el.innerHTML = `
      <div class="ep-card-top">
        <div class="ep-badge">EPISODE ${numStr}</div>
        <span class="ep-codec">${codecTag}</span>
        <span class="ep-duration-tag status-tag">Queued</span>
      </div>
      <div class="ep-title">${filename}</div>
      <div class="ep-tracks">
        <span class="track-tag cyan">${trackTag1}</span>
        <span class="track-tag magenta">${trackTag2}</span>
      </div>
      <div class="ep-progress-bar">
        <div class="ep-progress-fill" style="width: 0%;"></div>
      </div>
    `;

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

    // Exact or case-insensitive match
    for (const [key, item] of this.items.entries()) {
      if (key.toLowerCase() === cleanFn.toLowerCase()) {
        return item;
      }
    }

    // Substring match
    for (const [key, item] of this.items.entries()) {
      if (key.toLowerCase().includes(cleanFn.toLowerCase()) || cleanFn.toLowerCase().includes(key.toLowerCase())) {
        return item;
      }
    }

    // Sequence order fallback: return the first item currently queued or processing
    for (const [key, item] of this.items.entries()) {
      if (item.status === "queued" || item.status === "processing") {
        return item;
      }
    }

    // Fallback: create card if empty
    const placeholder = this.listEl.querySelector(".empty-log");
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
    item.statusTag.textContent = `⏱️ Done in ${dur}s`;
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
    item.statusTag.textContent = "⚡ Planned (Dry Run)";
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
      tracker.populateDirectoryFiles(files);
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
  const placeholder = logEl.querySelector(".empty-log");
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

  // 1. Explicit [JOB_DRY_RUN]
  const dryMatch = text.match(/\[JOB_DRY_RUN\]\s*([^|]+)/i);
  if (dryMatch) {
    tracker.markDryRun(dryMatch[1].trim());
    return;
  }

  // 2. Explicit [JOB_START]
  const startMatch = text.match(/\[JOB_START\]\s*(.+)$/i);
  if (startMatch) {
    tracker.markProcessing(startMatch[1].trim());
    return;
  }

  // 3. Explicit [JOB_PROGRESS]
  const progressMatch = text.match(/\[JOB_PROGRESS\]\s*([^|]+)\|\s*(\d+)%/i);
  if (progressMatch) {
    tracker.updateProgress(progressMatch[1].trim(), parseInt(progressMatch[2]));
    return;
  }

  // 4. Explicit [JOB_DONE]
  const doneMatch = text.match(/\[JOB_DONE\]\s*([^|]+)\|(?:.*?([\d\.]+)\s*s)?/i);
  if (doneMatch) {
    const fn = doneMatch[1].trim();
    const dur = doneMatch[2] ? doneMatch[2].trim() : null;
    tracker.markCompleted(fn, dur);
    return;
  }

  // 4. Explicit [JOB_FAIL]
  const failMatch = text.match(/\[JOB_FAIL\]\s*([^|]+)\|\s*(.+)$/i);
  if (failMatch) {
    const fn = failMatch[1].trim();
    const reason = failMatch[2].trim();
    tracker.markFailed(fn, reason);
    return;
  }

  // Fallback pattern matching for standard stdout messages
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
    <div class="empty-log">
      <span class="empty-icon">📋</span>
      <span>No output stream yet — run a job to see logs here.</span>
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
    startTelemetryMonitor(workers, ioJobs);

    // Auto-scan input folder first so episode cards appear immediately in QUEUED state!
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
      stopTelemetryMonitor();
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
    stopTelemetryMonitor();
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
    stopTelemetryMonitor();
    if (this.exitCodeEl) this.exitCodeEl.textContent = `exit ${code}`;

    const tracker = this.mode === "extract" ? extractQueue : remuxQueue;
    const isSuccess = (code === 0);
    tracker.items.forEach(item => {
      if (item.status === "processing" || item.status === "queued") {
        if (isSuccess) {
          tracker.markCompleted(item.filename);
        } else {
          tracker.markFailed(item.filename, `Process exited (${code})`);
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

function initApp() {
  initTheme();

  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows") || ua.includes("win32")) document.body.classList.add("is-windows");
  else if (ua.includes("mac")) document.body.classList.add("is-mac");
  else if (ua.includes("linux")) document.body.classList.add("is-linux");

  safeAddListener("btn-theme", "click", () => {
    const current = document.documentElement.getAttribute("data-theme");
    applyTheme(current === "dark" ? "light" : "dark");
  });

  safeAddListener("btn-close", "click", () => invoke("close_window"));
  safeAddListener("btn-min", "click", () => invoke("minimize_window"));
  safeAddListener("btn-max", "click", () => invoke("toggle_maximize"));

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
