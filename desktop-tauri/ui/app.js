/* MKV Anime Batch desktop frontend.
   Talks to the Tauri host through window.__TAURI__ (withGlobalTauri: true in tauri.conf.json). */

import { BULK_SELECTION_CONTROLS, JobController } from "./job-controller.js";
import { QueueTracker } from "./queue-view.js";
import { createRunLogs } from "./log-view.js";
import { createDirectoryScanner } from "./directory-scan.js";
import { createSettingsChips } from "./settings-chips.js";
import { createPresetMenu } from "./preset-menu.js";
import { createSettingsDraft } from "./settings-draft.js";
import { createTheme } from "./theme.js";
import { ALL_ROWS, nextState } from "./view-patch.js";
import { sparkPoints } from "./presentation.js";

// ─── Tauri API helpers ──────────────────────────────────────────────────────

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

/** Fills `el` with path `segments` as Finder-style crumbs: joined by ›, the last one bold. */
function renderCrumbs(el, segments) {
  if (!el) return;
  el.textContent = "";
  el.title = segments.join(" › ");
  segments.forEach((segment, i, all) => {
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

const chips = createSettingsChips({ doc: document });
const presets = createPresetMenu({
  doc: document,
  dispatch: dispatchIntent,
  isLocked: () => busy(),
});
const draftForm = createSettingsDraft({
  doc: document,
  dispatch: sendIntent,
  published: mode => appliedState?.[mode]?.draft,
});
const theme = createTheme({
  doc: document,
  win: window,
  // `localStorage` can throw on access, not only on write, so it is read inside the call, where
  // the theme's own try/catch covers it.
  storage: { setItem: (key, value) => localStorage.setItem(key, value) },
  invoke,
});

// ─── Number Stepper Helper ──────────────────────────────────────────────────

/**
 * One stepper click: the core steps the Settings Draft field (`draft.rs`, its bounds and its
 * auto value) and publishes the new value into the input. Only the hold-to-repeat timing is here.
 */
function stepNumber(btn, dir) {
  const input = btn.parentElement.querySelector('input[type="number"]');
  if (!input || input.disabled) return;
  // `remux-sub-offset` steps the remux draft's `sub_offset`.
  const [, mode, suffix] = input.id?.match(/^(remux|extract)-(.+)$/) ?? [];
  if (!mode) return;
  draftForm.step(mode, suffix.replaceAll("-", "_"), dir > 0 ? "up" : "down");
}

function initSteppers() {
  document.querySelectorAll('.stepper-btn').forEach(btn => {
    const dir = btn.classList.contains('minus') ? -1 : 1;
    let stepInterval = null;
    let stepTimeout = null;

    const startStepping = event => {
      if (event.button !== 0) return; // Only the primary button steps.
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
    // Per UI Events, a keyboard-activated click (Enter, Space) has `detail` 0 in WebKit,
    // Chromium and Gecko; a pointer click has `detail` >= 1 and already stepped on mousedown.
    btn.addEventListener('click', event => {
      if (event.detail === 0) stepNumber(btn, dir);
    });
  });
}

// ─── Desktop Core state ─────────────────────────────────────────────────────

/** The event the Desktop Core publishes the View State on; must match `src-tauri/src/publisher.rs`. */
const VIEW_STATE = "view-state";
/** Webview event names emitted by the engine bridge; must match `src-tauri/src/publisher.rs` (`cleanup-error`: `commands.rs`). */
const JOB_LOG = "job-log";
const CLEANUP_ERROR = "cleanup-error";
// The newest View State applied, which `syncState` hands out. A publication and a `get_state`
// answer can arrive in either order, so one not newer than this is stale and dropped.
let appliedState = null;
// The resync `get_state` in flight, and the payloads that asked for one meanwhile, which are
// evaluated again against its answer.
let resyncing = null;
let heldPayloads = [];
// What the global controls read from the View State when they were last synced. Progress and
// heartbeat publications leave it unchanged, so they skip the DOM-wide control sync.
let controlsKey = "";
// The Job Controllers by mode, once `initApp` made them.
let controllers = {};

/** Whether a start or a job holds the batch in the View State applied last. */
function busy() {
  return appliedState?.busy === true;
}

/** Re-renders every control a busy batch, a scan or the Job Coordinator moves. */
function syncControls() {
  for (const controller of Object.values(controllers)) controller.syncGlobalControls();
}

/**
 * Takes one `view-state` payload or `get_state` answer, a whole View State or a patch of the
 * one applied (`view-patch.js`): drops a stale one, renders a newer one, and resyncs for a patch
 * of a state this page does not hold.
 */
function receiveState(payload) {
  if (!payload) return;
  const next = nextState(appliedState, payload);
  if (next.action === "resync") {
    heldPayloads.push(payload);
    resync();
  } else if (next.action !== "drop") {
    applyState(next.state, next.rows);
  }
}

/**
 * Reads the whole View State once, however many patches ask for it meanwhile: a `get_state`
 * answer is a state the publisher already emitted, so the next patch's base is that answer's seq,
 * and the patches held meanwhile are dropped or merged against it. Resolves as `syncState`.
 */
function resync() {
  // `syncState` never rejects (a failed read answers null) and a held payload that throws is
  // logged, so this promise always fulfils and the flag is cleared on every path.
  resyncing ??= syncState().then(state => {
    // Swapped and cleared before the drain, so a held payload that needs another read starts its own.
    const held = heldPayloads;
    heldPayloads = [];
    resyncing = null;
    // An unreachable core is not asked again for the held patches; the next payload asks.
    if (!state) return state;
    for (const payload of held) {
      try {
        receiveState(payload);
      } catch (err) {
        console.error("[view-state] held payload failed:", err);
      }
    }
    return state;
  });
  return resyncing;
}

/**
 * Renders the View State `state`, redrawing only the queue cards of `rows` (per mode, the
 * changed row indices or "all", from `nextState`).
 */
function applyState(state, rows = ALL_ROWS) {
  appliedState = state;
  // The fields first, so the queues and the chrome below read the published draft.
  draftForm.apply("remux", state.remux.draft);
  draftForm.apply("extract", state.extract.draft);
  chips.render(state);
  presets.render(state.presets, state.remux.draft);
  renderTelemetry(state.telemetry);
  remuxQueue.applyView(state.remux, rows.remux);
  extractQueue.applyView(state.extract, rows.extract);
  // What the controls render: `busy`, each mode's published controls and flagged field, and its
  // manifest, whose fresh row checkboxes take the selection lock.
  const key = JSON.stringify([state.busy, ...["remux", "extract"].map(mode => {
    const { controls, draft, manifest_rev: rev } = state[mode];
    return [controls, draft?.invalid, rev];
  })]);
  if (key !== controlsKey) {
    controlsKey = key;
    syncControls();
  }
}

/**
 * Fetches and applies the current View State. Resolves to the newest state applied, which is a
 * publication that overtook the answer when it is stale, or null when the core is unreachable.
 */
async function syncState() {
  try {
    receiveState(await invoke("get_state"));
    return appliedState;
  } catch (err) {
    console.error("[get_state] failed:", err);
    return null;
  }
}

// The Intents sent so far. Each is sent once the one before it was answered: the IPC does not keep
// the order of concurrent calls, and an edit must not reach the core after a later Intent. A scan,
// a Run and a Stop are the exceptions: each is answered only once its inspection, its start or its
// teardown settles, so the chain waits for it to be sent, not answered (a Stop must not wait for
// the Run it stops; nor can they cross, as Stop is shown and enabled only once the published state
// shows the start, `run.phase` not idle). A scan reads its own path and a Run the draft the core
// already holds, so what overtakes either is harmless, and a newer scan of the mode can still
// overtake one in the core.
// ponytail: the IPC may still deliver an Intent sent right after one of them before it; an early
// ack from the core would close that gap if it ever matters.
let dispatchChain = Promise.resolve();

// The Intents the chain does not wait on.
const UNCHAINED_INTENTS = new Set(["scan_directory", "run", "stop"]);

// The Intents the core answers before it publishes their change, which reaches the page as a
// coalesced `view-state` publication (ADR-0016, S3 and S4): the draft Intents and `view_visible`.
const UNSYNCED_INTENTS = new Set(["edit_draft", "set_preset", "step_draft", "view_visible"]);

/**
 * Sends an Intent to the Desktop Core after every Intent sent before it was answered (a scan, a
 * Run or a Stop only sent), then applies the state it published for it, but for a draft Intent or
 * `view_visible`, whose state arrives as a publication. Resolves to the core's reply, and rejects with its
 * refusal (the queue or the settings are locked, a Run failed) once that state is applied.
 */
function sendIntent(intent) {
  const answer = dispatchChain.then(async () => {
    try {
      return await invoke("dispatch", { intent });
    } finally {
      if (!UNSYNCED_INTENTS.has(intent.type)) await syncState();
    }
  });
  if (!UNCHAINED_INTENTS.has(intent.type)) dispatchChain = answer.catch(() => {});
  return answer;
}

/** `sendIntent`, resolving to false when the core refused the Intent or is unreachable. */
function dispatchIntent(intent) {
  return sendIntent(intent).catch(err => {
    console.error(`[dispatch] ${intent.type} refused:`, err);
    return false;
  });
}

// ─── Queues and the view around them ────────────────────────────────────────

const remuxQueue = new QueueTracker("remux", dispatchIntent);
const extractQueue = new QueueTracker("extract", dispatchIntent);
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
  // The badge counts the shown mode's errors: not a frame late on a switch.
  renderLogBadge();
  scheduleChrome(currentView);
}

// The chrome (list header, filters, progress, library, inspector) redraws at most once a frame,
// from the queue's published ModeView, after the Queue Tracker reports a change.
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

/** Draws the published list header, filter counts, progress bar and clock of `mode`. */
function renderChrome(mode) {
  const queue = queueFor(mode);
  const { view } = queue;

  const headKey = JSON.stringify([view.title, view.crumbs]);
  if (listHeadKeys[mode] !== headKey) {
    listHeadKeys[mode] = headKey;
    setText(`${mode}-title`, view.title ?? "No folder open");
    renderCrumbs(byId(`${mode}-path`), view.crumbs ?? []);
  }

  // Every write below is skipped when the value is unchanged: this runs on each progress frame.
  document.querySelectorAll(`.filter-chip[data-mode="${mode}"]`).forEach(chip => {
    const on = chip.dataset.filter === queue.filter;
    if (chip.classList.contains("is-on") !== on) {
      chip.classList.toggle("is-on", on);
      chip.setAttribute("aria-pressed", String(on));
    }
    const n = chip.querySelector(".n");
    const count = String(view.filters?.[chip.dataset.filter] ?? 0);
    if (n && n.textContent !== count) n.textContent = count;
  });

  const stack = byId(`${mode}-stack`);
  if (stack && view.progress) {
    const parts = view.progress;
    [parts.done, parts.skipped, parts.failed, parts.live].forEach((pct, i) => {
      const seg = stack.children[i];
      const width = `${pct.toFixed(2)}%`;
      if (seg && seg.style.width !== width) seg.style.width = width;
    });
  }
  if (view.clock != null) setText(`${mode}-clock`, view.clock);

  if (mode !== currentView) return;
  renderLibrary(queue);
  renderInspector(queue);
  renderLogBadge();
}

let lastLibraryKey = null;

/** Draws the published Library: the folder, then each other Release Directory. */
function renderLibrary(queue) {
  const tree = byId("library-tree");
  const library = queue.view.library ?? [];
  const key = JSON.stringify([queue.mode, library]);
  if (!tree || key === lastLibraryKey) return;
  lastLibraryKey = key;
  tree.textContent = "";
  // These counts are each release's total; the list's group headers count only the rows the
  // filter shows, so the two differ under To do, Done and Failed.
  for (const entry of library) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `nav ${entry.release === null ? "lib-root" : "lib-release"}`;
    if (entry.release !== null) btn.dataset.release = entry.release;
    btn.title = entry.tooltip;
    const label = document.createElement("span");
    label.className = "lib-name";
    label.textContent = entry.label;
    const n = document.createElement("span");
    n.className = "lib-count";
    n.textContent = String(entry.count);
    btn.append(label, n);
    tree.appendChild(btn);
  }
}

// ─── Inspector ──────────────────────────────────────────────────────────────

let lastAssignKey = null;

/**
 * Draws the Episode Inspector the core published for the queue's mode (`inspector.rs`), or its
 * empty state while the queue has no rows.
 */
function renderInspector(queue) {
  const body = byId("insp-body");
  const empty = byId("insp-empty");
  const insp = queue.view.inspector ?? null;
  if (body) body.hidden = !insp;
  if (empty) empty.hidden = Boolean(insp);
  if (!insp || !body) return;

  const mode = queue.mode;
  setText("insp-num", insp.number);
  setText("insp-title", insp.title);
  const pathEl = byId("insp-path");
  if (pathEl && pathEl.dataset.path !== insp.id) {
    pathEl.dataset.path = insp.id;
    renderCrumbs(pathEl, insp.crumbs);
  }

  const errorEl = byId("insp-error");
  if (errorEl) errorEl.hidden = insp.error === null;
  setText("insp-error", insp.error ?? "");

  const pill = byId("insp-status");
  const pillClass = `pill tone-${insp.pill.tone}`;
  if (pill && pill.className !== pillClass) pill.className = pillClass;
  setText("insp-status", insp.pill.label);

  setText("insp-video", insp.video);
  setText("insp-audio-label", insp.audio_label);
  setText("insp-audio", insp.audio);

  // The output folder setting, or the setting's own description of its default. Where the file
  // lands inside it, and its name, are the engine's to decide.
  const outEl = byId("insp-out");
  const outKey = JSON.stringify([mode, insp.out_crumbs]);
  if (outEl && outEl.dataset.path !== outKey) {
    outEl.dataset.path = outKey;
    if (insp.out_crumbs) renderCrumbs(outEl, insp.out_crumbs);
    else outEl.textContent = byId(`${mode}-out-dir`)?.placeholder ?? "Default";
  }

  setText("insp-fonts-head", insp.fonts_head);
  setText("insp-fonts", insp.fonts);

  if (mode === "remux") renderSubtitleSection(insp);
}

function renderSubtitleSection(insp) {
  const { assignment } = insp;
  // Everything below depends only on these, so a progress event redraws none of it.
  const key = JSON.stringify([insp.id, insp.ladder, insp.options, assignment]);
  if (key === lastAssignKey) return;
  lastAssignKey = key;

  const ladderEl = byId("insp-ladder");
  if (ladderEl) {
    ladderEl.textContent = "";
    insp.ladder.forEach((rung, i) => {
      const li = document.createElement("li");
      li.className = rung.chosen ? "is-chosen" : rung.found ? "is-found" : "";
      const n = document.createElement("span");
      n.className = "rung-n";
      n.textContent = String(i + 1);
      const label = document.createElement("span");
      label.className = "rung-label";
      label.textContent = rung.label;
      if (rung.found) {
        const detail = document.createElement("small");
        detail.className = "rung-detail";
        detail.textContent = rung.detail;
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
    for (const { value, label } of insp.options) option(value, label);
    // The file picker is the view's own action, not a choice the core publishes.
    option(BROWSE_SUBTITLE, "Choose a file…");
    select.value = assignment?.asset_path ?? "";
  }

  const reset = byId("sub-assign-reset");
  if (reset) reset.hidden = !assignment;
  const opts = byId("sub-assign-opts");
  if (opts) opts.hidden = !assignment;

  // An assignment's null language is a custom tag not typed yet, not the "ind" default.
  const lang = assignment ? assignment.language : "ind";
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

/**
 * The Track Assignment for `itemId` and `path`, from the inspector's language, title and offset.
 * Still the view's (out of S4): a view that builds a request is logic the core could own.
 */
function subtitleAssignment(itemId, path) {
  let language = byId("sub-lang-select")?.value || "ind";
  if (language === "custom") {
    language = byId("sub-lang-custom")?.value?.trim() || null;
  }
  // `offset_ms` is an i64 and serde rejects a float, so a fraction is truncated. An
  // empty field must stay null (Number("") is 0, which would suppress the global offset),
  // and a value beyond the safe-integer range would fail the whole Intent.
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
async function assignSubtitle(path) {
  const insp = remuxQueue.view.inspector;
  if (!insp || busy()) return;
  await dispatchIntent(path
    ? { type: "assign_track", mode: "remux", assignment: subtitleAssignment(insp.id, path) }
    : { type: "clear_track", mode: "remux", id: insp.id });
  // Redraw the inspector even when the core refused the change, so it shows what is assigned.
  lastAssignKey = null;
  scheduleChrome("remux");
}

async function onAssignSelect(e) {
  const value = e.target.value;
  if (value !== BROWSE_SUBTITLE) {
    await assignSubtitle(value);
    return;
  }
  let picked = null;
  if (!busy()) {
    try {
      picked = await invoke("pick_subtitle_file");
    } catch (err) {
      console.error("Failed to pick subtitle file:", err);
    }
  }
  if (picked) {
    await assignSubtitle(picked);
  } else {
    // Nothing picked: put the select back on what is assigned.
    lastAssignKey = null;
    scheduleChrome("remux");
  }
}

/** A language, title or offset edit re-applies the current assignment with the new fields. */
function onAssignOptions() {
  const assignment = remuxQueue.view.inspector?.assignment;
  const custom = byId("sub-lang-select")?.value === "custom";
  const customLabel = byId("sub-lang-custom-label");
  if (customLabel) customLabel.hidden = !custom;
  if (assignment) assignSubtitle(assignment.asset_path);
  // Picking "Custom tag…" puts the cursor in the tag box.
  if (custom && document.activeElement === byId("sub-lang-select")) byId("sub-lang-custom")?.focus?.();
}

// ─── System Telemetry ───────────────────────────────────────────────────────

// The texts and samples are the core's (`crates/mkv-desktop-core/src/telemetry.rs`), which also
// sets the cadence; only the sparkline drawing is the page's. Null until the first sample.
let lastTelemetry = "";

function renderTelemetry(telemetry) {
  const key = JSON.stringify(telemetry ?? null);
  if (!telemetry || key === lastTelemetry) return;
  lastTelemetry = key;
  const { cpu, ram, disk, samples } = telemetry;
  setText("tel-cpu", cpu);
  setText("tel-ram", ram);
  setText("tel-disk", disk);
  byId("tel-cpu-line")?.setAttribute("points", sparkPoints(samples.cpu, 100));
  byId("tel-ram-line")?.setAttribute("points", sparkPoints(samples.ram, 100));
  // The disk line scales to at least 50 MB/s, so a few MB/s of idle writes stay near the floor.
  byId("tel-disk-line")?.setAttribute("points", sparkPoints(samples.disk, Math.max(50, ...samples.disk)));
}

/** Tells the core whether the window shows, so it samples no telemetry while hidden. */
function sendVisibility() {
  dispatchIntent({ type: "view_visible", visible: !document.hidden });
}

// ─── Directory Scan ─────────────────────────────────────────────────────────

const scanInputDirectory = createDirectoryScanner({
  dispatch: dispatchIntent,
  onSettled: () => syncControls(),
});

async function chooseInputDirectory(targetId) {
  if (busy()) return;
  const directory = await invoke("pick_directory");
  if (!directory || busy()) return;

  const input = document.getElementById(targetId);
  if (!input) return;
  input.value = directory;

  if (targetId === "remux-input-dir" || targetId === "extract-input-dir") {
    const mode = targetId.startsWith("extract") ? "extract" : "remux";
    // An input event, not a change: the field's own change rescans, and this scans below.
    input.dispatchEvent?.(new Event("input"));
    await scanInputDirectory(directory, mode);
  } else {
    // The change sends the edit to the Settings Draft too.
    input.dispatchEvent?.(new Event("change"));
  }
}

// ─── Log Drawer ─────────────────────────────────────────────────────────────

// Each mode's console draws the Run Log the core keeps, which a reload reads back with `get_log`.
const runLogs = createRunLogs({ doc: document, onErrors: () => renderLogBadge() });

function renderLogBadge() {
  const badge = byId("log-badge");
  if (!badge) return;
  const errors = runLogs.errors(currentView);
  badge.hidden = errors === 0;
  badge.textContent = String(errors);
}

function setLogOpen(open) {
  const drawer = byId("log-drawer");
  const toggle = byId("btn-log");
  // Hiding the drawer while focus is inside it would drop focus on <body>; hand it to the toggle.
  if (!open && drawer?.contains?.(document.activeElement)) toggle?.focus();
  if (drawer) drawer.hidden = !open;
  toggle?.setAttribute("aria-expanded", String(open));
}

async function copyLog() {
  const btn = byId("log-copy");
  let label = "Copied";
  try {
    // No await before this: WebKit may refuse a write that lost the click's user activation.
    await navigator.clipboard.writeText(runLogs.text(currentView));
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
// True when the last input inside the sheet was a pointer, so closing it with
// Done or a backdrop click hands focus back to the opener without a focus ring.
let sheetPointerInput = false;

/** Switches to `mode` and opens its settings sheet; closing it returns focus to `opener`. */
function openSettings(mode, opener) {
  switchView(mode);
  const sheet = byId("settings-sheet");
  if (!sheet || sheet.open) return;
  sheetOpener = opener ?? document.activeElement;
  sheetPointerInput = false;
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
    "Could not close safely",
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
  safeAddListener(sheet, "pointerdown", () => { sheetPointerInput = true; });
  safeAddListener(sheet, "keydown", () => { sheetPointerInput = false; });
  safeAddListener(sheet, "close", () => {
    const opener = sheetOpener;
    sheetOpener = null;
    if (!opener?.focus) return;
    // WebKit matches :focus-visible on this programmatic focus even after a
    // click, so mark the opener quiet until it loses focus.
    if (sheetPointerInput) {
      opener.classList.add("quiet-focus");
      opener.addEventListener("blur", () => opener.classList.remove("quiet-focus"), { once: true });
    }
    opener.focus();
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

  // ─── Presets and the Settings Draft ───────────────────────────────────────
  // Bound before the Job Controllers, so a field's edit is sent before the rescan its change starts.
  presets.bind();
  draftForm.bind();

  // ─── Complexity Slider & Number Synchronization ──────────────────────────
  // Still the view's (out of S4): the clamp and normalize are rules the core could own.
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
      if (cplxNumber.value === normalized && cplxSlider.value === normalized) return;
      cplxNumber.value = normalized;
      cplxSlider.value = normalized;
      draftForm.send("remux", "complexity", cplxNumber);
    };

    cplxSlider.addEventListener("input", syncFromSlider);
    cplxSlider.addEventListener("change", syncFromSlider);
    cplxNumber.addEventListener("input", syncFromNumber);
    cplxNumber.addEventListener("change", normalizeNumber);
    cplxNumber.addEventListener("blur", normalizeNumber);
  }

  const jobRuntime = {
    queueFor,
    safeAddListener,
    scanInputDirectory,
    send: sendIntent,
    showModal,
    state: () => appliedState,
    openSettings: mode => openSettings(mode),
  };
  controllers = {
    remux: new JobController("remux", jobRuntime),
    extract: new JobController("extract", jobRuntime),
  };
  for (const { id, mode, select } of BULK_SELECTION_CONTROLS) {
    safeAddListener(id, "click", () => controllers[mode].applyBulkSelection(mode, select));
  }
  syncControls();
  sendVisibility();
  document.addEventListener("visibilitychange", sendVisibility);
  boot();

  document.addEventListener("keydown", e => {
    if ((e.ctrlKey || e.metaKey) && e.key === "t") {
      e.preventDefault();
      theme.toggle();
    }
    // Esc closes the log drawer, unless an open dialog takes the key for itself.
    if (e.key === "Escape" && byId("log-drawer")?.hidden === false && !document.querySelector("dialog[open]")) {
      setLogOpen(false);
    }
  });
}

/**
 * Loads the queues, the Job Coordinator and any job from the Desktop Core, which outlives a
 * webview reload: listens to `view-state` and `job-log` first, so no publication after
 * `get_state` is missed, then renders the current state and each mode's Run Log.
 */
async function boot() {
  try {
    await listen(VIEW_STATE, event => receiveState(event.payload));
  } catch (err) {
    console.error("[view-state] listener failed:", err);
  }
  // Each Run Log batch names the console it goes to.
  try {
    await listen(JOB_LOG, event => runLogs.apply(event.payload));
  } catch (err) {
    console.error("[job-log] listener failed:", err);
  }
  // The boot's read is the resync a patch that arrives before it waits for, or none when a patch
  // that arrived while the listeners were bound already finished one.
  const state = appliedState ?? await resync();
  // The lines kept so far; a batch the answer already holds is not drawn twice.
  await Promise.all(["remux", "extract"].map(async mode => {
    let view = null;
    try {
      view = await invoke("get_log", { mode });
    } catch (err) {
      console.error("[get_log] failed:", err);
    }
    runLogs.load(mode, view);
  }));
  // An extract batch is shown in its own view.
  if ((state?.run?.mode ?? state?.job?.mode) === "extract") switchView("extract");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}
