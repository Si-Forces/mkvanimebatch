/* ─── MKV Anime Batch — Frontend App (Tokyo Glass + OLED Pro Hybrid) ───────
   Uses window.__TAURI__ (withGlobalTauri: true in tauri.conf.json)
──────────────────────────────────────────────────────────────────────────── */

"use strict";

import { JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { classifyLine } from "./log-classify.js";
import { createTelemetryLoop } from "./telemetry.js";
import { createJobCoordinator } from "./job-coordinator.js";
import { createInspectionCoordinator } from "./inspection-coordinator.js";

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

// Global execution coordinator
const JobCoordinator = createJobCoordinator();
const inspectDirectory = createInspectionCoordinator(path => invoke("inspect_directory", { path }));


async function invoke(cmd, args = {}) {
  const fn = window.__TAURI__?.core?.invoke
    || window.__TAURI__?.tauri?.invoke
    || window.__TAURI__?.invoke
    || window.__TAURI_INTERNALS__?.invoke;

  if (fn) {
    return await fn(cmd, args);
  }
  throw new Error(`Failed to invoke ${cmd}: Tauri API is unavailable.`);
}

async function listen(event, handler) {
  const fn = window.__TAURI__?.event?.listen || window.__TAURI__?.listen;
  if (fn) {
    return await fn(event, handler);
  }
  throw new Error(`Failed to register ${event} listener: Tauri API is unavailable.`);
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

function applyTelemetryData(data) {
  const cpuFills = document.querySelectorAll(".metric-cpu-fill, #cpu-telemetry-fill");
  const cpuVals = document.querySelectorAll(".metric-cpu-val, #cpu-telemetry-val");
  const ioFills = document.querySelectorAll(".metric-io-fill, #io-telemetry-fill");
  const ioVals = document.querySelectorAll(".metric-io-val, #io-telemetry-val");
  const ramFills = document.querySelectorAll(".metric-ram-fill, #ram-telemetry-fill");
  const ramVals = document.querySelectorAll(".metric-ram-val, #ram-telemetry-val");

  const cpu = Math.round(data.cpu_pct || 0);
  const ramUsed = data.ram_used_mb || 0;
  const ramTotal = data.ram_total_mb || 16384;
  const ramPct = Math.round(data.ram_pct || 0);
  // The backend currently exposes CPU/RAM only; keep an unmeasured disk
  // metric visibly neutral instead of presenting a hard-coded zero.
  const ioMb = typeof data.io_mb_s === "number" && data.io_mb_s > 0
    ? data.io_mb_s
    : null;

  const ioTooltip = ioMb === null
    ? "System-wide disk I/O is not measured; batch disk concurrency is limited by the Disk I/O Jobs (--io-jobs) setting"
    : `${ioMb.toFixed(1)} MB/s`;

  const setWidth = (fill, width) => {
    if (fill.style.width !== width) fill.style.width = width;
  };
  const setText = (el, text) => {
    if (el.textContent !== text) el.textContent = text;
  };

  cpuFills.forEach(fill => setWidth(fill, `${cpu}%`));
  cpuVals.forEach(el => setText(el, `${cpu}%`));

  ramFills.forEach(fill => setWidth(fill, `${ramPct}%`));
  ramVals.forEach(el => setText(el, `${ramUsed} MB / ${(ramTotal/1024).toFixed(0)} GB`));

  const ioWidth = ioMb === null ? "0%" : `${Math.min(100, (ioMb / 300) * 100)}%`;
  const ioText = ioMb === null ? "Not measured" : `${ioMb.toFixed(1)} MB/s`;

  ioFills.forEach(fill => {
    setWidth(fill, ioWidth);
    if (fill.classList.contains("unavailable") !== (ioMb === null)) {
      fill.classList.toggle("unavailable", ioMb === null);
    }
    if (fill.title !== ioTooltip) fill.title = ioTooltip;
  });
  ioVals.forEach(val => {
    setText(val, ioText);
    if (val.classList.contains("muted") !== (ioMb === null)) {
      val.classList.toggle("muted", ioMb === null);
    }
    if (val.title !== ioTooltip) val.title = ioTooltip;
  });
}

const telemetryLoop = createTelemetryLoop({
  invoke,
  onData: applyTelemetryData,
  onError: (e) => console.log("[Telemetry Error]", e),
  doc: document,
});


const remuxQueue = new QueueTracker("remux");
const extractQueue = new QueueTracker("extract");

function queueFor(mode) {
  return mode === "extract" ? extractQueue : remuxQueue;
}

// ─── GUI Manual Subtitle Selection ──────────────────────────────────────────

let subAssignModalItemId = null;

function openSubAssignModal(itemId) {
  const tracker = remuxQueue;
  const item = tracker.state?.items.get(itemId);
  const candidates = tracker.candidatesByItem.get(itemId) || [];
  if (!item) return;

  subAssignModalItemId = itemId;
  const titleEl = document.getElementById("sub-assign-episode");
  if (titleEl) titleEl.textContent = item.name;

  renderSubAssignList(candidates, tracker.assignments.get(itemId) || null);

  const modal = document.getElementById("sub-assign-modal");
  if (modal) modal.classList.add("active");
}

function renderSubAssignList(candidates, assignedPath) {
  const listEl = document.getElementById("sub-assign-list");
  if (!listEl) return;
  listEl.innerHTML = "";

  if (candidates.length === 0) {
    listEl.innerHTML = `<div class="sub-assign-empty">No external subtitle files were found near this episode.</div>`;
    return;
  }

  for (const candidate of candidates) {
    const base = candidate.split("/").pop();
    const dirHint = candidate.includes("/")
      ? candidate.slice(0, candidate.lastIndexOf("/"))
      : "";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sub-assign-option" + (candidate === assignedPath ? " selected" : "");
    btn.dataset.path = candidate;
    const nameSpan = document.createElement("span");
    nameSpan.className = "sub-assign-name";
    nameSpan.textContent = base;
    const dirSpan = document.createElement("span");
    dirSpan.className = "sub-assign-dir";
    dirSpan.textContent = dirHint || "(input root)";
    btn.append(nameSpan, dirSpan);
    listEl.appendChild(btn);
  }
}

function closeSubAssignModal() {
  subAssignModalItemId = null;
  const modal = document.getElementById("sub-assign-modal");
  if (modal) modal.classList.remove("active");
}

async function scanInputDirectory(path, mode, { force = false } = {}) {
  const tracker = queueFor(mode);
  let scanToken = null;
  path = path.trim();
  if (!path) {
    tracker.reset();
    tracker.lastScannedPath = null;
    JobCoordinator.syncControls();
    return false;
  }
  try {
    if (!force && tracker.reuseInspection(path)) return true;

    scanToken = tracker.startScan();
    const manifest = await inspectDirectory(path, { force });
    return tracker.acceptInspection(scanToken, path, manifest);
  } catch (err) {
    console.log("[inspect_directory] failed:", err);
    if (scanToken !== null) tracker.rejectInspection(scanToken);
    return false;
  } finally {
    JobCoordinator.syncControls();
  }
}

async function chooseInputDirectory(targetId) {
  if (JobCoordinator.activeMode !== null) return;
  const directory = await invoke("pick_folder");
  if (!directory || JobCoordinator.activeMode !== null) return;

  const input = document.getElementById(targetId);
  if (!input) return;
  input.value = directory;

  if (targetId === "remux-input-dir" || targetId === "extract-input-dir") {
    const mode = targetId.startsWith("extract") ? "extract" : "remux";
    await scanInputDirectory(directory, mode);
  }
}

// ─── Log Console ────────────────────────────────────────────────────────────

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

let modalChain = Promise.resolve();

function showModal(title, message, { confirmLabel = "Confirm", showCancel = true } = {}) {
  const run = () =>
    new Promise(resolve => {
      const modal = document.getElementById("confirm-modal");
      const titleEl = document.getElementById("modal-title");
      const descEl = document.getElementById("modal-desc");
      const cancelBtn = document.getElementById("modal-cancel");
      const confirmBtn = document.getElementById("modal-confirm");

      if (!modal || !cancelBtn || !confirmBtn) {
        if (!showCancel) {
          alert(message);
          resolve(true);
        } else {
          resolve(confirm(message));
        }
        return;
      }

      if (titleEl) titleEl.textContent = title;
      if (descEl) descEl.textContent = message;
      confirmBtn.textContent = confirmLabel;
      cancelBtn.textContent = "Cancel";
      cancelBtn.hidden = !showCancel;

      modal.classList.add("active");

      const cleanup = (result) => {
        modal.classList.remove("active");
        cancelBtn.hidden = false;
        cancelBtn.removeEventListener("click", onCancel);
        confirmBtn.removeEventListener("click", onConfirm);
        resolve(result);
      };

      const onCancel = () => cleanup(false);
      const onConfirm = () => cleanup(true);

      cancelBtn.addEventListener("click", onCancel);
      confirmBtn.addEventListener("click", onConfirm);
    });

  const nextPromise = modalChain.then(run, run);
  modalChain = nextPromise.catch(() => false);
  return nextPromise;
}

function showCleanupError(error) {
  console.error("[cleanup] failed:", error);
  return showModal(
    "Could Not Close Safely",
    `MKV Anime Batch is still open because process cleanup failed: ${error}`,
    { confirmLabel: "OK", showCancel: false }
  );

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

  safeAddListener("btn-close", "click", () => invoke("close_window").catch(showCleanupError));
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
  listen("cleanup-error", event => showCleanupError(event.payload)).catch(error => {
    console.error("[cleanup-error] listener failed:", error);
  });
  const tabGroup = document.querySelector(".pipeline-tab-group");
  if (tabGroup) tabGroup.setAttribute("role", "tablist");
  syncTabAria({ includeRole: true });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    safeAddListener(btn, "click", () => chooseInputDirectory(btn.dataset.target));
  });
  document.addEventListener("click", e => {
    const emptyAction = e.target.closest(".empty-state-action[data-target]");
    if (emptyAction) chooseInputDirectory(emptyAction.dataset.target);
  });

  // ─── GUI Manual Subtitle Selection ────────────────────────────────────────
  const remuxList = document.getElementById("remux-episode-list");
  if (remuxList) {
    remuxList.addEventListener("click", e => {
      const btn = e.target.closest(".sub-assign-btn");
      if (btn?.dataset.itemId) openSubAssignModal(btn.dataset.itemId);
    });
  }
  safeAddListener("sub-assign-close", "click", closeSubAssignModal);
  safeAddListener("sub-assign-clear", "click", () => {
    if (JobCoordinator.activeMode !== null) return;
    if (subAssignModalItemId) {
      remuxQueue.assignments.delete(subAssignModalItemId);
      remuxQueue.applyAssignmentToCard(subAssignModalItemId);
    }
    closeSubAssignModal();
  });
  safeAddListener("sub-assign-modal", "click", e => {
    if (e.target === e.currentTarget) closeSubAssignModal();
  });
  safeAddListener("sub-assign-list", "click", e => {
    if (JobCoordinator.activeMode !== null) return;
    const option = e.target.closest(".sub-assign-option");
    if (!option?.dataset.path || !subAssignModalItemId) return;
    remuxQueue.assignments.set(subAssignModalItemId, option.dataset.path);
    remuxQueue.applyAssignmentToCard(subAssignModalItemId);
    closeSubAssignModal();
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

  const jobRuntime = {
    coordinator: JobCoordinator,
    appendLog,
    clearLog,
    closeSubAssignModal,
    invoke,
    listen,
    queueFor,
    safeAddListener,
    scanInputDirectory,
    showModal,
    telemetryLoop,
    updateWorkerChips,
  };
  new JobController("remux", jobRuntime);
  new JobController("extract", jobRuntime);
  JobCoordinator.syncControls();
  telemetryLoop.start();

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
