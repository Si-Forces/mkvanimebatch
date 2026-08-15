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
  document.querySelectorAll('.stepper-btn').forEach(btn => {
    const dir = btn.classList.contains('minus') ? -1 : 1;
    // Per-button timers: sharing these across all steppers lets one button's
    // mouseup kill another button's active repeat interval.
    let stepInterval = null;
    let stepTimeout = null;

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
  // Keep tab aria-selected in sync with the active view.
  document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
    btn.setAttribute("aria-selected", btn.classList.contains("active") ? "true" : "false");
  });
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
    this.batchStartTime = null; // set when a job starts; basis for per-file durations
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

  // Batch-probe all files in one command (Rust joins paths + spawns mkvmerge
  // on a thread pool), rather than one inspect_mkv_file IPC call per MKV.
  populateFromInspection(entries) {
    this.items.forEach(item => {
      if (item.timerId) clearInterval(item.timerId);
    });
    this.items.clear();
    this.completedCount = 0;
    if (this.listEl) this.listEl.innerHTML = "";

    if (!entries || entries.length === 0) {
      this.reset();
      return;
    }
    this.totalCount = entries.length;
    if (this.countEl) this.countEl.textContent = `${entries.length} Episodes Queued`;

    const bitrate = document.getElementById(`${this.mode}-bitrate`)?.value?.trim() || "192k";

    entries.forEach(([filename, meta], idx) => {
      const epCard = this.createCardElement(filename, idx + 1);
      this.listEl.appendChild(epCard.el);
      this.items.set(filename, epCard);
      if (meta) this.applyMetadata(epCard, meta, bitrate);
    });
  }

  applyMetadata(epCard, meta, bitrate) {
    if (!epCard || !epCard.el || !meta) return;
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
        invoke("inspect_mkv_file", { dirPath, filename }).then(meta => {
          if (meta) this.applyMetadata(epCard, meta, bitrate);
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

  // Strict lookup used by terminal-state markers (completed/failed/skipped).
  // Matches by exact / case-insensitive / path-normalized basename equality and
  // a conservative "one endswith the other" check — but NEVER invents a card and
  // NEVER grabs an arbitrary queued card (that caused skips to land on the
  // wrong episode).
  findItem(filename) {
    if (!filename) return null;
    const norm = s => s.trim().toLowerCase().replace(/\\/g, "/").split("/").pop();
    const target = norm(filename);

    if (this.items.has(filename.trim())) return this.items.get(filename.trim());

    for (const [key, item] of this.items.entries()) {
      if (norm(key) === target) return item;
    }
    return null;
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

  // Promote the next queued card to "processing" (indeterminate). Called when a
  // job starts and after each real completion, so the queue flows live instead
  // of dumping all updates at the end of the batch.
  promoteNext() {
    for (const item of this.items.values()) {
      if (item.status === "queued") {
        this.markProcessing(item.filename);
        return;
      }
    }
  }

  markProcessing(filename) {
    const item = this.findItem(filename) || this.getItem(filename);
    if (!item) return;
    // Idempotent: re-marking a card already processing just refreshes the timer.
    if (item.status === "completed" || item.status === "failed" || item.status === "skipped") return;

    item.status = "processing";
    item.el.className = "tokyo-ep-card processing";
    item.badge.className = "ep-badge purple";
    item.badge.textContent = "PROCESSING";
    item.statusTag.className = "ep-duration-tag yellow status-tag";
    if (!item.startTime) item.startTime = Date.now();
    item.pct = null; // no real percentage — show indeterminate shimmer
    item.fill.className = "ep-progress-fill indeterminate";
    item.fill.style.width = "";
    item.statusTag.textContent = "Muxing...";

    if (item.timerId) { clearInterval(item.timerId); item.timerId = null; }
  }

  updateProgress(filename, pct, speedStr = "") {
    const item = this.getItem(filename);
    if (!item) return;
    item.pct = Math.min(100, Math.max(0, pct));
    item.fill.classList.remove("indeterminate");
    item.fill.style.width = `${item.pct}%`;
    const elapsedSec = item.startTime ? ((Date.now() - item.startTime) / 1000).toFixed(1) : "0.0";
    const speed = speedStr ? ` • ${speedStr}` : "";
    item.statusTag.textContent = `${item.pct}% (${elapsedSec}s${speed})`;
  }

  markCompleted(filename, durationSec = null) {
    const item = this.findItem(filename) || this.getItem(filename);
    if (!item) return;
    // Don't clobber a card already in a terminal state.
    if (item.status === "completed" || item.status === "failed" || item.status === "skipped") return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "completed";
    item.pct = 100;
    item.fill.classList.remove("indeterminate");
    // Duration: prefer the real elapsed time. A card promoted to processing has
    // its own startTime; otherwise fall back to time since the batch began (we
    // can't know true per-file start — the backend never streams one).
    const dur = durationSec !== null
      ? parseFloat(durationSec).toFixed(1)
      : (item.startTime
          ? ((Date.now() - item.startTime) / 1000).toFixed(1)
          : (this.batchStartTime ? ((Date.now() - this.batchStartTime) / 1000).toFixed(1) : null));

    item.el.className = "tokyo-ep-card completed";
    item.badge.className = "ep-badge green";
    item.badge.textContent = "COMPLETED";
    item.statusTag.className = "ep-duration-tag green status-tag";
    item.statusTag.textContent = dur ? `Done in ${dur}s` : "Done";
    item.fill.className = "ep-progress-fill green";
    item.fill.style.width = "100%";

    this.completedCount++;
    if (this.countEl) {
      this.countEl.textContent = `${this.completedCount} / ${Math.max(this.totalCount, this.completedCount)} Completed`;
    }
    this.promoteNext();
  }

  markSkipped(filename, reason = "Skipped") {
    // Strict match only — never stamp "Skipped" on an arbitrary card.
    const item = this.findItem(filename);
    if (!item) return;
    if (item.status === "completed" || item.status === "failed" || item.status === "skipped") return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "skipped";
    item.pct = 100;
    item.fill.classList.remove("indeterminate");
    item.el.className = "tokyo-ep-card skipped";
    item.badge.className = "ep-badge grey";
    item.badge.textContent = "SKIPPED";
    item.statusTag.className = "ep-duration-tag grey status-tag";
    item.statusTag.textContent = reason;
    item.fill.style.width = "100%";
    this.promoteNext();
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
    item.fill.classList.remove("indeterminate");
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
    const item = this.findItem(filename) || this.getItem(filename);
    if (!item) return;
    if (item.status === "completed" || item.status === "failed" || item.status === "skipped") return;

    if (item.timerId) {
      clearInterval(item.timerId);
      item.timerId = null;
    }

    item.status = "failed";
    item.fill.classList.remove("indeterminate");
    item.el.className = "tokyo-ep-card failed";
    item.badge.className = "ep-badge red";
    item.badge.textContent = "FAILED";
    item.statusTag.className = "ep-duration-tag red status-tag";
    item.statusTag.textContent = errReason;
    this.completedCount++;
    if (this.countEl) {
      this.countEl.textContent = `${this.completedCount} / ${Math.max(this.totalCount, this.completedCount)} Completed`;
    }
    this.promoteNext();
  }
}

const remuxQueue = new QueueTracker("remux");
const extractQueue = new QueueTracker("extract");

async function scanInputDirectory(path, mode) {
  if (!path) return;
  const tracker = mode === "extract" ? extractQueue : remuxQueue;
  try {
    // Preferred: single batch command that scans + probes all MKVs in Rust.
    const entries = await invoke("inspect_directory", { path });
    if (entries && Array.isArray(entries)) {
      tracker.populateFromInspection(entries);
      return;
    }
    tracker.populateDirectoryFiles([], path);
  } catch (err) {
    // Fallback for older backends without inspect_directory: scan filenames,
    // then probe per-file (inspect_mkv_file joins dir+filename in Rust).
    console.log("[inspect_directory] unavailable, falling back:", err);
    try {
      const files = await invoke("scan_directory", { path });
      if (files && Array.isArray(files)) {
        tracker.populateDirectoryFiles(files, path);
      }
    } catch (err2) {
      console.log("[Scan Dir] Error:", err2);
    }
  }
}

// ─── Log Console & Episode Parser ──────────────────────────────────────────

// The backend logs via Python logging with format '%(asctime)s [%(levelname)s]
// %(message)s', so every stdout line actually arrives prefixed like:
//   2026-08-12 07:33:38,123 [INFO] -> [1/3 (33%)] Remuxed: file.mkv
// Stderr lines additionally get "[ERR] " prepended by the Rust sidecar reader.
// Queue-state regexes are anchored to the start of the *message*, so we must
// strip this logging envelope first or nothing ever matches.
function stripLogPrefix(text) {
  return text.replace(/^(\[ERR\]\s*)?\d{4}-\d{2}-\d{2}[ T][\d:]+[.,]\d+\s+\[\w+\]\s+/, "$1");
}

function classifyLine(text) {
  const t = stripLogPrefix(text).toLowerCase();
  if (t.includes("crashed") || t.includes("[-]") || t.includes("[err]") || t.includes("error") || t.includes("failed") || t.includes("[error]")) return "line-err";
  if (t.includes("skipped") || t.includes("skip") || t.includes("already exists") || t.includes("already complete")) return "line-skip";
  if (t.includes("[+]") || t.includes("success") || t.includes("finished") || t.includes("completed") || t.includes("remuxed:") || t.includes("extracted:") || t.includes("remuxed cleanly") || t.includes("extracted cleanly")) return "line-ok";
  if (t.includes("warn") || t.includes("[warn]")) return "line-warn";
  if (t.includes("folder:") || t.includes("dry run") || t.startsWith("processing") || t.startsWith("extracting")) return "line-info";
  if (t.includes("persistent inspection cache") || t.includes("cache cleared")) return "line-done";
  return "";
}

const MAX_LOG_LINES = 2000;
const _logQueues = new WeakMap();
const _logRafIds = new WeakMap();

function appendLog(logEl, text, mode = "remux") {
  if (!logEl) return;

  if (Array.isArray(text)) {
    for (const line of text) {
      appendLog(logEl, line, mode);
    }
    return;
  }

  parseLogStream(text, mode);

  let queue = _logQueues.get(logEl);
  if (!queue) {
    queue = [];
    _logQueues.set(logEl, queue);
  }
  queue.push(text);

  if (!_logRafIds.has(logEl)) {
    const rafId = requestAnimationFrame(() => {
      _logRafIds.delete(logEl);
      const pending = _logQueues.get(logEl);
      if (!pending || pending.length === 0) return;
      _logQueues.set(logEl, []);

      const placeholder = logEl.querySelector(".empty-state-card, .empty-log, .console-empty-prompt");
      if (placeholder) placeholder.remove();

      const frag = document.createDocumentFragment();
      for (const lineText of pending) {
        const div = document.createElement("div");
        div.className = `log-line ${classifyLine(lineText)}`;
        div.textContent = lineText;
        frag.appendChild(div);
      }
      logEl.appendChild(frag);

      while (logEl.childElementCount > MAX_LOG_LINES) {
        logEl.firstElementChild.remove();
      }

      logEl.scrollTop = logEl.scrollHeight;
    });
    _logRafIds.set(logEl, rafId);
  }
}

// Parses the *actual* stdout/stderr lines emitted by mkv_remux.py /
// mkv_extract.py. Filenames are captured as "rest of line after the colon" so
// real fansub names with spaces/brackets/dots ([Group] Show - 01 [1080p].mkv)
// match — the old [\w-]+\.mkv heuristic silently failed on those.
//
// Real backend line shapes (verified against the Python source):
//   remux   -> [N/M (P%)] Remuxed: <name>
//   remux   -> [N/M (P%)] Failed/Skipped: <name>
//   remux   SKIPPED: <name> | Reason: <r>
//   extract [N/M (P%)] Extracted: <name>
//   extract [N/M (P%)] Skipped: <name>
//   both    -> [N/M (P%)] CRASHED: <name>
//   both    Processing: <name>          (per-file deferred log block header)
//   remux     <name>  |  sub=... | chap=...   (under "--- DRY RUN: Planned jobs ---")
//   extract   <name>                        (under "--- DRY RUN: Files that would be processed ---")
function parseLogStream(text, mode) {
  const tracker = mode === "extract" ? extractQueue : remuxQueue;

  // Strip the Python logging envelope ("2026-08-12 07:33:38,123 [INFO] ") and any
  // Rust-added "[ERR] " prefix so the anchored regexes below actually match.
  text = stripLogPrefix(text);

  // Track dry-run list context across lines (module-level, reset per section).
  if (/---\s*DRY RUN:/i.test(text)) {
    parseLogStream._drySection = true;
    return;
  }
  if (parseLogStream._drySection) {
    // Dry-run sections end at the Total/summary lines.
    if (/^\s*Total:/i.test(text) || /---\s*SKIPPED FILES/i.test(text) || /^=+$/.test(text.trim())) {
      parseLogStream._drySection = false;
    } else {
      const dryName = text.replace(/^\s+/, "").split(/\s{2,}|\s*\|\s*/)[0];
      if (dryName && /\.mkv$/i.test(dryName)) {
        tracker.markDryRun(dryName.trim());
      }
      return;
    }
  }

  // Per-file deferred log block header. The backend buffers these into r.log and
  // dumps them all at the END of the batch — so acting on them would (a) re-trigger
  // processing on already-done cards and (b) cause the "all-at-once" dump. Cards are
  // already promoted to processing live via promoteNext(), so we intentionally
  // IGNORE these lines for queue-state purposes. (They still render in the console.)
  if (/^\s*Processing:\s*(.+?)\s*$/i.test(text) && /\.mkv$/i.test(text)) {
    return;
  }

  // remux: SKIPPED: <name> | Reason: <r>
  const skipMatch = text.match(/^\s*SKIPPED:\s*(.+?)\s*\|\s*Reason:\s*(.+?)\s*$/i);
  if (skipMatch) {
    tracker.markSkipped(skipMatch[1].trim(), skipMatch[2].trim());
    return;
  }

  // remux:  -> [N/M (P%)] Remuxed: <name>
  // extract:   [N/M (P%)] Extracted: <name>
  // both:   -> [N/M (P%)] CRASHED: <name>   /   Failed/Skipped: <name>
  const progMatch = text.match(/^\s*(?:\[ERR\]\s*)?(?:->\s*)?\[\s*(\d+)\s*\/\s*(\d+)\s*\(\s*([\d.]+)%\)\]\s*(Remuxed|Extracted|Failed\/Skipped|Skipped|CRASHED):\s*(.+?)\s*$/i);
  if (progMatch) {
    const verb = progMatch[4].toLowerCase();
    const name = progMatch[5].trim();
    if (verb === "remuxed" || verb === "extracted") {
      tracker.markCompleted(name);
    } else if (verb === "crashed") {
      tracker.markFailed(name, "Worker crashed");
    } else {
      // "Failed/Skipped" and "Skipped" both mean the backend produced no output.
      tracker.markSkipped(name, "Skipped");
    }
    // Note: per-card bars stay indeterminate while running and snap to 100% on
    // that card's own completion line. Batch-level N/M progress is shown in the
    // queue-count badge, so we no longer paint a batch % onto individual bars.
    return;
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
    this.lastExitCode = null; // internal only — no visible badge
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

  flagInputError(inputId, message) {
    const el = document.getElementById(`${this.mode}-${inputId}`);
    if (el) {
      el.classList.add("input-error");
      el.focus();
      const clear = () => el.classList.remove("input-error");
      el.addEventListener("input", clear, { once: true });
      el.addEventListener("change", clear, { once: true });
    }
    appendLog(this.logEl, `[ERROR] ${message}`, this.mode);
  }

  buildArgs() {
    const val = id => document.getElementById(`${this.mode}-${id}`)?.value?.trim() ?? "";
    const chk = id => document.getElementById(`${this.mode}-${id}`)?.checked ?? false;
    const args = [];

    if (this.mode === "remux") {
      const inputDir = val("input-dir");
      if (!inputDir) { this.flagInputError("input-dir", "Input directory is required."); return null; }
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
      if (!inputDir) { this.flagInputError("input-dir", "Input directory is required."); return null; }
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
    this.lastExitCode = null;

    const workers = parseInt(document.getElementById(`${this.mode}-workers`)?.value || '8');
    const ioJobs  = parseInt(document.getElementById(`${this.mode}-io-jobs`)?.value || '2');
    updateWorkerChips(true, workers, ioJobs);

    const inputDir = document.getElementById(`${this.mode}-input-dir`)?.value?.trim();
    if (inputDir) {
      await scanInputDirectory(inputDir, this.mode);
    }

    // Kick off live flow immediately: mark the first `workers` queued cards as
    // processing (indeterminate). The backend only emits per-file lines on
    // *completion* (in real time), so without this the queue would look frozen
    // until the batch dumped its deferred log block at the end.
    const tracker = this.mode === "extract" ? extractQueue : remuxQueue;
    tracker.batchStartTime = Date.now();
    for (let i = 0; i < workers; i++) tracker.promoteNext();

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
    this.lastExitCode = code;

    const isDryRun = document.getElementById(`${this.mode}-dry-run`)?.checked ?? false;
    const tracker = this.mode === "extract" ? extractQueue : remuxQueue;
    const ok = code === 0;

    // Reaching job-done means the batch ran to completion. Cards still
    // "processing" were mid-flight and finished; cards still "queued" were
    // resolved by their own skip/completion log lines already — so a leftover
    // here simply means the per-file line didn't parse. Resolve leftovers to a
    // sensible terminal state instead of leaving them stuck or falsely marking
    // them "Not processed".
    tracker.items.forEach(item => {
      if (item.status !== "processing" && item.status !== "queued") return;
      if (isDryRun) {
        tracker.markDryRun(item.filename);
      } else if (ok) {
        tracker.markCompleted(item.filename);
      } else {
        tracker.markFailed(item.filename, `Aborted (exit ${code})`);
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

    if (!running && this.lastExitCode === 0) {
      el.className = "status-pill status-success";
      el.innerHTML = `<span class="status-dot"></span> Done`;
    } else if (!running && this.lastExitCode !== null && this.lastExitCode !== 0) {
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

  // NOTE: OS-class detection (is-windows / is-mac / is-linux) lives in the
  // inline <head> script in index.html so it runs before first paint; we do
  // not duplicate it here.

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

  // ─── Low-cost accessibility wiring ──────────────────────────────────────
  document.querySelectorAll(".stepper-btn").forEach(btn => {
    if (!btn.hasAttribute("aria-label")) {
      btn.setAttribute("aria-label", btn.classList.contains("minus") ? "Decrease" : "Increase");
    }
  });
  document.querySelectorAll(".log-body").forEach(el => el.setAttribute("aria-live", "polite"));
  const modal = document.getElementById("confirm-modal");
  if (modal) { modal.setAttribute("role", "dialog"); modal.setAttribute("aria-modal", "true"); }
  const tabGroup = document.querySelector(".pipeline-tab-group");
  if (tabGroup) tabGroup.setAttribute("role", "tablist");
  const syncTabAria = () => {
    document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
      btn.setAttribute("role", "tab");
      btn.setAttribute("aria-selected", btn.classList.contains("active") ? "true" : "false");
    });
  };
  syncTabAria();

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