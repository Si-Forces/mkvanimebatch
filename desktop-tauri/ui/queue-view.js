import {
  formatOutcomeDuration,
  matchesFilter,
  prettyPath,
  remuxAudioText,
  statusLabel,
  statusTone,
  subtitleLadder,
  subtitleSourceText,
} from "./presentation.js";

/** The status ring: its radius, and the circumference its dash offset is measured against. */
const RING_RADIUS = 8;
export const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

/** A mode's queue before the Desktop Core published one. */
const EMPTY_VIEW = {
  directory: null,
  scan: { state: "idle" },
  manifest_rev: 0,
  subtitle_candidates: {},
  rows: [],
  vanished_selected_ids: [],
  assignments: {},
};

/** Terminal item status -> the state counter it increments. */
const OUTCOME_COUNTERS = new Map([
  ["completed", "completedCount"],
  ["planned", "completedCount"],
  ["skipped", "skippedCount"],
  ["cancelled", "cancelledCount"],
  ["error", "errorCount"],
]);

/**
 * The labels a row shows for an outcome the engine gave no reason for. The Desktop Core keeps
 * only what the engine sent; until it supplies presentation values (S4), the view does.
 */
const DEFAULT_REASONS = { skipped: "Skipped", cancelled: "Cancelled" };
const DEFAULT_ERROR = "Failed";

/**
 * Returns true if both Directory Identities name the same directory. The Engine Bridge
 * produces every identity in display-canonical form, so the UI compares them byte for byte
 * and never normalizes a path itself. An empty, null, or missing identity is never the same.
 *
 * @param {string|null|undefined} dirA
 * @param {string|null|undefined} dirB
 * @returns {boolean}
 */
export function isSameDirectory(dirA, dirB) {
  return Boolean(dirA) && dirA === dirB;
}

/** One published Row as the renderer and `presentation.js` read an item. */
function projectRow(row) {
  return {
    id: row.id,
    name: row.name,
    release: row.release,
    // Every manifest row carries its inspection metadata; only a row the Job Snapshot lists
    // but the manifest does not has none.
    metadata: row.metadata ?? null,
    inspectionError: row.inspection_error ?? null,
    selected: row.selected,
    status: row.status,
    startTime: row.start_time_ms ?? null,
    durationSec: row.duration_sec ?? null,
    progress: typeof row.progress === "number" ? row.progress : null,
    reason: row.reason ?? DEFAULT_REASONS[row.status] ?? null,
    error: row.error ?? (row.status === "error" ? DEFAULT_ERROR : null),
  };
}

/**
 * Projects a published ModeView, and the Job View when it is this mode's, into the queue state
 * the renderer and `presentation.js` read. Rendering only: every queue rule lives in the core.
 */
function projectState(view, job) {
  const state = {
    items: new Map(),
    order: [],
    vanishedSelectedIds: view.vanished_selected_ids ?? [],
    batchStartTime: null,
    batchItemIds: null,
    batchCompleted: false,
    batchSuccess: null,
    batchCancelled: false,
    completedCount: 0,
    totalCount: 0,
    skippedCount: 0,
    cancelledCount: 0,
    errorCount: 0,
  };
  for (const row of view.rows ?? []) {
    state.items.set(row.id, projectRow(row));
    state.order.push(row.id);
    const counter = OUTCOME_COUNTERS.get(row.status);
    if (counter && row.participant) state[counter]++;
  }
  state.totalCount = state.order.length;
  if (job) {
    state.batchCompleted = job.status !== "running";
    state.batchSuccess = job.status === "completed";
    state.batchCancelled = job.status === "cancelled";
    // A job that ended before `BatchStart` ran no batch, so the queue keeps its selection count.
    if (job.participants.length > 0) {
      state.batchStartTime = job.start_time_ms;
      state.batchItemIds = job.participants;
    }
  }
  return state;
}

function episodeNumberText(epNum) {
  return String(epNum).padStart(2, "0");
}

/** Structural equality of two JSON-shaped values (an Inspection Manifest entry and its parts). */
function sameJsonValue(a, b) {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every(key => Object.hasOwn(b, key) && sameJsonValue(a[key], b[key]));
}

function sameSequence(a, b) {
  return a.length === b.length && a.every((id, index) => id === b[index]);
}

/** What a row renders from its manifest entry: a change to any of these remounts the row. */
function manifestPart(row) {
  return row && { name: row.name, release: row.release, metadata: row.metadata, inspection_error: row.inspection_error };
}

/** The meta line of a row the Job Snapshot lists but the manifest does not: it has no inspection metadata. */
function jobOnlyMetaText(item) {
  switch (item.status) {
    case "completed":
      return item.durationSec != null ? `Processed · Done in ${formatOutcomeDuration(item.durationSec)}` : "Processed";
    case "skipped":
      return `Skipped · ${item.reason || "Already processed"}`;
    case "cancelled":
      return `Cancelled · ${item.reason || "Cancelled"}`;
    case "error":
      return `Failed · ${item.error || "Error"}`;
    default:
      return "Queued";
  }
}

const CHECK_ICON = `<svg class="ep-icon ep-icon-ok" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>`;
const CROSS_ICON = `<svg class="ep-icon ep-icon-err" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>`;
const RING = `<svg class="ep-ring" width="20" height="20" viewBox="0 0 20 20" aria-hidden="true"><circle class="ep-ring-track" cx="10" cy="10" r="${RING_RADIUS}"/><circle class="ep-ring-arc" cx="10" cy="10" r="${RING_RADIUS}" transform="rotate(-90 10 10)" stroke-dasharray="${RING_CIRCUMFERENCE.toFixed(2)}"/></svg>`;
const CHEVRON = d => `<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${d}"/></svg>`;

/**
 * The Queue Tracker's view of one mode: renders the queue the Desktop Core publishes
 * (`applyView`) and turns the user's selection and order clicks into Intents through `dispatch`.
 * It holds no queue rules of its own.
 */
export class QueueTracker {
  /** @param {(intent: object) => Promise<unknown>} dispatch sends an Intent to the Desktop Core */
  constructor(mode, dispatch = async () => false) {
    this.mode = mode;
    this.dispatch = dispatch;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.filterEmptyEl = document.getElementById(`${mode}-filter-empty`);
    // The last ModeView applied, and its projection.
    this.view = EMPTY_VIEW;
    this.state = projectState(EMPTY_VIEW, null);
    this.cards = new Map();
    // What the list shows: "empty", "scanning", "failed", or "rows" (mounted or mounting).
    this.shown = null;
    this.emptyTitle = null;
    // The row IDs in the order they are mounted, once an incremental render completes.
    this.renderedOrder = [];
    // The order an incremental render is mounting, or null when none is pending.
    this.renderingOrder = null;
    this.renderRafId = null;
    this.currentDirectory = null;
    this.isScanning = false;
    // Set by the owning JobController. While true, every user selection or order change is
    // refused: `start()` captured the ordered selected IDs before its forced inspection. The
    // guard lives on the mutation, not only on the disabled controls, because a row re-rendered
    // after the last sync carries a fresh, enabled checkbox.
    this.selectionLocked = false;
    // Track Assignments (remux only): item id -> assignment payload, as published.
    this.assignments = new Map();
    this.assignmentsKey = "{}";
    // Subtitle candidates, once per Release Directory.
    this.subtitleCandidates = {};
    this.heartbeatTimer = null;
    // Presentation only: the list filter chip, the row shown in the inspector, and the
    // release group headers currently mounted between rows.
    this.filter = "all";
    this.inspectedId = null;
    this.markedId = null;
    this.groupHeads = [];
    // Called after anything the surrounding chrome (inspector, library, bottom bar) shows changed.
    this.onChange = null;
    if (this.listEl) {
      this.listEl.addEventListener("change", e => {
        const check = e.target.closest(".ep-select-check");
        const id = check?.dataset.itemId;
        if (!id) return;
        if (this.selectionLocked) {
          // Put the DOM back in step with the state the request was built from.
          const item = this.state.items.get(id);
          check.checked = item ? item.selected !== false : false;
          return;
        }
        this.dispatch({ type: "set_selected", mode: this.mode, id, selected: check.checked });
      });

      this.listEl.addEventListener("click", e => {
        const pick = e.target.closest(".ep-pick");
        if (pick?.dataset.itemId) {
          this.inspect(pick.dataset.itemId);
          return;
        }
        const btn = e.target.closest(".ep-reorder-btn");
        if (!btn?.dataset.itemId) return;
        this.reorderCard(btn.dataset.itemId, btn.classList.contains("ep-up-btn") ? "up" : "down");
      });
    }
  }

  cancelRender() {
    if (this.renderRafId) {
      if (typeof cancelAnimationFrame === "function") {
        cancelAnimationFrame(this.renderRafId);
      }
      this.renderRafId = null;
    }
    this.renderingOrder = null;
  }

  startHeartbeat() {
    if (this.heartbeatTimer !== null) return;
    this.heartbeatTimer = setInterval(() => this.tickHeartbeat(), 1000);
    // Node timers keep the process (and test runner) alive unless unref'd.
    this.heartbeatTimer.unref?.();
  }

  stopHeartbeat() {
    if (this.heartbeatTimer !== null) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  hasProcessingItems() {
    for (const item of this.state.items.values()) {
      if (item.status === "processing") return true;
    }
    return false;
  }

  // The Presentation Heartbeat: one timer for every processing row, and the bottom bar's clock
  // through `onChange`.
  tickHeartbeat() {
    let hasProcessing = false;
    for (const [id, card] of this.cards) {
      const item = this.state.items.get(id);
      if (item?.status !== "processing") continue;
      hasProcessing = true;
      card.statusTag.textContent = statusLabel(item);
    }
    if (!hasProcessing && !this.hasProcessingItems()) {
      this.stopHeartbeat();
    }
    this.onChange?.();
  }

  candidatesForItem(itemId) {
    const item = this.state.items.get(itemId);
    if (!item?.release) return [];
    return this.subtitleCandidates[item.release] ?? [];
  }

  /** The subtitle precedence ladder of one item, from the manifest, its assignment and the assets dir. */
  ladderFor(itemId) {
    const item = this.state.items.get(itemId);
    return subtitleLadder({
      release: item?.release,
      candidates: this.candidatesForItem(itemId),
      embedded: item?.metadata?.sub_summary ?? null,
      assignment: this.assignments.get(itemId) ?? null,
      assetsDir: document.getElementById(`${this.mode}-assets-dir`)?.value?.trim() ?? "",
    });
  }

  emptyStateMarkup(title, description, actionLabel = "Open folder…") {
    return `
      <div class="empty-state-card">
        <svg class="empty-icon" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>
        <div class="empty-state-title">${title}</div>
        <div class="empty-state-desc">${description}</div>
        <button class="btn btn-accent empty-state-action" type="button" data-target="${this.mode}-input-dir">${actionLabel}</button>
      </div>`;
  }

  /**
   * Renders the ModeView the Desktop Core published for this mode, with `job`, the Job View,
   * laid over it only when the job is this mode's.
   *
   * @param {object} view a ModeView
   * @param {object|null} job the Job View, or null
   */
  applyView(view, job = null) {
    const previous = this.view;
    this.view = view;
    this.state = projectState(view, job?.mode === this.mode ? job : null);
    this.currentDirectory = view.directory ?? null;
    this.subtitleCandidates = view.subtitle_candidates ?? {};
    const assignmentsKey = JSON.stringify(view.assignments ?? {});
    const assignmentsChanged = assignmentsKey !== this.assignmentsKey;
    this.assignmentsKey = assignmentsKey;
    this.assignments = new Map(Object.entries(view.assignments ?? {}));
    this.isScanning = view.scan?.state === "scanning";

    if (this.isScanning) this.showScanning(previous, view.scan.directory);
    else if (view.scan?.state === "failed") this.showFailure();
    else if (this.state.order.length === 0) this.showEmpty();
    else this.showRows(previous, assignmentsChanged);
    // A view with nothing processing ends the heartbeat at once, as a finished job's does.
    if (!this.hasProcessingItems()) this.stopHeartbeat();
    this.onChange?.();
  }

  /** Replaces the list with `markup`, dropping its cards and headers and ending any render and heartbeat. */
  clearList(markup) {
    this.stopHeartbeat();
    this.cancelRender();
    this.cards.clear();
    this.groupHeads = [];
    this.renderedOrder = [];
    if (this.listEl) this.listEl.innerHTML = markup;
  }

  showScanning(previous, directory) {
    this.stopHeartbeat();
    this.setCountText("Scanning…");
    // Re-inspecting the directory already shown (every Run forces one) leaves its fully rendered
    // rows on screen, so the accepted manifest can reuse the unchanged ones instead of flashing
    // the list empty.
    const keepCards = this.shown === "rows" && this.cards.size > 0 && this.renderingOrder === null
      && isSameDirectory(previous.directory, directory);
    if (keepCards || this.shown === "scanning") return;
    this.shown = "scanning";
    this.clearList(`
          <div class="empty-state-card scanning-state">
            <div class="scan-spinner" aria-hidden="true"></div>
            <div class="empty-state-title">Scanning folder</div>
            <div class="empty-state-desc">Inspecting MKV files and reading track metadata…</div>
          </div>`);
  }

  showFailure() {
    this.setCountText("Scan failed");
    if (this.shown === "failed") return;
    this.shown = "failed";
    // A scan that kept its rows must not leave them registered once the failure markup replaces them.
    this.clearList(this.emptyStateMarkup(
      "Scan failed",
      "That folder could not be inspected. Choose another folder and try again.",
      "Choose another folder…"
    ));
  }

  /** No rows: an empty folder when a directory is shown, else no folder yet. */
  showEmpty() {
    const [title, description] = this.currentDirectory
      ? ["No MKV files found", "This folder holds no MKV episodes. Open another release folder to continue."]
      : ["No episodes yet", "Open a release folder to scan its MKV episodes into this batch."];
    this.setCountText("Ready");
    if (this.shown === "empty" && this.emptyTitle === title) return;
    this.shown = "empty";
    this.emptyTitle = title;
    this.clearList(this.emptyStateMarkup(title, description));
    if (this.filterEmptyEl) this.filterEmptyEl.hidden = true;
  }

  /**
   * Brings the mounted rows in step with the published ones. A new manifest of the directory
   * shown, with the same IDs in the same order, updates the rows in place; the same rows in
   * another order are moved; any other row set rebuilds the list.
   */
  showRows(previous, assignmentsChanged) {
    const { order } = this.state;
    const wasRows = this.shown === "rows";
    this.shown = "rows";
    if (!wasRows) {
      this.renderCardsIncrementally();
    } else if (this.renderingOrder !== null) {
      // A pending render mounts each row from the current state, so only another row set or
      // another manifest restarts it; otherwise the rows already mounted are redrawn.
      if (sameSequence(this.renderingOrder, order) && previous.manifest_rev === this.view.manifest_rev) {
        this.updateCards(assignmentsChanged);
      }
      else this.renderCardsIncrementally();
    } else if (previous.manifest_rev !== this.view.manifest_rev) {
      if (isSameDirectory(previous.directory, this.view.directory) && sameSequence(this.renderedOrder, order)) {
        this.refreshCards(previous.rows);
      } else {
        this.renderCardsIncrementally();
      }
    } else if (sameSequence(this.renderedOrder, order)) {
      this.updateCards(assignmentsChanged);
    } else if (this.renderedOrder.length === order.length && order.every(id => this.cards.has(id))) {
      this.moveCards();
      this.updateCards(assignmentsChanged);
    } else {
      this.renderCardsIncrementally();
    }
    this.renderQueueCount();
  }

  /** Redraws each mounted row's selection and status, and its meta line when the assignments changed. */
  updateCards(assignmentsChanged) {
    let changed = false;
    for (const [id, card] of this.cards) {
      const item = this.state.items.get(id);
      if (!item) continue;
      const selected = item.selected !== false;
      if (card.checkbox.checked !== selected || card.renderedStatus !== item.status) changed = true;
      card.checkbox.checked = selected;
      if (assignmentsChanged) this.applyCardMeta(card, item, card.bitrate ?? this.getBitrate());
      this.renderCardState(card, item);
    }
    // The filter matches on selection and status, so only a change there redraws the chrome.
    if (changed && this.filter !== "all") this.applyListChrome();
  }

  /** Puts the mounted rows in the published order. */
  moveCards() {
    // Headers sit between rows; take them out so the rows line up.
    this.clearGroupHeads();
    this.listEl?.append(...this.state.order.map(id => this.cards.get(id).el));
    this.renderedOrder = [...this.state.order];
    this.updateEpisodeNumbers();
    this.applyListChrome();
  }

  // Updates the mounted rows in place for a same-directory manifest with the same IDs in the same
  // order. A row keeps its element when its manifest part is deep-equal to the previous one and it
  // was rendered at the current bitrate, since those are everything mountCard renders from besides
  // the selection, assignment and status redrawn here. Any other row is remounted in place.
  refreshCards(previousRows) {
    const previousById = new Map(previousRows.map(row => [row.id, row]));
    const rowsById = new Map(this.view.rows.map(row => [row.id, row]));
    const bitrate = this.getBitrate();
    this.state.order.forEach((id, index) => {
      const item = this.state.items.get(id);
      const card = this.cards.get(id);
      if (card.bitrate === bitrate && sameJsonValue(manifestPart(previousById.get(id)), manifestPart(rowsById.get(id)))) {
        card.checkbox.checked = item.selected !== false;
        this.applyCardMeta(card, item, bitrate);
        this.renderCardState(card, item);
        return;
      }
      const fresh = this.mountCard(item, index + 1, bitrate);
      card.el.replaceWith(fresh.el);
    });
    this.applyListChrome();
  }

  /** The bitrate control's value, or "" without one: the row never assumes a default. */
  getBitrate() {
    return document.getElementById(`${this.mode}-bitrate`)?.value?.trim() ?? "";
  }

  // Creates rows in requestAnimationFrame batches under a ~8 ms frame budget so a large manifest
  // does not block the main thread. A tick aborts once another render or a cleared list took
  // over. Rows render from the item's current state, so a view applied before a row exists
  // shows once it is created.
  renderCardsIncrementally() {
    this.cancelRender();
    const order = this.state.order;
    this.cards.clear();
    this.groupHeads = [];
    this.renderedOrder = [];
    if (!this.listEl || order.length === 0) return;

    const bitrate = this.getBitrate();
    this.listEl.innerHTML = "";
    this.renderingOrder = order;
    let index = 0;

    const renderBatch = () => {
      this.renderRafId = null;
      if (this.renderingOrder !== order) return;
      const frag = document.createDocumentFragment();
      const deadline = performance.now() + 8;
      do {
        const item = this.state.items.get(order[index]);
        index++;
        if (!item) continue;
        frag.appendChild(this.mountCard(item, index, bitrate).el);
      } while (index < order.length && performance.now() < deadline);
      this.listEl.appendChild(frag);
      if (index < order.length) {
        this.renderRafId = requestAnimationFrame(renderBatch);
      } else {
        this.renderingOrder = null;
        this.renderedOrder = [...order];
        this.applyListChrome();
        this.onChange?.();
      }
    };

    this.renderRafId = requestAnimationFrame(renderBatch);
  }

  /** Create and register a row for `item` with its meta line and current state drawn; the caller attaches it. */
  mountCard(item, epNum, bitrate) {
    const card = this.createCardElement(item, epNum);
    card.bitrate = bitrate;
    this.cards.set(item.id, card);
    this.applyCardMeta(card, item, bitrate);
    this.renderCardState(card, item);
    if (item.id === this.markedId) card.el.classList.add("is-inspected");
    return card;
  }

  applyCardMeta(card, item, bitrate) {
    // A failed row says why, so a batch with several failures reads without opening each row.
    card.showsFailure = item.status === "error";
    if (card.showsFailure) {
      card.meta.className = "ep-meta is-error";
      card.meta.textContent = `Failed · ${item.error || "Error"}`;
    } else if (item.inspectionError) {
      this.applyInspectionError(card, item.inspectionError);
    } else if (item.metadata) {
      this.applyMetadata(card, item.metadata, bitrate, item.id);
    } else {
      this.applyJobOnlyMeta(card, item);
    }
  }

  applyMetadata(card, meta, bitrate, id) {
    card.meta.className = "ep-meta";
    if (this.mode === "remux") {
      const subs = subtitleSourceText(this.ladderFor(id));
      card.meta.textContent = `${remuxAudioText(meta, bitrate)} · ${subs} · ${meta.fonts_count || 0} fonts`;
    } else {
      card.meta.textContent = `${meta.sub_summary || "No subs"} · ${meta.fonts_count || 0} fonts`;
    }
  }

  applyInspectionError(card, error) {
    card.meta.className = "ep-meta is-error";
    card.meta.textContent = `Inspection failed: ${error}`;
  }

  applyJobOnlyMeta(card, item) {
    card.meta.className = "ep-meta";
    card.meta.textContent = jobOnlyMetaText(item);
  }

  /** Redraws every row's meta line, after the bitrate or the assets dir changed. */
  refreshMeta() {
    const bitrate = this.getBitrate();
    for (const [id, card] of this.cards) {
      const item = this.state.items.get(id);
      if (!item) continue;
      card.bitrate = bitrate;
      this.applyCardMeta(card, item, bitrate);
    }
    this.onChange?.();
  }

  createCardElement(item, epNum) {
    const el = document.createElement("div");
    el.className = "ep-row";
    el.innerHTML = `
      <input type="checkbox" class="ep-select-check" />
      <button type="button" class="ep-pick">
        <span class="ep-num"></span>
        <span class="ep-text"><b class="ep-title"></b><small class="ep-meta"></small></span>
      </button>
      <span class="ep-status">${RING}${CHECK_ICON}${CROSS_ICON}<span class="ep-status-text status-tag"></span></span>
      <span class="ep-reorder">
        <button type="button" class="ep-reorder-btn ep-up-btn" title="Move up" aria-label="Move up">${CHEVRON("M6 15l6-6 6 6")}</button>
        <button type="button" class="ep-reorder-btn ep-down-btn" title="Move down" aria-label="Move down">${CHEVRON("M6 9l6 6 6-6")}</button>
      </span>
    `;

    const card = {
      el,
      checkbox: el.querySelector(".ep-select-check"),
      pick: el.querySelector(".ep-pick"),
      num: el.querySelector(".ep-num"),
      title: el.querySelector(".ep-title"),
      meta: el.querySelector(".ep-meta"),
      status: el.querySelector(".ep-status"),
      statusTag: el.querySelector(".status-tag"),
      arc: el.querySelector(".ep-ring-arc"),
      epNum,
      // The status renderCardState last drew. None yet: mountCard draws the first status,
      // so the queued look is drawn only in renderCardState.
      renderedStatus: null,
      // Whether the meta line shows the failure rather than the inspection (applyCardMeta).
      showsFailure: false,
    };
    card.title.textContent = item.name;
    card.num.textContent = episodeNumberText(epNum);
    card.checkbox.checked = item.selected !== false;
    card.checkbox.setAttribute?.("aria-label", `Include ${item.name} in the batch`);
    for (const control of [card.checkbox, card.pick, el.querySelector(".ep-up-btn"), el.querySelector(".ep-down-btn")]) {
      if (control) control.dataset.itemId = item.id;
    }
    return card;
  }

  /** The item the inspector shows: the one picked, or else the first in order. */
  inspectedItem() {
    return this.state.items.get(this.inspectedId) ?? this.state.items.get(this.state.order[0]) ?? null;
  }

  inspect(id) {
    if (!this.state.items.has(id)) return;
    this.inspectedId = id;
    this.markInspected();
    this.onChange?.();
  }

  /** Highlights the row the inspector shows, the first row included before any pick. */
  markInspected() {
    const id = this.inspectedItem()?.id ?? null;
    if (id === this.markedId) return;
    this.cards.get(this.markedId)?.el.classList.remove("is-inspected");
    this.markedId = id;
    this.cards.get(id)?.el.classList.add("is-inspected");
  }

  /** How many queued items each release directory holds, in queue order. */
  releaseCounts() {
    const counts = new Map();
    for (const id of this.state.order) {
      const release = this.state.items.get(id)?.release || ".";
      counts.set(release, (counts.get(release) ?? 0) + 1);
    }
    return counts;
  }

  /** Scrolls to a release's header (or its first row), or to the top for no release. */
  revealRelease(release) {
    if (!this.listEl) return;
    if (!release) {
      this.listEl.scrollTop = 0;
      return;
    }
    this.setFilter("all");
    const target = this.groupHeads.find(head => head.dataset.release === release)
      ?? this.cards.get(this.state.order.find(id => this.state.items.get(id)?.release === release))?.el;
    target?.scrollIntoView?.({ block: "start" });
  }

  setFilter(filter) {
    this.filter = filter;
    this.applyListChrome();
    this.onChange?.();
  }

  /**
   * Shows the rows the filter chip matches and puts a sticky header before each run of rows from
   * one release directory, when the queue spans more than one. Rows stay direct children of the
   * list in queue order, so reordering and incremental rendering never see a header.
   */
  applyListChrome() {
    if (!this.listEl || this.renderingOrder !== null) return;
    this.markInspected();
    this.clearGroupHeads();
    const filtering = this.filter !== "all";
    if (filtering) this.listEl.classList.add("is-filtered");
    else this.listEl.classList.remove("is-filtered");

    const shown = this.state.order.filter(id => {
      const card = this.cards.get(id);
      const item = this.state.items.get(id);
      if (!card || !item) return false;
      const match = matchesFilter(item, this.filter);
      card.el.hidden = !match;
      return match;
    });
    if (this.filterEmptyEl) this.filterEmptyEl.hidden = !(filtering && shown.length === 0 && this.cards.size > 0);

    const releaseOf = id => this.state.items.get(id).release;
    if (new Set(this.state.order.map(id => this.state.items.get(id)?.release)).size < 2) return;
    for (let i = 0; i < shown.length; i++) {
      const release = releaseOf(shown[i]);
      if (i > 0 && releaseOf(shown[i - 1]) === release) continue;
      let count = 0;
      while (i + count < shown.length && releaseOf(shown[i + count]) === release) count++;
      const head = document.createElement("div");
      head.className = "ep-group-head";
      head.dataset.release = release;
      const name = document.createElement("span");
      name.textContent = prettyPath(release) || prettyPath(this.currentDirectory) || "Episodes";
      const total = document.createElement("span");
      total.textContent = String(count);
      head.append(name, total);
      this.cards.get(shown[i]).el.before(head);
      this.groupHeads.push(head);
    }
  }

  clearGroupHeads() {
    for (const head of this.groupHeads) head.remove();
    this.groupHeads = [];
  }

  // Classes are written only when the status differs from the one the row last
  // drew, so the stream of progress events on a processing row writes just the
  // ring and the status text. The heartbeat is not stopped here: tickHeartbeat
  // stops itself once nothing is processing.
  renderCardState(card, item) {
    const statusChanged = card.renderedStatus !== item.status;
    card.renderedStatus = item.status;
    // Entering or leaving "error" swaps the meta line between the failure and the inspection.
    if (statusChanged && (item.status === "error" || card.showsFailure)) {
      this.applyCardMeta(card, item, card.bitrate ?? this.getBitrate());
    }

    if (item.status === "processing") {
      if (statusChanged) {
        card.el.dataset.status = "processing";
        card.status.title = "";
      }
      // Card Progress: a spinning ring until the first progress value, then a determinate one.
      const determinate = typeof item.progress === "number";
      const statusClass = determinate ? "ep-status tone-info" : "ep-status tone-info is-spinning";
      if (card.status.className !== statusClass) card.status.className = statusClass;
      card.arc.style.strokeDashoffset = determinate
        ? (RING_CIRCUMFERENCE * (1 - item.progress / 100)).toFixed(2)
        : "";
      card.statusTag.textContent = statusLabel(item);
      this.startHeartbeat();
      return;
    }
    if (!statusChanged) return;

    card.el.dataset.status = item.status;
    card.status.className = `ep-status tone-${statusTone(item.status)}`;
    card.status.title = item.error || item.reason || "";
    card.arc.style.strokeDashoffset = "";
    card.statusTag.textContent = statusLabel(item);
  }

  setCountText(text) {
    if (!this.countEl || this.countEl.textContent === text) return;
    this.countEl.textContent = text;
    this.countEl.title = text;
  }

  updateSelectionBadge() {
    const total = this.state.totalCount;
    this.setCountText(total === 0 ? "Ready" : `${this.getSelectedCount()} / ${total} Selected`);
  }

  getSelectedCount() {
    let count = 0;
    for (const item of this.state.items.values()) {
      if (item.selected !== false) count++;
    }
    return count;
  }

  /** The IDs of the manifest rows, in queue order: what the last accepted inspection found. */
  manifestItemIds() {
    return this.state.order.filter(id => this.state.items.get(id).metadata !== null);
  }

  /** The Queue Selection: the selected manifest rows, in queue order. */
  getSelectedOrderedItemIds() {
    return this.manifestItemIds().filter(id => this.state.items.get(id).selected !== false);
  }

  selectAll() {
    this.bulkSelect(true);
  }

  deselectAll() {
    this.bulkSelect(false);
  }

  bulkSelect(selected) {
    if (this.selectionLocked) return;
    this.dispatch({ type: "set_all_selected", mode: this.mode, selected });
  }

  reorderCard(id, direction) {
    if (this.selectionLocked) return;
    this.dispatch({ type: "reorder", mode: this.mode, id, direction });
  }

  updateEpisodeNumbers() {
    this.state.order.forEach((id, idx) => {
      const card = this.cards.get(id);
      if (!card) return;
      card.epNum = idx + 1;
      card.num.textContent = episodeNumberText(card.epNum);
    });
  }

  renderQueueCount() {
    if (!this.countEl) return;
    // A job that ended before `BatchStart` ran no batch, so it leaves the selection count.
    if (this.state.batchStartTime === null) {
      this.updateSelectionBadge();
      return;
    }
    const { totalCount, completedCount, skippedCount, cancelledCount, errorCount, batchItemIds } = this.state;
    const total = batchItemIds?.length ?? totalCount;
    // "6 of 12 · 5 done · 1 failed": settled items, then each outcome that occurred. The batch
    // clock beside it says whether the batch is still running.
    const settled = completedCount + skippedCount + cancelledCount + errorCount;
    const parts = [`${completedCount} done`];
    if (skippedCount > 0) parts.push(`${skippedCount} skipped`);
    if (cancelledCount > 0) parts.push(`${cancelledCount} cancelled`);
    if (errorCount > 0) parts.push(`${errorCount} failed`);
    this.setCountText(`${settled} of ${total} · ${parts.join(" · ")}`);
  }
}
