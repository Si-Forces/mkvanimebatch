/* ─── MKV Anime Batch — Frontend App (Tokyo Glass + OLED Pro Hybrid) ───────
   Uses window.__TAURI__ (withGlobalTauri: true in tauri.conf.json)
──────────────────────────────────────────────────────────────────────────── */

"use strict";

import { createQueueState, reduceQueueEvent, finalizeQueueState } from "./queue-state.js";

// ─── Preset Configurations ──────────────────────────────────────────────────

const PRESETS = {
  default: {
    name: "Opus 192k + Keep Fonts",
    bitrate: "192k",
    complexity: "10",
    workers: "8",
    ioJobs: "2",
    ffmpegThreads: "",
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
    ffmpegThreads: "",
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
    ffmpegThreads: "",
    subOffset: "0",
    keepFonts: true,
    dryRun: false,
    noCache: false,
  }
};

function setActiveProfileOption(presetKey) {
  document.querySelectorAll(".profile-option-item").forEach(item => {
    const active = item.dataset.preset === presetKey;
    item.classList.toggle("active", active);
    item.setAttribute("aria-pressed", active ? "true" : "false");
  });
}

function applyPreset(presetKey) {
  const p = PRESETS[presetKey];
  const nameEl = document.getElementById("active-profile-name");

  if (!p) {
    if (nameEl) nameEl.textContent = "Custom Configuration";
    setActiveProfileOption("custom");
    return;
  }

  if (nameEl) nameEl.textContent = p.name;
  setActiveProfileOption(presetKey);

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
  setVal("remux-ffmpeg-threads", p.ffmpegThreads || "");
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
function stepNumber(btn, dir) {
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
}

function initSteppers() {
  document.querySelectorAll('.stepper-btn').forEach(btn => {
    const dir = btn.classList.contains('minus') ? -1 : 1;
    let stepInterval = null;
    let stepTimeout = null;

    const startStepping = () => {
      stepNumber(btn, dir);
      stepTimeout = setTimeout(() => {
        stepInterval = setInterval(() => stepNumber(btn, dir), 80);
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
}

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

function syncTabAria({ includeRole = false } = {}) {
  document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
    if (includeRole) btn.setAttribute("role", "tab");
    btn.setAttribute("aria-selected", btn.classList.contains("active") ? "true" : "false");
  });
}

function toggleTheme() {
  const current = document.documentElement.getAttribute("data-theme");
  applyTheme(current === "dark" ? "light" : "dark");
}

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
  syncTabAria();

  // Toggle titlebar context controls between Remux Profiles and Extract Mode Badge
  const profileDropdown = document.getElementById("profile-dropdown-wrapper");
  const extractBadge = document.getElementById("extract-header-badge");
  if (profileDropdown) {
    profileDropdown.style.display = viewId === "extract" ? "none" : "";
  }
  if (extractBadge) {
    extractBadge.style.display = viewId === "extract" ? "inline-flex" : "none";
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
        // The backend currently exposes CPU/RAM only; keep an unmeasured disk
        // metric visibly neutral instead of presenting a hard-coded zero.
        const ioMb = typeof data.io_mb_s === "number" && data.io_mb_s > 0
          ? data.io_mb_s
          : null;

        cpuFills.forEach(fill => fill.style.width = `${cpu}%`);
        cpuVals.forEach(val => val.innerText = `${cpu}%`);

        ramFills.forEach(fill => fill.style.width = `${ramPct}%`);
        ramVals.forEach(val => val.innerText = `${ramUsed} MB / ${(ramTotal/1024).toFixed(0)} GB`);

        const ioTooltip = ioMb === null
          ? "System-wide disk I/O is not measured; batch disk concurrency is limited by the Disk I/O Jobs (--io-jobs) setting"
          : `${ioMb.toFixed(1)} MB/s`;

        ioFills.forEach(fill => {
          fill.classList.toggle("unavailable", ioMb === null);
          fill.style.width = ioMb === null ? "0%" : `${Math.min(100, (ioMb / 300) * 100)}%`;
          fill.title = ioTooltip;
        });
        ioVals.forEach(val => {
          val.classList.toggle("muted", ioMb === null);
          val.innerText = ioMb === null ? "Not measured" : `${ioMb.toFixed(1)} MB/s`;
          val.title = ioTooltip;
        });
      }
    } catch (e) {
      console.log("[Telemetry Error]", e);
    }
  };

  updateMetrics();
  telemetryTimer = setInterval(updateMetrics, 1000);

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      clearInterval(telemetryTimer);
      telemetryTimer = null;
    } else if (!telemetryTimer) {
      updateMetrics();
      telemetryTimer = setInterval(updateMetrics, 1000);
    }
  });
}

// ─── Real-Time Episode Queue & Progress/Duration Tracker ───────────────────

class QueueTracker {
  constructor(mode) {
    this.mode = mode;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.state = createQueueState({ version: 1, items: [] }, { mode });
    this.cards = new Map();
    this.scanToken = 0;
  }

  emptyStateMarkup(title, description, actionLabel = "📁 Choose Input Folder") {
    return `
      <div class="empty-state-card">
        <div class="empty-icon-glow">
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M22 19a2 2 0 0 1-2-2V8a2 2 0 0 0-2-2h-7l-2-3H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2z"></path>
            <line x1="12" y1="10" x2="12" y2="16"></line>
            <line x1="9" y1="13" x2="15" y2="13"></line>
          </svg>
        </div>
        <div class="empty-state-title">${title}</div>
        <div class="empty-state-desc">${description}</div>
        <button class="btn btn-gradient empty-state-action" type="button" data-target="${this.mode}-input-dir">${actionLabel}</button>
      </div>`;
  }

  startScan() {
    const token = ++this.scanToken;
    this.cards.clear();
    this.state = createQueueState({ version: 1, items: [] }, { mode: this.mode });
    if (this.listEl) {
      this.listEl.innerHTML = `
        <div class="empty-state-card scanning-state">
          <div class="scan-spinner" aria-hidden="true"></div>
          <div class="empty-state-title">Scanning Folder</div>
          <div class="empty-state-desc">Inspecting MKV files and reading track metadata…</div>
        </div>`;
    }
    if (this.countEl) this.countEl.textContent = "Scanning…";
    return token;
  }

  isCurrentScan(token) {
    return token === this.scanToken;
  }

  showScanError(token) {
    if (!this.isCurrentScan(token)) return;
    if (this.listEl) {
      this.listEl.innerHTML = this.emptyStateMarkup(
        "Scan failed",
        "That folder could not be inspected. Choose another folder and try again.",
        "↻ Choose Another Folder"
      );
    }
    if (this.countEl) this.countEl.textContent = "Scan failed";
  }

  reset(title = "No Episodes Queued", description = "Choose an input folder to scan MKV episodes into this batch.") {
    this.scanToken++;
    this.cards.clear();
    this.state = createQueueState({ version: 1, items: [] }, { mode: this.mode });
    if (this.listEl) this.listEl.innerHTML = this.emptyStateMarkup(title, description);
    if (this.countEl) this.countEl.textContent = "Ready";
  }

  populateFromInspection(manifest) {
    this.cards.clear();
    if (this.listEl) this.listEl.innerHTML = "";

    const bitrate = document.getElementById(`${this.mode}-bitrate`)?.value?.trim() || "192k";
    this.state = createQueueState(manifest, { mode: this.mode });

    if (!manifest || !manifest.items || manifest.items.length === 0) {
      this.reset(
        "No MKV files found",
        "This folder does not contain any MKV episodes. Choose another input folder to continue."
      );
      return;
    }

    if (this.countEl) this.countEl.textContent = `${this.state.totalCount} Episodes Queued`;

    if (this.state) {
      let epIdx = 1;
      for (const id of this.state.order) {
        const item = this.state.items.get(id);
        const card = this.createCardElement(item.id, item.name, epIdx++);
        this.listEl.appendChild(card.el);
        this.cards.set(id, card);
        if (item.metadata) {
          this.applyMetadata(card, item.metadata, bitrate);
        }
      }
    }
  }

  applyMetadata(card, meta, bitrate) {
    if (!card || !card.el || !meta) return;
    if (card.codec) card.codec.textContent = meta.video_tag || "MKV";

    if (this.mode === "remux") {
      if (card.tag1) {
        card.tag1.textContent = meta.has_flac
          ? `${meta.audio_summary || "Audio"} → Opus ${bitrate}`
          : (meta.audio_summary ? `${meta.audio_summary} (Passthrough)` : "No Audio");
      }
      if (card.tag2) {
        card.tag2.textContent = meta.fonts_count > 0
          ? `${meta.sub_summary || "No Subs"} + ${meta.fonts_count} Fonts`
          : (meta.sub_summary || "No Subs");
      }
    } else {
      if (card.tag1) card.tag1.textContent = meta.sub_summary || "No Subs";
      if (card.tag2) card.tag2.textContent = `${meta.fonts_count || 0} Attached Fonts`;
    }
  }

  createCardElement(id, name, epNum = 1) {
    const el = document.createElement("div");
    el.className = "tokyo-ep-card";
    const numStr = String(epNum).padStart(2, "0");

    const codecTag = "MKV";
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
    if (titleEl) titleEl.textContent = name;

    return {
      el,
      id,
      name,
      statusTag: el.querySelector(".status-tag"),
      badge: el.querySelector(".ep-badge"),
      fill: el.querySelector(".ep-progress-fill"),
      codec: el.querySelector(".ep-codec"),
      tag1: el.querySelector(".track-tag.cyan"),
      tag2: el.querySelector(".track-tag.magenta"),
    };
  }

  handleEvent(event) {
    if (!this.state) return;
    this.state = reduceQueueEvent(this.state, event);

    if (event.id) {
      const item = this.state.items.get(event.id);
      const card = this.cards.get(event.id);
      if (item && card) {
        this.renderCardState(card, item);
      }
    }

    this.renderQueueCount();
  }

  renderCardState(card, item) {
    if (!card || !card.el || !item) return;

    const styles = {
      processing: {
        el: "tokyo-ep-card processing",
        badge: "ep-badge purple",
        badgeText: "PROCESSING",
        tag: "ep-duration-tag yellow status-tag",
        tagText: "Processing...",
        fill: "ep-progress-fill indeterminate",
        width: "",
      },
      completed: {
        el: "tokyo-ep-card completed",
        badge: "ep-badge green",
        badgeText: "COMPLETED",
        tag: "ep-duration-tag green status-tag",
        tagText: item.durationSec ? `Done in ${item.durationSec}s` : "Done",
        fill: "ep-progress-fill green",
        width: "100%",
      },
      skipped: {
        el: "tokyo-ep-card skipped",
        badge: "ep-badge grey",
        badgeText: "SKIPPED",
        tag: "ep-duration-tag grey status-tag",
        tagText: item.reason || "Skipped",
        fill: "ep-progress-fill",
        width: "100%",
      },
      planned: {
        el: "tokyo-ep-card dry-run",
        badge: "ep-badge cyan",
        badgeText: "DRY RUN",
        tag: "ep-duration-tag cyan status-tag",
        tagText: "Planned (Dry Run)",
        fill: "ep-progress-fill cyan",
        width: "100%",
      },
      error: {
        el: "tokyo-ep-card failed",
        badge: "ep-badge red",
        badgeText: "FAILED",
        tag: "ep-duration-tag red status-tag",
        tagText: item.error || "Failed",
        fill: "ep-progress-fill",
        width: "100%",
      },
    };
    const style = styles[item.status];
    if (!style) return;

    card.el.className = style.el;
    card.badge.className = style.badge;
    card.badge.textContent = style.badgeText;
    card.statusTag.className = style.tag;
    card.statusTag.textContent = style.tagText;
    card.fill.className = style.fill;
    card.fill.style.width = style.width;
  }

  finalize(exitCode) {
    if (!this.state) return;
    this.state = finalizeQueueState(this.state, exitCode);

    for (const [id, item] of this.state.items.entries()) {
      const card = this.cards.get(id);
      if (card) {
        this.renderCardState(card, item);
      }
    }

    this.renderQueueCount();
  }

  renderQueueCount() {
    if (!this.countEl || !this.state) return;
    const { totalCount, completedCount, skippedCount, errorCount, batchCompleted } = this.state;
    const processed = completedCount + skippedCount + errorCount;

    if (batchCompleted) {
      const parts = [`${completedCount} Completed`];
      if (skippedCount > 0) parts.push(`${skippedCount} Skipped`);
      if (errorCount > 0) parts.push(`${errorCount} Failed`);
      this.countEl.textContent = `${parts.join(", ")} (${totalCount} Total)`;
    } else {
      this.countEl.textContent = `${processed} / ${totalCount} Processed`;
    }
  }
}

const remuxQueue = new QueueTracker("remux");
const extractQueue = new QueueTracker("extract");

function queueFor(mode) {
  return mode === "extract" ? extractQueue : remuxQueue;
}

async function scanInputDirectory(path, mode) {
  const tracker = queueFor(mode);
  if (!path) {
    tracker.reset();
    return;
  }

  const scanToken = tracker.startScan();
  try {
    const manifest = await invoke("inspect_directory", { path });
    if (!tracker.isCurrentScan(scanToken)) return;
    if (manifest && manifest.version === 1 && Array.isArray(manifest.items)) {
      tracker.populateFromInspection(manifest);
    } else {
      tracker.showScanError(scanToken);
    }
  } catch (err) {
    console.log("[inspect_directory] failed:", err);
    tracker.showScanError(scanToken);
  }
}

async function chooseInputFolder(targetId) {
  const folder = await invoke("pick_folder");
  if (!folder) return;

  const input = document.getElementById(targetId);
  if (!input) return;
  input.value = folder;

  if (targetId === "remux-input-dir" || targetId === "extract-input-dir") {
    const mode = targetId.startsWith("extract") ? "extract" : "remux";
    await scanInputDirectory(folder, mode);
  }
}

// ─── Log Console ────────────────────────────────────────────────────────────

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

function appendLog(logEl, text) {
  if (!logEl) return;

  if (Array.isArray(text)) {
    for (const line of text) {
      appendLog(logEl, line);
    }
    return;
  }

  if (typeof text === "string" && text.startsWith("@event:")) {
    return;
  }

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
      const shouldFollow = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 32;
      logEl.appendChild(frag);

      while (logEl.childElementCount > MAX_LOG_LINES) {
        logEl.firstElementChild.remove();
      }

      if (shouldFollow) logEl.scrollTop = logEl.scrollHeight;
    });
    _logRafIds.set(logEl, rafId);
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
    this.unlistenLog = null;
    this.unlistenEvent = null;
    this.unlistenDone = null;

    this.logEl     = document.getElementById(`${mode}-log`);
    this.statusEl  = document.getElementById(`${mode}-status`);
    this.lastExitCode = null;
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
    appendLog(this.logEl, `[ERROR] ${message}`);
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

    const tracker = queueFor(this.mode);

    this.cleanupListeners();

    this.unlistenLog = await listen("job-log", e => appendLog(this.logEl, e.payload));
    this.unlistenEvent = await listen("job-event", e => {
      if (!e.payload?.mode || e.payload.mode === this.mode) {
        tracker.handleEvent(e.payload);
      }
    });

    this.unlistenDone = await listen("job-done", e => {
      const payload = e.payload;
      if (typeof payload === "object" && payload?.mode && payload.mode !== this.mode) return;
      this.onDone(typeof payload === "object" ? payload.code : payload);
      this.cleanupListeners();
    });

    try {
      await invoke("run_job", { mode: this.mode, args });
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
      tracker.finalize(-1);
      this.lastExitCode = -1;
      this.setRunning(false);
      updateWorkerChips(false);
      this.cleanupListeners();
    }
  }

  async stop() {
    try {
      await invoke("stop_job");
      appendLog(this.logEl, "[INFO] Job stopped by user.");
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
    }
  }

  async clearCache() {
    const confirmed = await showConfirmModal(
      "Clear Inspection Cache",
      `Are you sure you want to clear the persistent inspection cache for ${this.mode}?`
    );
    if (!confirmed) return;

    try {
      const msg = await invoke("clear_cache", { mode: this.mode });
      appendLog(this.logEl, `[CACHE] ${msg || "Cache cleared."}`);
    } catch (err) {
      appendLog(this.logEl, `[ERROR] ${err}`);
    }
  }

  onDone(code) {
    const tracker = queueFor(this.mode);
    tracker.finalize(code);
    this.lastExitCode = tracker.state?.batchSuccess ? code : -1;
    this.setRunning(false);
    updateWorkerChips(false);
  }

  setRunning(running) {
    this.running = running;
    if (this.runBtn)  this.runBtn.disabled  = running;
    if (this.stopBtn) this.stopBtn.disabled = !running;
    const el = this.statusEl;
    if (!el) return;
    let statusClass = "status-idle";
    let statusLabel = "Idle";
    if (running) {
      statusClass = "status-running";
      statusLabel = "Running";
    } else if (this.lastExitCode === 0) {
      statusClass = "status-success";
      statusLabel = "Done";
    } else if (this.lastExitCode !== null) {
      statusClass = "status-error";
      statusLabel = "Failed";
    }
    el.className = "status-pill " + statusClass;
    el.innerHTML = `<span class="status-dot"></span> ${statusLabel}`;
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

  safeAddListener("btn-theme", "click", toggleTheme);
  initSteppers();

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
  syncTabAria({ includeRole: true });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    safeAddListener(btn, "click", () => chooseInputFolder(btn.dataset.target));
  });
  document.addEventListener("click", e => {
    const emptyAction = e.target.closest(".empty-state-action[data-target]");
    if (emptyAction) chooseInputFolder(emptyAction.dataset.target);
  });

  // ─── Active Profile Dropdown Menu Handlers ────────────────────────────────
  const profileBtn = document.getElementById("active-profile-btn");
  const profileMenu = document.getElementById("profile-dropdown-menu");

  if (profileBtn && profileMenu) {
    const setProfileMenuOpen = open => {
      profileMenu.classList.toggle("active", open);
      profileBtn.setAttribute("aria-expanded", open ? "true" : "false");
    };

    profileBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      setProfileMenuOpen(!profileMenu.classList.contains("active"));
    });

    profileBtn.addEventListener("keydown", e => {
      if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        setProfileMenuOpen(true);
        profileMenu.querySelector(".profile-option-item")?.focus();
      } else if (e.key === "Escape") {
        setProfileMenuOpen(false);
      }
    });

    document.addEventListener("click", () => setProfileMenuOpen(false));
    profileMenu.addEventListener("keydown", e => {
      if (e.key === "Escape") {
        e.preventDefault();
        setProfileMenuOpen(false);
        profileBtn.focus();
      }
    });

    document.querySelectorAll(".profile-option-item[data-preset]").forEach(item => {
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        applyPreset(item.dataset.preset);
        setProfileMenuOpen(false);
        profileBtn.focus();
      });
    });
  }

  // ─── Complexity Slider & Stepper Synchronization ────────────────────────
  const cplxSlider = document.getElementById("remux-complexity");
  const cplxNumber = document.getElementById("remux-cplx-val");
  if (cplxSlider && cplxNumber) {
    const syncFromSlider = () => {
      cplxNumber.value = cplxSlider.value;
    };
    const syncFromNumber = () => {
      let v = parseInt(cplxNumber.value, 10);
      if (!isNaN(v)) {
        if (v < 0) v = 0;
        if (v > 10) v = 10;
        cplxSlider.value = String(v);
      }
    };
    const normalizeNumber = () => {
      let v = parseInt(cplxNumber.value, 10);
      if (isNaN(v) || v < 0) v = 0;
      else if (v > 10) v = 10;
      cplxNumber.value = String(v);
      cplxSlider.value = String(v);
    };

    cplxSlider.addEventListener("input", syncFromSlider);
    cplxSlider.addEventListener("change", syncFromSlider);
    cplxNumber.addEventListener("input", syncFromNumber);
    cplxNumber.addEventListener("change", normalizeNumber);
    cplxNumber.addEventListener("blur", normalizeNumber);
  }

  // Detect custom input modifications
  ["remux-bitrate", "remux-complexity", "remux-cplx-val", "remux-workers", "remux-io-jobs", "remux-ffmpeg-threads", "remux-sub-offset", "remux-keep-fonts", "remux-dry-run", "remux-no-cache"].forEach(id => {
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
      toggleTheme();
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}
