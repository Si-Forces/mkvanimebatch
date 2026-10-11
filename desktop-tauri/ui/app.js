/* MKV Anime Batch desktop frontend.
   Talks to the Tauri host through window.__TAURI__ (withGlobalTauri: true in tauri.conf.json). */

import { BULK_SELECTION_CONTROLS, CLEANUP_ERROR, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { classifyLine, pushCapped } from "./log-classify.js";
import { createTelemetryLoop } from "./telemetry.js";
import { createJobCoordinator } from "./job-coordinator.js";
import { createDirectoryScanner } from "./directory-scan.js";
import { nextStepValue } from "./stepper.js";
import { createSettingsChips } from "./settings-chips.js";
import { createPresetMenu } from "./preset-menu.js";
import { createTheme } from "./theme.js";
import {
  batchClock,
  candidateTier,
  filterCounts,
  logDisplayText,
  pathSegments,
  prettyPath,
  progressSegments,
  remuxAudioText,
  sparkPoints,
  statusLabel,
  statusTone,
} from "./presentation.js";

// ─── Tauri API helpers ──────────────────────────────────────────────────────

const jobCoordinator = createJobCoordinator();

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

const byId = id => document.getElementById(id);

function setText(id, text) {
  const el = byId(id);
  if (el && el.textContent !== text) el.textContent = text;
}

/** Fills `el` with `path` as Finder-style crumbs: segments joined by ›, the last one bold. */
function renderCrumbs(el, path) {
  if (!el) return;
  el.textContent = "";
  el.title = prettyPath(path);
  pathSegments(path).forEach((segment, i, all) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.setAttribute?.("aria-hidden", "true");
      sep.textContent = "›";
      el.appendChild(sep);
    }
    const span = document.createElement("span");
    if (i === all.length - 1) span.className = "crumb-end";
    span.textContent = segment;
    el.appendChild(span);
  });
}

// ─── Worker Chips, Presets and Theme ────────────────────────────────────────

const chips = createSettingsChips({
  invoke,
  doc: document,
  // A running job owns the chips; they show its own settings, not the defaults.
  isJobActive: () => jobCoordinator.activeMode !== null,
});
const presets = createPresetMenu({ invoke, doc: document });
const theme = createTheme({
  doc: document,
  win: window,
  // `localStorage` can throw on access, not only on write, so it is read inside the call, where
  // the theme's own try/catch covers it.
  storage: { setItem: (key, value) => localStorage.setItem(key, value) },
  invoke,
});

// ─── Number Stepper Helper ──────────────────────────────────────────────────

function stepNumber(btn, dir) {
  const input = btn.parentElement.querySelector('input[type="number"]');
  if (!input || input.disabled) return;

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

// ─── Queues and the view around them ────────────────────────────────────────

const remuxQueue = new QueueTracker("remux");
const extractQueue = new QueueTracker("extract");
let currentView = "remux";

function queueFor(mode) {
  return mode === "extract" ? extractQueue : remuxQueue;
}

function switchView(viewId) {
  currentView = viewId === "extract" ? "extract" : "remux";
  document.documentElement.dataset.view = currentView;
  document.querySelectorAll(".pipeline-tab-btn[data-view]").forEach(btn => {
    const active = btn.dataset.view === currentView;
    btn.classList.toggle("active", active);
    if (active) btn.setAttribute("aria-current", "page");
    else btn.removeAttribute?.("aria-current");
  });
  lastLibraryKey = null;
  lastAssignKey = null;
  scheduleChrome(currentView);
}

// The chrome (list header, filters, progress, library, inspector) redraws at most once a frame,
// from the queue's state, after the Queue Tracker reports a change.
const pendingChrome = new Set();
let chromeScheduled = false;

function scheduleChrome(mode) {
  pendingChrome.add(mode);
  if (chromeScheduled) return;
  chromeScheduled = true;
  requestAnimationFrame(() => {
    chromeScheduled = false;
    const modes = [...pendingChrome];
    pendingChrome.clear();
    for (const m of modes) renderChrome(m);
  });
}

for (const mode of ["remux", "extract"]) queueFor(mode).onChange = () => scheduleChrome(mode);

const listHeadKeys = { remux: null, extract: null };

function renderChrome(mode) {
  const queue = queueFor(mode);
  const { state } = queue;
  const dir = queue.currentDirectory;

  if (listHeadKeys[mode] !== dir) {
    listHeadKeys[mode] = dir;
    const segments = pathSegments(dir);
    setText(`${mode}-title`, segments.at(-1) ?? "No folder open");
    renderCrumbs(byId(`${mode}-path`), dir);
  }

  const counts = filterCounts(state);
  // Every write below is skipped when the value is unchanged: this runs on each progress frame.
  document.querySelectorAll(`.filter-chip[data-mode="${mode}"]`).forEach(chip => {
    const on = chip.dataset.filter === queue.filter;
    if (chip.classList.contains("is-on") !== on) {
      chip.classList.toggle("is-on", on);
      chip.setAttribute("aria-pressed", String(on));
    }
    const n = chip.querySelector(".n");
    const count = String(counts[chip.dataset.filter] ?? 0);
    if (n && n.textContent !== count) n.textContent = count;
  });

  const stack = byId(`${mode}-stack`);
  if (stack) {
    const parts = progressSegments(state);
    [parts.done, parts.skipped, parts.failed, parts.live].forEach((pct, i) => {
      const seg = stack.children[i];
      const width = `${pct.toFixed(2)}%`;
      if (seg && seg.style.width !== width) seg.style.width = width;
    });
  }
  setText(`${mode}-clock`, batchClock(state));

  if (mode !== currentView) return;
  renderLibrary(queue);
  renderInspector(queue);
  renderLogBadge();
}

let lastLibraryKey = null;

function renderLibrary(queue) {
  const tree = byId("library-tree");
  const dir = queue.currentDirectory;
  const releases = queue.releaseCounts();
  const key = JSON.stringify([queue.mode, dir, [...releases]]);
  if (!tree || key === lastLibraryKey) return;
  lastLibraryKey = key;
  tree.textContent = "";
  if (!dir) return;

  const row = (className, name, count, release) => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `nav ${className}`;
    if (release) btn.dataset.release = release;
    btn.title = release ? prettyPath(release) : prettyPath(dir);
    const label = document.createElement("span");
    label.className = "lib-name";
    label.textContent = name;
    const n = document.createElement("span");
    n.className = "lib-count";
    n.textContent = String(count);
    btn.append(label, n);
    tree.appendChild(btn);
  };
  row("lib-root", pathSegments(dir).at(-1) ?? dir, queue.state.totalCount, null);
  for (const [release, count] of releases) {
    if (release !== ".") row("lib-release", prettyPath(release), count, release);
  }
}

// ─── Inspector ──────────────────────────────────────────────────────────────

const TIER_NAMES = { same: "Same folder", ancestor: "Parent folder" };
let lastAssignKey = null;

function renderInspector(queue) {
  const body = byId("insp-body");
  const empty = byId("insp-empty");
  const item = queue.inspectedItem();
  if (body) body.hidden = !item;
  if (empty) empty.hidden = Boolean(item);
  if (!item || !body) return;

  const mode = queue.mode;
  const meta = item.metadata;
  setText("insp-num", String(queue.state.order.indexOf(item.id) + 1).padStart(2, "0"));
  setText("insp-title", item.name);
  const pathEl = byId("insp-path");
  if (pathEl && pathEl.dataset.path !== item.id) {
    pathEl.dataset.path = item.id;
    renderCrumbs(pathEl, item.id);
  }

  const error = item.status === "error" ? item.error : item.inspectionError;
  const errorEl = byId("insp-error");
  if (errorEl) errorEl.hidden = !error;
  setText("insp-error", error || "");

  const pill = byId("insp-status");
  const included = item.selected !== false;
  const pillClass = `pill tone-${included ? statusTone(item.status) : "mute"}`;
  if (pill && pill.className !== pillClass) pill.className = pillClass;
  setText("insp-status", included ? statusLabel(item) : "Not in batch");

  const video = meta?.video_tag || (item.inspectionError ? "Inspection failed" : "Unknown");
  setText("insp-video", mode === "remux" && meta?.video_tag ? `${video} · copied` : video);
  setText("insp-audio-label", mode === "remux" ? "Audio" : "Subtitles");
  setText("insp-audio", mode === "remux"
    ? (meta ? remuxAudioText(meta, queue.getBitrate()) : "Unknown")
    : (meta?.sub_summary || (meta ? "None" : "Unknown")));

  // The output folder setting, or the setting's own description of its default. Where the file
  // lands inside it, and its name, are the engine's to decide.
  const outInput = byId(`${mode}-out-dir`);
  const outDir = outInput?.value?.trim() ?? "";
  const outEl = byId("insp-out");
  if (outEl && outEl.dataset.path !== outDir) {
    outEl.dataset.path = outDir;
    if (outDir) renderCrumbs(outEl, outDir);
    else outEl.textContent = outInput?.placeholder ?? "Default";
  }

  const fonts = meta?.fonts_count;
  setText("insp-fonts-head", fonts == null ? "Fonts" : `Fonts · ${fonts}`);
  const fontsDir = byId(`${mode}-fonts-dir`)?.value?.trim();
  setText("insp-fonts", fonts == null
    ? "Not inspected"
    : `${fonts} attached ${fonts === 1 ? "font" : "fonts"} · folder: ${fontsDir ? prettyPath(fontsDir) : "auto"}`);

  if (mode === "remux") renderSubtitleSection(queue, item);
}

function renderSubtitleSection(queue, item) {
  const assignment = queue.assignments.get(item.id) ?? null;
  const candidates = queue.candidatesForItem(item.id);
  const ladder = queue.ladderFor(item.id);
  // Everything below depends only on these, so a progress event redraws none of it.
  const key = JSON.stringify([item.id, ladder, assignment, candidates]);
  if (key === lastAssignKey) return;
  lastAssignKey = key;

  const ladderEl = byId("insp-ladder");
  if (ladderEl) {
    ladderEl.textContent = "";
    ladder.forEach((rung, i) => {
      const li = document.createElement("li");
      li.className = rung.chosen ? "is-chosen" : rung.detail ? "is-found" : "";
      const n = document.createElement("span");
      n.className = "rung-n";
      n.textContent = String(i + 1);
      const label = document.createElement("span");
      label.className = "rung-label";
      label.textContent = rung.label;
      if (rung.detail) {
        const detail = document.createElement("small");
        detail.className = "rung-detail";
        detail.textContent = rung.key === "embedded" ? rung.detail : prettyPath(rung.detail);
        detail.title = detail.textContent;
        label.appendChild(detail);
      }
      const tag = document.createElement("span");
      tag.className = "rung-tag";
      tag.textContent = rung.tag;
      li.append(n, label, tag);
      ladderEl.appendChild(li);
    });
  }

  const select = byId("sub-assign-select");
  if (select) {
    select.textContent = "";
    const option = (value, label) => {
      const opt = document.createElement("option");
      opt.value = value;
      opt.textContent = label;
      select.appendChild(opt);
    };
    option("", "Automatic (follow the ladder)");
    for (const path of candidates) {
      option(path, `${prettyPath(path)} (${TIER_NAMES[candidateTier(path, item.release)]})`);
    }
    if (assignment && !candidates.includes(assignment.asset_path)) {
      option(assignment.asset_path, `${prettyPath(assignment.asset_path)} (Custom file)`);
    }
    option(BROWSE_SUBTITLE, "Choose a file…");
    select.value = assignment?.asset_path ?? "";
  }

  const reset = byId("sub-assign-reset");
  if (reset) reset.hidden = !assignment;
  const opts = byId("sub-assign-opts");
  if (opts) opts.hidden = !assignment;

  const lang = assignment?.language ?? "ind";
  const preset = PRESET_SUBTITLE_LANGUAGES.includes(lang);
  const langSelect = byId("sub-lang-select");
  if (langSelect) langSelect.value = preset ? lang : "custom";
  const langCustom = byId("sub-lang-custom");
  if (langCustom) langCustom.value = preset ? "" : (lang || "");
  const customLabel = byId("sub-lang-custom-label");
  if (customLabel) customLabel.hidden = preset;
  const title = byId("sub-title-input");
  if (title) title.value = assignment?.title || "";
  const offset = byId("sub-offset-input");
  if (offset) offset.value = typeof assignment?.offset_ms === "number" ? String(assignment.offset_ms) : "";
}

// ─── GUI Manual Subtitle Selection ──────────────────────────────────────────

const PRESET_SUBTITLE_LANGUAGES = ["ind", "eng", "jpn"];
const BROWSE_SUBTITLE = "__browse__";

/** The Track Assignment for `itemId` and `path`, from the inspector's language, title and offset. */
function subtitleAssignment(itemId, path) {
  let language = byId("sub-lang-select")?.value || "ind";
  if (language === "custom") {
    language = byId("sub-lang-custom")?.value?.trim() || null;
  }
  // `offset_ms` is an i64 and serde rejects a float, so a fraction is truncated. An
  // empty field must stay null (Number("") is 0, which would suppress the global offset),
  // and a value beyond the safe-integer range would fail the whole run_job request.
  const raw = byId("sub-offset-input")?.value?.trim() ?? "";
  const truncatedMs = Math.trunc(Number(raw));
  const offsetMs = raw !== "" && Number.isSafeInteger(truncatedMs) ? truncatedMs : null;
  return {
    target_video: itemId,
    asset_path: path,
    track_type: "subtitle",
    language,
    title: byId("sub-title-input")?.value?.trim() || null,
    offset_ms: offsetMs,
  };
}

/** Sets (or, with no path, clears) the inspected item's Track Assignment. Refused while a job runs. */
function assignSubtitle(path) {
  const item = remuxQueue.inspectedItem();
  if (!item || jobCoordinator.activeMode !== null) return;
  if (path) remuxQueue.assignments.set(item.id, subtitleAssignment(item.id, path));
  else remuxQueue.assignments.delete(item.id);
  remuxQueue.applyAssignmentToCard(item.id);
}

async function onAssignSelect(e) {
  const value = e.target.value;
  if (value !== BROWSE_SUBTITLE) {
    assignSubtitle(value);
    return;
  }
  let picked = null;
  if (jobCoordinator.activeMode === null) {
    try {
      picked = await invoke("pick_subtitle_file");
    } catch (err) {
      console.error("Failed to pick subtitle file:", err);
    }
  }
  if (picked) {
    assignSubtitle(picked);
  } else {
    // Nothing picked: put the select back on what is assigned.
    lastAssignKey = null;
    scheduleChrome("remux");
  }
}

/** A language, title or offset edit re-applies the current assignment with the new fields. */
function onAssignOptions() {
  const item = remuxQueue.inspectedItem();
  const assignment = item && remuxQueue.assignments.get(item.id);
  const customLabel = byId("sub-lang-custom-label");
  if (customLabel) customLabel.hidden = byId("sub-lang-select")?.value !== "custom";
  if (assignment) assignSubtitle(assignment.asset_path);
}

// ─── System Telemetry ───────────────────────────────────────────────────────

const SPARK_SAMPLES = 40;
const telemetrySamples = { cpu: [], ram: [], disk: [] };

function pushSample(name, value) {
  const list = telemetrySamples[name];
  list.push(value);
  if (list.length > SPARK_SAMPLES) list.shift();
}

function applyTelemetryData(data) {
  const cpu = Math.max(0, data.cpu_pct || 0);
  const ramUsedMb = data.ram_used_mb || 0;
  // `io_mb_s` is the write rate of the engine's own processes, so it is zero whenever no batch
  // is writing; show that as idle rather than "0 MB/s".
  const io = typeof data.io_mb_s === "number" && data.io_mb_s > 0 ? data.io_mb_s : 0;
  pushSample("cpu", cpu);
  pushSample("ram", data.ram_pct || 0);
  pushSample("disk", io);

  setText("tel-cpu", `${Math.round(cpu)}%`);
  setText("tel-ram", `${(ramUsedMb / 1024).toFixed(2)} GB`);
  setText("tel-disk", io > 0 ? `${io.toFixed(1)} MB/s` : "Idle");
  byId("tel-cpu-line")?.setAttribute("points", sparkPoints(telemetrySamples.cpu, 100));
  byId("tel-ram-line")?.setAttribute("points", sparkPoints(telemetrySamples.ram, 100));
  byId("tel-disk-line")?.setAttribute("points", sparkPoints(telemetrySamples.disk, Math.max(50, ...telemetrySamples.disk)));
}

const telemetryLoop = createTelemetryLoop({
  invoke,
  onData: applyTelemetryData,
  onError: (e) => console.log("[Telemetry Error]", e),
  doc: document,
});

// ─── Directory Scan ─────────────────────────────────────────────────────────

const scanInputDirectory = createDirectoryScanner({
  invoke,
  queueFor,
  onSettled: () => jobCoordinator.syncControls(),
});

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
  } else {
    input.dispatchEvent?.(new Event("change"));
  }
}

// ─── Log Drawer ─────────────────────────────────────────────────────────────

const MAX_LOG_LINES = 2000;
const _logQueues = new WeakMap();
const _logRafIds = new WeakMap();
// What each console holds, as the engine wrote it (Copy uses these, with their real paths), and
// how many of those lines read as errors (the Log button's badge).
const _rawLogs = new WeakMap();
const _logErrors = new WeakMap();

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
      let errors = _logErrors.get(logEl) ?? 0;
      for (const lineText of pending) {
        const div = document.createElement("div");
        const kind = classifyLine(lineText);
        if (kind === "line-err") errors++;
        div.className = `log-line ${kind}`;
        // Paths display Finder-style; the raw line is kept for Copy.
        div.textContent = logDisplayText(lineText);
        frag.appendChild(div);
      }
      _rawLogs.set(logEl, pushCapped(_rawLogs.get(logEl) ?? [], pending, MAX_LOG_LINES));
      _logErrors.set(logEl, errors);
      const shouldFollow = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 32;
      logEl.appendChild(frag);

      while (logEl.childElementCount > MAX_LOG_LINES) {
        logEl.firstElementChild.remove();
      }

      if (shouldFollow) logEl.scrollTop = logEl.scrollHeight;
      renderLogBadge();
    });
    _logRafIds.set(logEl, rafId);
  }
}

function clearLog(logEl) {
  if (!logEl) return;
  _rawLogs.set(logEl, []);
  _logErrors.set(logEl, 0);
  logEl.innerHTML = `<div class="console-empty-prompt">No output yet. Run a batch to see its output here.</div>`;
  renderLogBadge();
}

/** The current view's log as the engine wrote it, real `/` paths included. */
function rawLogText(logEl) {
  return [...(_rawLogs.get(logEl) ?? []), ...(_logQueues.get(logEl) ?? [])].join("\n");
}

function renderLogBadge() {
  const badge = byId("log-badge");
  if (!badge) return;
  const errors = _logErrors.get(byId(`${currentView}-log`)) ?? 0;
  badge.hidden = errors === 0;
  badge.textContent = String(errors);
}

function setLogOpen(open) {
  const drawer = byId("log-drawer");
  if (drawer) drawer.hidden = !open;
  byId("btn-log")?.setAttribute("aria-expanded", String(open));
}

async function copyLog() {
  const btn = byId("log-copy");
  let label = "Copied";
  try {
    await navigator.clipboard.writeText(rawLogText(byId(`${currentView}-log`)));
  } catch (err) {
    console.error("[log] copy failed:", err);
    label = "Copy failed";
  }
  if (!btn) return;
  btn.textContent = label;
  setTimeout(() => { btn.textContent = "Copy"; }, 1500);
}

// ─── Settings Sheet and Modal Dialog ────────────────────────────────────────

let sheetOpener = null;

/** Switches to `mode` and opens its settings sheet; closing it returns focus to `opener`. */
function openSettings(mode, opener) {
  switchView(mode);
  const sheet = byId("settings-sheet");
  if (!sheet || sheet.open) return;
  sheetOpener = opener ?? document.activeElement;
  sheet.showModal();
}

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

      // Esc closes the dialog too, which counts as Cancel.
      let confirmed = false;
      const onCancel = () => modal.close();
      const onConfirm = () => {
        confirmed = true;
        modal.close();
      };
      const onClose = () => {
        cancelBtn.hidden = false;
        cancelBtn.removeEventListener("click", onCancel);
        confirmBtn.removeEventListener("click", onConfirm);
        modal.removeEventListener("close", onClose);
        resolve(confirmed);
      };

      cancelBtn.addEventListener("click", onCancel);
      confirmBtn.addEventListener("click", onConfirm);
      modal.addEventListener("close", onClose);
      modal.showModal();
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

/** The Custom preset's subtitle shows the form's own bitrate and complexity. */
function updateCustomPresetText() {
  const bitrate = byId("remux-bitrate")?.value;
  const complexity = byId("remux-complexity")?.value;
  if (bitrate && complexity !== undefined) setText("preset-custom-sub", `Opus ${bitrate} · complexity ${complexity}`);
}

function initApp() {
  theme.init();

  safeAddListener("btn-theme", "click", theme.toggle);
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
  document.querySelectorAll(".task-settings-btn[data-view]").forEach(btn => {
    btn.addEventListener("click", () => openSettings(btn.dataset.view, btn));
  });

  listen(CLEANUP_ERROR, event => showCleanupError(event.payload)).catch(error => {
    console.error("[cleanup-error] listener failed:", error);
  });

  document.querySelectorAll(".browse-btn[data-target]").forEach(btn => {
    btn.addEventListener("click", () => chooseInputDirectory(btn.dataset.target));
  });
  document.addEventListener("click", e => {
    const emptyAction = e.target.closest?.(".empty-state-action[data-target]");
    if (emptyAction) chooseInputDirectory(emptyAction.dataset.target);
    const chip = e.target.closest?.(".filter-chip[data-filter]");
    if (chip) queueFor(chip.dataset.mode).setFilter(chip.dataset.filter);
  });
  safeAddListener("library-open", "click", () => chooseInputDirectory(`${currentView}-input-dir`));
  safeAddListener("library-tree", "click", e => {
    const row = e.target.closest?.(".nav");
    if (row) queueFor(currentView).revealRelease(row.dataset.release);
  });

  // ─── Settings sheet ───────────────────────────────────────────────────────
  const sheet = byId("settings-sheet");
  safeAddListener("sheet-done", "click", () => sheet?.close());
  safeAddListener(sheet, "click", e => {
    if (e.target === sheet) sheet.close();
  });
  safeAddListener(sheet, "close", () => {
    sheetOpener?.focus?.();
    sheetOpener = null;
  });

  // ─── Log drawer ───────────────────────────────────────────────────────────
  safeAddListener("btn-log", "click", () => setLogOpen(byId("log-drawer")?.hidden !== false));
  safeAddListener("log-close", "click", () => setLogOpen(false));
  safeAddListener("log-copy", "click", copyLog);

  // ─── GUI Manual Subtitle Selection ────────────────────────────────────────
  safeAddListener("sub-assign-select", "change", onAssignSelect);
  safeAddListener("sub-assign-reset", "click", () => assignSubtitle(null));
  for (const id of ["sub-lang-select", "sub-lang-custom", "sub-title-input", "sub-offset-input"]) {
    safeAddListener(id, "change", onAssignOptions);
  }

  // ─── Presets ──────────────────────────────────────────────────────────────
  document.querySelectorAll(".profile-option-item[data-preset]").forEach(item => {
    item.addEventListener("click", (e) => {
      e.stopPropagation?.();
      if (jobCoordinator.activeMode !== null) return;
      presets.apply(item.dataset.preset);
      updateCustomPresetText();
      remuxQueue.refreshMeta();
    });
  });

  // ─── Complexity Slider & Number Synchronization ──────────────────────────
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
    const toCustom = () => {
      presets.apply("custom");
      updateCustomPresetText();
    };
    safeAddListener(id, "change", toCustom);
    safeAddListener(id, "input", toCustom);
  });
  // The rows and the inspector show the bitrate and the subtitle ladder; the inspector the folders.
  safeAddListener("remux-bitrate", "change", () => remuxQueue.refreshMeta());
  safeAddListener("remux-assets-dir", "change", () => remuxQueue.refreshMeta());
  for (const id of ["remux-out-dir", "remux-fonts-dir", "extract-out-dir", "extract-fonts-dir"]) {
    safeAddListener(id, "change", () => scheduleChrome(currentView));
  }

  const jobRuntime = {
    coordinator: jobCoordinator,
    appendLog,
    clearLog,
    invoke,
    listen,
    queueFor,
    safeAddListener,
    scanInputDirectory,
    showModal,
    telemetryLoop,
    updateWorkerChips: chips.update,
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
  presets.load();
  chips.loadDefaults();
  updateCustomPresetText();
  jobCoordinator.restore({ listen, invoke, queueFor, switchView });

  document.addEventListener("keydown", e => {
    if ((e.ctrlKey || e.metaKey) && e.key === "t") {
      e.preventDefault();
      theme.toggle();
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}
