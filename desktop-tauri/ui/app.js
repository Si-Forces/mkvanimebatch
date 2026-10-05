/* MKV Anime Batch desktop frontend.
   Talks to the Tauri host through window.__TAURI__ (withGlobalTauri: true in tauri.conf.json). */

import { BULK_SELECTION_CONTROLS, CLEANUP_ERROR, JOB_DONE, JOB_EVENT, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { classifyLine, pushCapped } from "./log-classify.js";
import { createTelemetryLoop } from "./telemetry.js";
import { createJobCoordinator } from "./job-coordinator.js";
import { createInspectionCoordinator } from "./inspection-coordinator.js";
import { nextStepValue } from "./stepper.js";

// ─── Preset Configurations ──────────────────────────────────────────────────

// Fallback catalog; `get_preset_profiles` overwrites it at startup.
const PRESETS = {
  balanced: {
    icon: "🌸",
    name: "Balanced Anime (Opus 192k)",
    bitrate: "192k",
    complexity: "5",
  },
  fast: {
    icon: "⚡",
    name: "Fast Muxing (128k)",
    bitrate: "128k",
    complexity: "5",
  },
  hifi: {
    icon: "🎧",
    name: "Hi-Fi Master (320k)",
    bitrate: "320k",
    complexity: "10",
  },
};

async function restoreQueueFromSnapshot(controllers) {
  const buffered = [];
  let active = null;
  let unlistenEvent = null;
  let unlistenDone = null;
  jobCoordinator.isRestoring = true;
  jobCoordinator.syncControls();
  const deliver = (type, payload) => {
    if (active) active.handleSnapshotSignal(type, payload);
    else buffered.push([type, payload]);
  };

  try {
    unlistenEvent = await listen(JOB_EVENT, e => deliver("event", e.payload));
    unlistenDone = await listen(JOB_DONE, e => deliver("done", e.payload));
    const snapshot = await invoke("get_job_snapshot");
    if (!snapshot || snapshot.job_id == null || !snapshot.items || !controllers[snapshot.mode]) return;
    const queue = queueFor(snapshot.mode);
    if (snapshot.status !== "running" && queue.state.items.size > 0) return;

    queue.restoreFromSnapshot(snapshot);
    if (snapshot.mode === "extract") switchView("extract");
    if (snapshot.status !== "running") {
      controllers[snapshot.mode].restoreTerminalSnapshot(snapshot);
      return;
    }

    active = controllers[snapshot.mode];
    active.restoreActiveSnapshot(snapshot, unlistenEvent, unlistenDone);
    unlistenEvent = null;
    unlistenDone = null;
    for (const [type, payload] of buffered) {
      active.handleSnapshotSignal(type, payload, true);
    }
  } catch (err) {
    console.error("[get_job_snapshot] restore failed:", err);
  } finally {
    unlistenEvent?.();
    unlistenDone?.();
    if (jobCoordinator.isRestoring) {
      jobCoordinator.isRestoring = false;
      jobCoordinator.syncControls();
    }
  }
}

async function loadPresetProfiles() {
  try {
    const profiles = await invoke("get_preset_profiles");
    for (const p of profiles ?? []) {
      PRESETS[p.id] = {
        icon: p.icon,
        name: p.name,
        bitrate: p.bitrate,
        complexity: String(p.complexity),
      };
    }
  } catch (err) {
    console.error("[get_preset_profiles] failed:", err);
  }
}

/**
 * The engine's default worker count: `null` until `get_default_workers` answers, or when it
 * fails. An empty workers field sends `workers: null`, which the engine resolves to this.
 */
let defaultWorkers = null;

const WORKERS_INPUT_IDS = ["remux-workers", "extract-workers"];

async function loadDefaultWorkers() {
  try {
    const workers = await invoke("get_default_workers");
    if (!Number.isInteger(workers) || workers < 1) {
      throw new Error(`unexpected value ${JSON.stringify(workers)}`);
    }
    defaultWorkers = workers;
    for (const id of WORKERS_INPUT_IDS) {
      const input = document.getElementById(id);
      if (!input) continue;
      input.placeholder = `auto (${workers})`;
      input.dataset.autoValue = String(workers);
    }
    // Leave the chips alone while a job owns them; they show that job's own count.
    if (jobCoordinator.activeMode === null) updateWorkerChips(false);
  } catch (err) {
    // The placeholder stays "auto" and the chips keep falling back to 8.
    console.error("[get_default_workers] failed:", err);
  }
}

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
  const profileBtn = document.getElementById("active-profile-btn");

  if (!p || presetKey === "custom") {
    if (nameEl) nameEl.textContent = "🛠️ Custom Configuration";
    if (profileBtn) profileBtn.dataset.preset = "custom";
    setActiveProfileOption("custom");
    return;
  }

  if (nameEl) nameEl.textContent = `${p.icon} ${p.name}`;
  if (profileBtn) profileBtn.dataset.preset = presetKey;
  setActiveProfileOption(presetKey);

  const setVal = (id, val) => {
    const el = document.getElementById(id);
    if (el) el.value = val;
  };

  if (p.bitrate !== undefined) setVal("remux-bitrate", p.bitrate);
  if (p.complexity !== undefined) {
    setVal("remux-complexity", p.complexity);
    setVal("remux-cplx-val", p.complexity);
  }
}

// ─── Tauri API helpers ──────────────────────────────────────────────────────

const jobCoordinator = createJobCoordinator();
const inspectDirectory = createInspectionCoordinator((path, options) => invoke("inspect_directory", { path, options }));

async function invoke(cmd, args = {}) {
  const fn = window.__TAURI__?.core?.invoke;
  if (!fn) throw new Error(`Failed to invoke ${cmd}: Tauri API is unavailable.`);
  return fn(cmd, args);
}

async function listen(event, handler) {
  const fn = window.__TAURI__?.event?.listen;
  if (!fn) throw new Error(`Failed to register ${event} listener: Tauri API is unavailable.`);
  return fn(event, handler);
}

function safeAddListener(idOrEl, event, handler) {
  const el = typeof idOrEl === "string" ? document.getElementById(idOrEl) : idOrEl;
  el?.addEventListener(event, handler);
}

// ─── Number Stepper Helper ──────────────────────────────────────────────────

function stepNumber(btn, dir) {
  const input = btn.parentElement.querySelector('input[type="number"]');
  if (!input) return;

  const step = parseFloat(input.getAttribute('step') || '1');
  const min = parseFloat(input.getAttribute('min'));
  const max = parseFloat(input.getAttribute('max'));
  // Set only on inputs whose empty state means "auto" and whose resolved value is known.
  const autoValue = parseFloat(input.dataset.autoValue);

  input.value = nextStepValue(input.value, autoValue, dir, step, min, max);
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

// The last explicitly chosen theme; a blocked or full storage must not lose the choice.
let chosenTheme = null;

function setDocumentTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
}

/**
 * An explicit choice also pins the native window theme; following the OS leaves it unpinned.
 * Updates `dataset.themeExplicit` as well as `data-theme`, so the DOM stays the single source
 * of truth after a toggle. Without it the dataset would still read "false" while storage holds
 * an explicit choice, and the two copies of the theme would disagree.
 */
function applyChosenTheme(theme) {
  setDocumentTheme(theme);
  document.documentElement.dataset.themeExplicit = "true";
  invoke("set_window_theme", { theme });
}

// Reads the theme `theme-init.js` published at bootstrap. It is the sole resolver of the stored
// choice: this must never re-read localStorage, or the two copies could disagree.
// Returns null when the user has made no explicit choice, meaning "follow the OS".
function storedTheme() {
  const dataset = document.documentElement?.dataset;
  if (dataset?.themeExplicit !== "true") return null;
  return dataset.theme === "light" || dataset.theme === "dark" ? dataset.theme : null;
}

/** Applies the user's stored theme, or follows the OS colour scheme live until they pick one. */
function initTheme() {
  const saved = storedTheme();
  if (saved) {
    applyChosenTheme(saved);
    return;
  }
  const darkScheme = window.matchMedia?.("(prefers-color-scheme: dark)");
  const follow = dark => {
    if (!chosenTheme && !storedTheme()) setDocumentTheme(dark ? "dark" : "light");
  };
  if (!document.documentElement?.dataset?.theme) {
    follow(darkScheme?.matches ?? true);
  }
  darkScheme?.addEventListener("change", e => follow(e.matches));
}

function toggleTheme() {
  const theme = document.documentElement.getAttribute("data-theme") === "dark" ? "light" : "dark";
  chosenTheme = theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {}
  applyChosenTheme(theme);
}

// ─── Navigation ──────────────────────────────────────────────────────────────

function switchView(viewId) {
  document.querySelectorAll(".view").forEach(v => v.classList.remove("active"));
  document.querySelectorAll(".pipeline-tab-btn").forEach(btn => btn.classList.remove("active"));
  document.getElementById(`view-${viewId}`)?.classList.add("active");
  document.getElementById(`nav-${viewId}`)?.classList.add("active");
  document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
    btn.setAttribute("aria-selected", btn.classList.contains("active") ? "true" : "false");
  });

  // The titlebar shows the Remux profile picker or the Extract mode badge.
  const profileDropdown = document.getElementById("profile-dropdown-wrapper");
  const extractBadge = document.getElementById("extract-header-badge");
  if (profileDropdown) {
    profileDropdown.style.display = viewId === "extract" ? "none" : "";
  }
  if (extractBadge) {
    extractBadge.style.display = viewId === "extract" ? "inline-flex" : "none";
  }
}

// ─── System Telemetry ───────────────────────────────────────────────────────

// Engine write throughput that fills the Disk I/O bar completely.
const IO_BAR_FULL_SCALE_MB_S = 300;

function updateWorkerChips(isJobActive = false, workersCount = defaultWorkers ?? 8, ioJobsCount = 2) {
  document.querySelectorAll(".metric-workers-chip").forEach(chip => {
    chip.innerText = isJobActive ? `⚡ ${workersCount} Workers Active` : `⚡ Standby (${workersCount} Workers)`;
  });
  document.querySelectorAll(".metric-iojobs-chip").forEach(chip => {
    chip.innerText = isJobActive ? `🔀 ${ioJobsCount} IO Jobs Active` : `🔀 Idle (${ioJobsCount} IO Jobs)`;
  });
}

function applyTelemetryData(data) {
  const cpu = Math.round(data.cpu_pct || 0);
  const ramUsed = data.ram_used_mb || 0;
  const ramTotal = data.ram_total_mb || 16384;
  const ramPct = Math.round(data.ram_pct || 0);
  // `io_mb_s` is the write throughput of the engine's own processes, so it is
  // zero whenever no batch is writing; show that as neutral rather than "0 MB/s".
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

  document.querySelectorAll(".metric-cpu-fill").forEach(fill => setWidth(fill, `${cpu}%`));
  document.querySelectorAll(".metric-cpu-val").forEach(el => setText(el, `${cpu}%`));

  document.querySelectorAll(".metric-ram-fill").forEach(fill => setWidth(fill, `${ramPct}%`));
  document.querySelectorAll(".metric-ram-val").forEach(el => setText(el, `${ramUsed} MB / ${(ramTotal / 1024).toFixed(0)} GB`));

  const ioWidth = ioMb === null ? "0%" : `${Math.min(100, (ioMb / IO_BAR_FULL_SCALE_MB_S) * 100)}%`;
  const ioText = ioMb === null ? "Not measured" : `${ioMb.toFixed(1)} MB/s`;

  document.querySelectorAll(".metric-io-fill").forEach(fill => {
    setWidth(fill, ioWidth);
    if (fill.classList.contains("unavailable") !== (ioMb === null)) {
      fill.classList.toggle("unavailable", ioMb === null);
    }
    if (fill.title !== ioTooltip) fill.title = ioTooltip;
  });
  document.querySelectorAll(".metric-io-val").forEach(val => {
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

const PRESET_SUBTITLE_LANGUAGES = ["ind", "eng", "jpn"];

let subAssignModalItemId = null;

function openSubAssignModal(itemId) {
  const item = remuxQueue.state.items.get(itemId);
  if (!item) return;
  const candidates = remuxQueue.candidatesForItem(itemId);

  subAssignModalItemId = itemId;
  const titleEl = document.getElementById("sub-assign-episode");
  if (titleEl) titleEl.textContent = item.name;

  const assignment = remuxQueue.assignments.get(itemId) ?? null;
  const currentPath = assignment?.asset_path ?? null;
  renderSubAssignList(candidates, currentPath);

  const customPathInput = document.getElementById("sub-custom-path");
  if (customPathInput) {
    customPathInput.value = currentPath && !candidates.includes(currentPath) ? currentPath : "";
  }

  const lang = assignment ? assignment.language : "ind";
  const isPresetLang = PRESET_SUBTITLE_LANGUAGES.includes(lang);
  const customLang = !isPresetLang && lang ? lang : "";
  const langSelect = document.getElementById("sub-lang-select");
  if (langSelect) langSelect.value = isPresetLang ? lang : (customLang ? "custom" : "ind");
  const langCustom = document.getElementById("sub-lang-custom");
  if (langCustom) {
    langCustom.value = customLang;
    langCustom.style.display = customLang ? "block" : "none";
  }

  const titleInput = document.getElementById("sub-title-input");
  if (titleInput) titleInput.value = assignment?.title || "";
  const offsetInput = document.getElementById("sub-offset-input");
  if (offsetInput) offsetInput.value = typeof assignment?.offset_ms === "number" ? assignment.offset_ms : "";

  document.getElementById("sub-assign-modal")?.classList.add("active");
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
    const slash = candidate.lastIndexOf("/");
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sub-assign-option" + (candidate === assignedPath ? " selected" : "");
    btn.dataset.path = candidate;
    const nameSpan = document.createElement("span");
    nameSpan.className = "sub-assign-name";
    nameSpan.textContent = candidate.slice(slash + 1);
    const dirSpan = document.createElement("span");
    dirSpan.className = "sub-assign-dir";
    dirSpan.textContent = slash > 0 ? candidate.slice(0, slash) : "(input root)";
    btn.append(nameSpan, dirSpan);
    listEl.appendChild(btn);
  }
}

function closeSubAssignModal() {
  subAssignModalItemId = null;
  document.getElementById("sub-assign-modal")?.classList.remove("active");
}

function clearSubAssignOptionSelection() {
  document.querySelectorAll("#sub-assign-list .sub-assign-option").forEach(btn => {
    btn.classList.remove("selected");
  });
}

function saveSubAssignment() {
  const itemId = subAssignModalItemId;
  const customPath = document.getElementById("sub-custom-path")?.value?.trim() || "";
  const candidatePath = document.querySelector("#sub-assign-list .sub-assign-option.selected")?.dataset.path || "";
  const chosenPath = customPath || candidatePath;

  if (!chosenPath) {
    remuxQueue.assignments.delete(itemId);
  } else {
    let language = document.getElementById("sub-lang-select")?.value || "ind";
    if (language === "custom") {
      language = document.getElementById("sub-lang-custom")?.value?.trim() || null;
    }
    // `offset_ms` is an i64 and serde rejects a float, so a fraction is truncated. An
    // empty field must stay null (Number("") is 0, which would suppress the global offset),
    // and a value beyond the safe-integer range would fail the whole run_job request.
    const raw = document.getElementById("sub-offset-input")?.value?.trim() ?? "";
    const truncatedMs = Math.trunc(Number(raw));
    const offsetMs = raw !== "" && Number.isSafeInteger(truncatedMs) ? truncatedMs : null;
    remuxQueue.assignments.set(itemId, {
      target_video: itemId,
      asset_path: chosenPath,
      track_type: "subtitle",
      language,
      title: document.getElementById("sub-title-input")?.value?.trim() || null,
      offset_ms: offsetMs,
    });
  }
  remuxQueue.applyAssignmentToCard(itemId);
}

export async function scanInputDirectory(path, mode, { force = false, options } = {}) {
  const tracker = queueFor(mode);
  let scanToken = null;
  path = path.trim();
  if (!path) {
    tracker.reset();
    tracker.lastScannedPath = null;
    jobCoordinator.syncControls();
    return false;
  }
  try {
    if (!force && tracker.reuseInspection(path)) return true;

    scanToken = tracker.startScan(path);
    jobCoordinator.syncControls();
    const manifest = await inspectDirectory(path, { force, options });
    const acceptedPath = manifest?.canonical_dir || path;
    return tracker.acceptInspection(scanToken, acceptedPath, manifest);
  } catch (err) {
    console.log("[inspect_directory] failed:", err);
    if (scanToken !== null) tracker.rejectInspection(scanToken);
    return false;
  } finally {
    jobCoordinator.syncControls();
  }
}

async function chooseInputDirectory(targetId) {
  if (jobCoordinator.activeMode !== null) return;
  const directory = await invoke("pick_directory");
  if (!directory || jobCoordinator.activeMode !== null) return;

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

/**
 * Queues one line or a `job-log` batch and flushes it on the next animation frame.
 * The queue keeps only the newest MAX_LOG_LINES: no frame fires while the window
 * is hidden, and the console would drop the older lines on flush anyway.
 */
function appendLog(logEl, text) {
  if (!logEl) return;

  _logQueues.set(logEl, pushCapped(_logQueues.get(logEl) ?? [], text, MAX_LOG_LINES));

  if (!_logRafIds.has(logEl)) {
    const rafId = requestAnimationFrame(() => {
      _logRafIds.delete(logEl);
      const pending = _logQueues.get(logEl);
      if (!pending || pending.length === 0) return;
      _logQueues.set(logEl, []);

      logEl.querySelector(".console-empty-prompt")?.remove();

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
      <span class="prompt-text">Console output stream initialized. Ready for batch execution...</span>
    </div>`;
}

// ─── Modal Dialog ────────────────────────────────────────────────────────────

let modalChain = Promise.resolve();

/** Shows `#confirm-modal`; calls queue so a second dialog waits for the first to close. */
function showModal(title, message, { confirmLabel = "Confirm", showCancel = true } = {}) {
  const run = () =>
    new Promise(resolve => {
      const modal = document.getElementById("confirm-modal");
      const cancelBtn = document.getElementById("modal-cancel");
      const confirmBtn = document.getElementById("modal-confirm");

      document.getElementById("modal-title").textContent = title;
      document.getElementById("modal-desc").textContent = message;
      confirmBtn.textContent = confirmLabel;
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
    el.addEventListener("mousedown", (e) => {
      if (e.target.closest('button, input, a, select, textarea, .resize-handle')) {
        return;
      }
      if (e.buttons === 1) {
        invoke("start_drag");
      }
    });
  });

  document.querySelectorAll(".resize-handle").forEach(el => {
    el.addEventListener("mousedown", (e) => {
      if (e.buttons === 1 && el.dataset.direction) {
        invoke("start_resize", { direction: el.dataset.direction });
      }
    });
  });

  document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
    btn.addEventListener("click", () => switchView(btn.dataset.view));
  });

  listen(CLEANUP_ERROR, event => showCleanupError(event.payload)).catch(error => {
    console.error("[cleanup-error] listener failed:", error);
  });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    btn.addEventListener("click", () => chooseInputDirectory(btn.dataset.target));
  });
  document.addEventListener("click", e => {
    const emptyAction = e.target.closest(".empty-state-action[data-target]");
    if (emptyAction) chooseInputDirectory(emptyAction.dataset.target);
  });

  // ─── GUI Manual Subtitle Selection ────────────────────────────────────────
  safeAddListener("remux-episode-list", "click", e => {
    const btn = e.target.closest(".sub-assign-btn");
    if (btn?.dataset.itemId) openSubAssignModal(btn.dataset.itemId);
  });
  safeAddListener("sub-assign-save", "click", () => {
    if (jobCoordinator.activeMode !== null) return;
    if (subAssignModalItemId) saveSubAssignment();
    closeSubAssignModal();
  });

  safeAddListener("sub-assign-cancel", "click", closeSubAssignModal);
  safeAddListener("sub-assign-clear", "click", () => {
    if (jobCoordinator.activeMode !== null) return;
    if (subAssignModalItemId) {
      remuxQueue.assignments.delete(subAssignModalItemId);
      remuxQueue.applyAssignmentToCard(subAssignModalItemId);
    }
    closeSubAssignModal();
  });

  safeAddListener("sub-browse-btn", "click", async () => {
    if (jobCoordinator.activeMode !== null) return;
    try {
      const picked = await invoke("pick_subtitle_file");
      if (picked) {
        const customPathInput = document.getElementById("sub-custom-path");
        if (customPathInput) customPathInput.value = picked;
        clearSubAssignOptionSelection();
      }
    } catch (e) {
      console.error("Failed to pick subtitle file:", e);
    }
  });

  safeAddListener("sub-lang-select", "change", e => {
    const langCustom = document.getElementById("sub-lang-custom");
    if (langCustom) {
      langCustom.style.display = e.target.value === "custom" ? "block" : "none";
      if (e.target.value === "custom") langCustom.focus();
    }
  });

  safeAddListener("sub-assign-modal", "click", e => {
    if (e.target === e.currentTarget) closeSubAssignModal();
  });

  safeAddListener("sub-assign-list", "click", e => {
    if (jobCoordinator.activeMode !== null) return;
    const option = e.target.closest(".sub-assign-option");
    if (!option?.dataset.path) return;
    clearSubAssignOptionSelection();
    option.classList.add("selected");
    const customPathInput = document.getElementById("sub-custom-path");
    if (customPathInput) customPathInput.value = "";
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
    const clamp = v => Math.min(Number(cplxSlider.max), Math.max(Number(cplxSlider.min), v));
    const syncFromSlider = () => {
      cplxNumber.value = cplxSlider.value;
    };
    const syncFromNumber = () => {
      const v = parseInt(cplxNumber.value, 10);
      if (!isNaN(v)) cplxSlider.value = String(clamp(v));
    };
    const normalizeNumber = () => {
      const v = parseInt(cplxNumber.value, 10);
      const normalized = String(isNaN(v) ? Number(cplxSlider.min) : clamp(v));
      cplxNumber.value = normalized;
      cplxSlider.value = normalized;
    };

    cplxSlider.addEventListener("input", syncFromSlider);
    cplxSlider.addEventListener("change", syncFromSlider);
    cplxNumber.addEventListener("input", syncFromNumber);
    cplxNumber.addEventListener("change", normalizeNumber);
    cplxNumber.addEventListener("blur", normalizeNumber);
  }

  // Editing any encoding field by hand leaves the selected preset.
  ["remux-bitrate", "remux-complexity", "remux-cplx-val", "remux-ffmpeg-threads", "remux-sub-offset", "remux-dry-run", "remux-no-cache"].forEach(id => {
    safeAddListener(id, "change", () => applyPreset("custom"));
    safeAddListener(id, "input", () => applyPreset("custom"));
  });

  const jobRuntime = {
    coordinator: jobCoordinator,
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
  const controllers = {
    remux: new JobController("remux", jobRuntime),
    extract: new JobController("extract", jobRuntime),
  };
  for (const { id, mode, select } of BULK_SELECTION_CONTROLS) {
    safeAddListener(id, "click", () => controllers[mode].applyBulkSelection(mode, select));
  }
  jobCoordinator.syncControls();
  telemetryLoop.start();
  loadPresetProfiles();
  loadDefaultWorkers();
  restoreQueueFromSnapshot(controllers);

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
