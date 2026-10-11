import {
  carryOverAssignments,
  createQueueState,
  createQueueStateFromSnapshot,
  formatOutcomeDuration,
  getSelectedCount,
  guardUnsettledItems,
  isSameDirectory,
  reduceQueueEvent,
  reconcileQueueSelection,
  reorderItem,
  setAllSelected,
  setItemSelected,
} from "./queue-state.js";
import {
  matchesFilter,
  prettyPath,
  remuxAudioText,
  statusLabel,
  statusTone,
  subtitleLadder,
  subtitleSourceText,
} from "./presentation.js";

const EMPTY_MANIFEST = { version: 2, items: [] };

/** The status ring: its radius, and the circumference its dash offset is measured against. */
const RING_RADIUS = 8;
export const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

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

/** The meta line of a row restored from a JobSnapshot, which carries no inspection metadata. */
function restoredMetaText(item) {
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

export class QueueTracker {
  constructor(mode) {
    this.mode = mode;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.filterEmptyEl = document.getElementById(`${mode}-filter-empty`);
    this.state = createQueueState(EMPTY_MANIFEST);
    this.cards = new Map();
    this.scanToken = 0;
    this.scanRequest = 0;
    this.pendingRenderToken = null;
    this.currentDirectory = null;
    this.lastScannedPath = null;
    this.isScanning = false;
    this.renderRafId = null;
    this.lastManifest = null;
    // Set by the owning JobController. While true, every user-driven selection
    // or order mutation is refused: `start()` snapshots the ordered selected IDs
    // before its forced inspection, so a click landing afterwards would change
    // what the user believes they are running without changing the request.
    // Disabling the controls is not the invariant — a row re-rendered after
    // the last sync carries a fresh, enabled checkbox — so the guard lives on
    // the mutation itself.
    this.selectionLocked = false;
    // Track Assignment overrides (remux only): item id -> assignment payload
    this.assignments = new Map();
    // v2 manifest: candidates are stored once per release directory.
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
        setItemSelected(this.state, id, check.checked);
        this.updateSelectionBadge();
        if (this.filter !== "all") this.applyListChrome();
        this.onSelectionChange?.();
        this.onChange?.();
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
    this.pendingRenderToken = null;
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

  startScan(directory) {
    this.stopHeartbeat();
    this.isScanning = true;
    // Re-inspecting the directory already shown (every Run forces one) leaves
    // its fully rendered rows on screen; populateFromInspection then reuses
    // the unchanged ones instead of flashing the list empty.
    const keepCards = this.cards.size > 0 && this.pendingRenderToken === null && isSameDirectory(this.currentDirectory, directory);
    this.cancelRender();
    const token = ++this.scanToken;
    if (!keepCards) {
      this.cards.clear();
      this.groupHeads = [];
      if (this.listEl) {
        this.listEl.innerHTML = `
          <div class="empty-state-card scanning-state">
            <div class="scan-spinner" aria-hidden="true"></div>
            <div class="empty-state-title">Scanning folder</div>
            <div class="empty-state-desc">Inspecting MKV files and reading track metadata…</div>
          </div>`;
      }
    }
    this.setCountText("Scanning…");
    this.onChange?.();
    return token;
  }

  isCurrentScan(token) {
    return token === this.scanToken;
  }

  // A scan request begins when the user asks for a scan, before `canonical_directory` has
  // answered. The newest request of a queue wins, whatever order the answers come back in:
  // an older one must not start a scan, as a later `startScan` supersedes an earlier one.
  // `reset()` leaves the counter alone, so the request an empty path begins still cancels
  // one pending in this queue.
  beginScanRequest() {
    return ++this.scanRequest;
  }

  isNewestScanRequest(request) {
    return request === this.scanRequest;
  }

  setDirectory(directory) {
    this.currentDirectory = directory;
    this.lastScannedPath = directory;
  }

  reuseInspection(directory) {
    if (!this.lastManifest || !isSameDirectory(this.lastScannedPath, directory)) return false;
    this.scanToken++;
    this.populateFromInspection(this.lastManifest, true);
    return true;
  }

  acceptInspection(token, directory, manifest) {
    if (!this.isCurrentScan(token)) return false;
    if (manifest?.version !== 2 || !Array.isArray(manifest.items)) {
      this.failScan();
      return false;
    }
    this.populateFromInspection(manifest, isSameDirectory(this.currentDirectory, directory));
    this.setDirectory(directory);
    this.onChange?.();
    return true;
  }

  rejectInspection(token) {
    if (this.isCurrentScan(token)) this.failScan();
  }

  failScan() {
    this.isScanning = false;
    this.lastScannedPath = null;
    // A scan that kept its rows must not leave them registered once the
    // failure markup replaces them.
    this.cards.clear();
    this.groupHeads = [];
    if (this.listEl) {
      this.listEl.innerHTML = this.emptyStateMarkup(
        "Scan failed",
        "That folder could not be inspected. Choose another folder and try again.",
        "Choose another folder…"
      );
    }
    this.setCountText("Scan failed");
    this.onChange?.();
  }

  reset(title = "No episodes yet", description = "Open a release folder to scan its MKV episodes into this batch.") {
    this.stopHeartbeat();
    this.isScanning = false;
    this.cancelRender();
    this.scanToken++;
    this.cards.clear();
    this.groupHeads = [];
    this.assignments.clear();
    this.subtitleCandidates = {};
    this.currentDirectory = null;
    this.lastScannedPath = null;
    this.state = createQueueState(EMPTY_MANIFEST);
    if (this.listEl) this.listEl.innerHTML = this.emptyStateMarkup(title, description);
    if (this.filterEmptyEl) this.filterEmptyEl.hidden = true;
    this.setCountText("Ready");
    this.onChange?.();
  }

  populateFromInspection(manifest, preserveSelection = false) {
    this.stopHeartbeat();
    this.isScanning = false;
    // Read before cancelRender clears it: a half-rendered list is never reused.
    const cardsComplete = this.pendingRenderToken === null;
    this.cancelRender();
    const previousManifest = this.lastManifest;
    const previousOrder = this.state.order;
    this.lastManifest = manifest;
    const nextState = createQueueState(manifest);
    this.state = preserveSelection ? reconcileQueueSelection(nextState, this.state) : nextState;

    // A different input directory starts a new batch; only a same-directory
    // rescan carries assignments for IDs that are still present.
    this.assignments = preserveSelection ? carryOverAssignments(this.assignments, manifest) : new Map();
    this.subtitleCandidates = manifest.subtitle_candidates ?? {};

    if (manifest.items.length === 0) {
      const directory = this.currentDirectory || this.lastScannedPath;
      const { vanishedSelectedIds, knownOrder } = this.state;
      this.reset(
        "No MKV files found",
        "This folder holds no MKV episodes. Open another release folder to continue."
      );
      if (preserveSelection) {
        if (directory) this.setDirectory(directory);
        this.state.vanishedSelectedIds = vanishedSelectedIds;
        this.state.knownOrder = knownOrder;
      }
      return;
    }

    this.updateSelectionBadge();
    if (preserveSelection && cardsComplete && previousManifest && this.cardsMatchOrder(previousOrder)) {
      this.refreshCards(previousManifest, manifest);
    } else {
      this.cards.clear();
      this.renderCardsIncrementally();
    }
    this.onChange?.();
  }

  /** True when the mounted rows are exactly `previousOrder` and the new state keeps that order. */
  cardsMatchOrder(previousOrder) {
    const { order } = this.state;
    return order.length > 0
      && this.cards.size === order.length
      && previousOrder.length === order.length
      && order.every((id, index) => id === previousOrder[index] && this.cards.has(id));
  }

  // Updates the mounted rows in place for a same-directory manifest with the
  // same IDs in the same order. A row keeps its element when its manifest
  // entry is deep-equal to the previous one and it was rendered at the current
  // bitrate, since those are everything mountCard renders from besides the
  // selection and status re-applied here (a surviving ID keeps its assignment
  // unchanged, so its meta line is already current). Any other row is remounted
  // in place.
  refreshCards(previousManifest, manifest) {
    const entriesById = items => new Map(items.filter(entry => entry?.id).map(entry => [String(entry.id), entry]));
    const previousEntries = entriesById(previousManifest.items);
    const entries = entriesById(manifest.items);
    const bitrate = this.getBitrate();
    this.state.order.forEach((id, index) => {
      const item = this.state.items.get(id);
      const card = this.cards.get(id);
      if (card.bitrate === bitrate && sameJsonValue(previousEntries.get(id), entries.get(id))) {
        card.checkbox.checked = item.selected !== false;
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

  // Creates row DOM in requestAnimationFrame batches under a ~8 ms frame
  // budget so large manifests don't block the main thread. Each tick aborts
  // when scanToken has moved on (reset/startScan/reuseInspection bump it).
  // Rows render from the item's CURRENT reducer state, so events that
  // arrived before a row existed are reflected once it is created.
  renderCardsIncrementally() {
    this.cancelRender();
    const order = this.state.order;
    if (!this.listEl || order.length === 0) return;

    const token = this.scanToken;
    const bitrate = this.getBitrate();
    this.cards.clear();
    this.groupHeads = [];
    this.listEl.innerHTML = "";
    this.pendingRenderToken = token;
    let index = 0;

    const renderBatch = () => {
      this.renderRafId = null;
      if (!this.isCurrentScan(token)) {
        this.pendingRenderToken = null;
        return;
      }
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
        this.pendingRenderToken = null;
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
    if (item.inspectionError) {
      this.applyInspectionError(card, item.inspectionError);
    } else if (item.metadata) {
      this.applyMetadata(card, item.metadata, bitrate, item.id);
    } else {
      this.applyRestoredMeta(card, item);
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

  applyRestoredMeta(card, item) {
    card.meta.className = "ep-meta";
    card.meta.textContent = restoredMetaText(item);
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

  /** Redraws the meta line of `id`'s row after its Track Assignment changed. */
  applyAssignmentToCard(id) {
    const card = this.cards.get(id);
    const item = this.state.items.get(id);
    if (card && item) this.applyCardMeta(card, item, card.bitrate ?? this.getBitrate());
    this.onChange?.();
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
    if (!this.listEl || this.pendingRenderToken !== null) return;
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

  restoreFromSnapshot(snapshot) {
    this.stopHeartbeat();
    this.isScanning = false;
    this.cancelRender();
    this.scanToken++;
    this.cards.clear();
    this.groupHeads = [];
    this.lastManifest = null;
    if (this.listEl) this.listEl.innerHTML = "";

    this.state = createQueueStateFromSnapshot(snapshot);
    if (snapshot.input_dir) this.setDirectory(snapshot.input_dir);
    this.renderQueueCount();
    this.renderCardsIncrementally();
    this.onChange?.();
  }

  handleEvent(event) {
    this.state = reduceQueueEvent(this.state, event);

    if (event.type === "batch_start") {
      if (event.input_dir) this.setDirectory(event.input_dir);
      if (this.cards.size === 0 && this.pendingRenderToken === null && this.state.order.length > 0) {
        this.renderCardsIncrementally();
      }
    }

    const item = this.state.items.get(event.id);
    if (item) {
      const card = this.cards.get(item.id);
      // A missing row is mounted here unless an incremental render is pending, which will
      // mount it from current state.
      if (card) {
        this.renderCardState(card, item);
        if (this.filter !== "all" && event.type !== "item_progress") this.applyListChrome();
      } else if (this.pendingRenderToken !== this.scanToken) {
        const mounted = this.mountCard(item, this.cards.size + 1, this.getBitrate());
        if (this.listEl) {
          this.listEl.querySelector(".empty-state-card")?.remove();
          this.listEl.appendChild(mounted.el);
        }
        this.applyListChrome();
      }
    }

    this.renderQueueCount();
    this.onChange?.();
  }

  // Classes are written only when the status differs from the one the row last
  // drew, so the stream of progress events on a processing row writes just the
  // ring and the status text. The heartbeat is not stopped here: tickHeartbeat
  // stops itself once nothing is processing.
  renderCardState(card, item) {
    const statusChanged = card.renderedStatus !== item.status;
    card.renderedStatus = item.status;

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

  /**
   * Ends the batch's presentation on `job-done`. The Engine Bridge has settled every item, so
   * the Item Settlement guard only acts on a run that broke that protocol.
   *
   * @param {boolean} cancelled whether the job was stopped
   */
  endBatch(cancelled) {
    this.stopHeartbeat();
    guardUnsettledItems(this.state, cancelled);
    for (const [id, card] of this.cards) {
      this.renderCardState(card, this.state.items.get(id));
    }
    this.applyListChrome();
    this.renderQueueCount();
    this.onChange?.();
  }

  setCountText(text) {
    if (!this.countEl || this.countEl.textContent === text) return;
    this.countEl.textContent = text;
    this.countEl.title = text;
  }

  updateSelectionBadge() {
    const total = this.state.totalCount;
    this.setCountText(total === 0 ? "Ready" : `${getSelectedCount(this.state)} / ${total} Selected`);
  }

  getSelectedCount() {
    return getSelectedCount(this.state);
  }

  getSelectedOrderedItemIds() {
    return this.state.order.filter(id => this.state.items.get(id).selected !== false);
  }

  selectAll() {
    this.bulkSelect(true);
  }

  deselectAll() {
    this.bulkSelect(false);
  }

  bulkSelect(selected) {
    if (this.selectionLocked) return;
    setAllSelected(this.state, selected);
    for (const card of this.cards.values()) {
      card.checkbox.checked = selected;
    }
    this.updateSelectionBadge();
    if (this.filter !== "all") this.applyListChrome();
    this.onSelectionChange?.();
    this.onChange?.();
  }

  reorderCard(id, dir) {
    if (this.selectionLocked) return;
    reorderItem(this.state, id, dir);
    const card = this.cards.get(id);
    if (card) {
      // Headers sit between rows; take them out so the swap is with the neighbouring row.
      this.clearGroupHeads();
      if (dir === "up" && card.el.previousElementSibling) {
        card.el.previousElementSibling.before(card.el);
      } else if (dir === "down" && card.el.nextElementSibling) {
        card.el.nextElementSibling.after(card.el);
      }
      this.applyListChrome();
    }
    this.updateEpisodeNumbers();
    this.onSelectionChange?.();
    this.onChange?.();
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
    const { totalCount, completedCount, skippedCount, cancelledCount, errorCount, batchCompleted, batchItemIds } = this.state;
    const total = batchItemIds?.length ?? totalCount;

    if (batchCompleted) {
      const parts = [`${completedCount} Completed`];
      if (skippedCount > 0) parts.push(`${skippedCount} Skipped`);
      if (cancelledCount > 0) parts.push(`${cancelledCount} Cancelled`);
      if (errorCount > 0) parts.push(`${errorCount} Failed`);
      this.setCountText(`${parts.join(", ")} (${total} Total)`);
    } else {
      const processed = completedCount + skippedCount + cancelledCount + errorCount;
      this.setCountText(`${processed} / ${total} Processed`);
    }
  }
}
