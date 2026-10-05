import {
  carryOverAssignments,
  createQueueState,
  createQueueStateFromSnapshot,
  finalizeQueueState,
  formatElapsedDuration,
  formatOutcomeDuration,
  getSelectedCount,
  isSameDirectory,
  normalizeDirectoryPath,
  reduceQueueEvent,
  reconcileQueueSelection,
  reorderItem,
  setAllSelected,
  setItemSelected,
} from "./queue-state.js";

const EMPTY_MANIFEST = { version: 2, items: [] };

/** Card presentation per terminal item status. */
const TERMINAL_CARD_STYLES = {
  completed: { card: "completed", color: "green", badge: "COMPLETED", text: item => doneText(item, "Done") },
  skipped: { card: "skipped", color: "grey", badge: "SKIPPED", text: item => item.reason || "Skipped" },
  cancelled: { card: "skipped", color: "grey", badge: "CANCELLED", text: item => item.reason || "Cancelled" },
  planned: { card: "dry-run", color: "cyan", badge: "DRY RUN", text: () => "Planned (Dry Run)" },
  error: { card: "failed", color: "red", badge: "FAILED", text: item => item.error || "Failed" },
};

function doneText(item, fallback) {
  return item.durationSec != null ? `Done in ${formatOutcomeDuration(item.durationSec)}` : fallback;
}

function episodeBadgeText(epNum) {
  return `EPISODE ${String(epNum).padStart(2, "0")}`;
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

/** Track tags for a card restored from a JobSnapshot, which carries no inspection metadata. */
function restoredTagText(item) {
  switch (item.status) {
    case "completed":
      return ["Processed", doneText(item, "Complete")];
    case "skipped":
      return ["Skipped", item.reason || "Already processed"];
    case "cancelled":
      return ["Cancelled", item.reason || "Cancelled"];
    case "error":
      return ["Failed", item.error || "Error"];
    default:
      return ["Queued", ""];
  }
}

export class QueueTracker {
  constructor(mode) {
    this.mode = mode;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.state = createQueueState(EMPTY_MANIFEST);
    this.cards = new Map();
    this.scanToken = 0;
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
    // Disabling the controls is not the invariant — a card re-rendered after
    // the last sync carries a fresh, enabled checkbox — so the guard lives on
    // the mutation itself.
    this.selectionLocked = false;
    // Track Assignment overrides (remux only): item id -> assignment payload
    this.assignments = new Map();
    // v2 manifest: candidates are stored once per release directory.
    this.subtitleCandidates = {};
    this.heartbeatTimer = null;
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
        this.onSelectionChange?.();
      });

      this.listEl.addEventListener("click", e => {
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

  formatProcessingTag(item) {
    const elapsed = formatElapsedDuration((Date.now() - item.startTime) / 1000);
    return typeof item.progress === "number"
      ? `${Math.round(item.progress)}% (${elapsed})`
      : `Processing (${elapsed})`;
  }

  tickHeartbeat() {
    let hasProcessing = false;
    for (const [id, card] of this.cards) {
      const item = this.state.items.get(id);
      if (item?.status !== "processing") continue;
      hasProcessing = true;
      card.statusTag.textContent = this.formatProcessingTag(item);
    }
    if (!hasProcessing && !this.hasProcessingItems()) {
      this.stopHeartbeat();
    }
  }

  candidatesForItem(itemId) {
    const item = this.state.items.get(itemId);
    if (!item?.release) return [];
    return this.subtitleCandidates[item.release] ?? [];
  }

  emptyStateMarkup(title, description, actionLabel = "📁 Choose Input Directory") {
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

  startScan(path) {
    this.stopHeartbeat();
    this.isScanning = true;
    // Re-inspecting the directory already shown (every Run forces one) leaves
    // its fully rendered cards on screen; populateFromInspection then reuses
    // the unchanged ones instead of flashing the list empty.
    const keepCards = this.cards.size > 0 && this.pendingRenderToken === null && isSameDirectory(this.currentDirectory, path);
    this.cancelRender();
    const token = ++this.scanToken;
    if (!keepCards) {
      this.cards.clear();
      if (this.listEl) {
        this.listEl.innerHTML = `
          <div class="empty-state-card scanning-state">
            <div class="scan-spinner" aria-hidden="true"></div>
            <div class="empty-state-title">Scanning Directory</div>
            <div class="empty-state-desc">Inspecting MKV files and reading track metadata…</div>
          </div>`;
      }
    }
    this.setCountText("Scanning…");
    return token;
  }

  isCurrentScan(token) {
    return token === this.scanToken;
  }

  setDirectory(path) {
    const directory = normalizeDirectoryPath(path);
    this.currentDirectory = directory;
    this.lastScannedPath = directory;
  }

  reuseInspection(path) {
    if (!this.lastManifest || !isSameDirectory(this.lastScannedPath, path)) return false;
    this.scanToken++;
    this.populateFromInspection(this.lastManifest, true);
    return true;
  }

  acceptInspection(token, path, manifest) {
    if (!this.isCurrentScan(token)) return false;
    if (manifest?.version !== 2 || !Array.isArray(manifest.items)) {
      this.failScan();
      return false;
    }
    this.populateFromInspection(manifest, isSameDirectory(this.currentDirectory, path));
    this.setDirectory(path);
    return true;
  }

  rejectInspection(token) {
    if (this.isCurrentScan(token)) this.failScan();
  }

  failScan() {
    this.isScanning = false;
    this.lastScannedPath = null;
    // A scan that kept its cards must not leave them registered once the
    // failure markup replaces them.
    this.cards.clear();
    if (this.listEl) {
      this.listEl.innerHTML = this.emptyStateMarkup(
        "Scan failed",
        "That release directory could not be inspected. Choose another directory and try again.",
        "↻ Choose Another Directory"
      );
    }
    this.setCountText("Scan failed");
  }

  reset(title = "No Episodes Queued", description = "Choose an input directory to scan MKV episodes into this batch.") {
    this.stopHeartbeat();
    this.isScanning = false;
    this.cancelRender();
    this.scanToken++;
    this.cards.clear();
    this.assignments.clear();
    this.subtitleCandidates = {};
    this.currentDirectory = null;
    this.lastScannedPath = null;
    this.state = createQueueState(EMPTY_MANIFEST);
    if (this.listEl) this.listEl.innerHTML = this.emptyStateMarkup(title, description);
    this.setCountText("Ready");
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
        "This release directory does not contain any MKV episodes. Choose another input directory to continue."
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
  }

  /** True when the mounted cards are exactly `previousOrder` and the new state keeps that order. */
  cardsMatchOrder(previousOrder) {
    const { order } = this.state;
    return order.length > 0
      && this.cards.size === order.length
      && previousOrder.length === order.length
      && order.every((id, index) => id === previousOrder[index] && this.cards.has(id));
  }

  // Updates the mounted cards in place for a same-directory manifest with the
  // same IDs in the same order. A card keeps its element when its manifest
  // entry is deep-equal to the previous one and it was rendered at the current
  // bitrate, since those are everything mountCard renders from besides the
  // selection and status re-applied here (a surviving ID keeps its assignment
  // unchanged, so its button is already current). Any other card is remounted
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
      this.renderCardState(fresh, item);
      card.el.replaceWith(fresh.el);
    });
  }

  getBitrate() {
    return document.getElementById(`${this.mode}-bitrate`)?.value?.trim() || "192k";
  }

  // Creates card DOM in requestAnimationFrame batches under a ~8 ms frame
  // budget so large manifests don't block the main thread. Each tick aborts
  // when scanToken has moved on (reset/startScan/reuseInspection bump it).
  // Cards render from the item's CURRENT reducer state, so events that
  // arrived before a card existed are reflected once it is created.
  renderCardsIncrementally() {
    this.cancelRender();
    const order = this.state.order;
    if (!this.listEl || order.length === 0) return;

    const token = this.scanToken;
    const bitrate = this.getBitrate();
    this.cards.clear();
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
        const card = this.mountCard(item, index, bitrate);
        this.renderCardState(card, item);
        frag.appendChild(card.el);
      } while (index < order.length && performance.now() < deadline);
      this.listEl.appendChild(frag);
      if (index < order.length) {
        this.renderRafId = requestAnimationFrame(renderBatch);
      } else {
        this.pendingRenderToken = null;
      }
    };

    this.renderRafId = requestAnimationFrame(renderBatch);
  }

  /** Create and register a card for `item` with its track tags; the caller attaches it and renders its state. */
  mountCard(item, epNum, bitrate) {
    const card = this.createCardElement(item, epNum);
    card.bitrate = bitrate;
    this.cards.set(item.id, card);
    this.applyAssignmentToCard(item.id);
    if (item.inspectionError) {
      this.applyInspectionError(card, item.inspectionError);
    } else if (item.metadata) {
      this.applyMetadata(card, item.metadata, bitrate);
    } else {
      this.applyRestoredTags(card, item);
    }
    return card;
  }

  applyMetadata(card, meta, bitrate) {
    card.codec.textContent = meta.video_tag || "MKV";
    if (this.mode === "remux") {
      card.tag1.textContent = meta.has_flac
        ? `${meta.audio_summary || "Audio"} → Opus ${bitrate}`
        : (meta.audio_summary ? `${meta.audio_summary} (Passthrough)` : "No Audio");
      card.tag2.textContent = meta.fonts_count > 0
        ? `${meta.sub_summary || "No Subs"} + ${meta.fonts_count} Fonts`
        : (meta.sub_summary || "No Subs");
    } else {
      card.tag1.textContent = meta.sub_summary || "No Subs";
      card.tag2.textContent = `${meta.fonts_count || 0} Attached Fonts`;
    }
  }

  applyInspectionError(card, error) {
    card.codec.textContent = "Inspection Failed";
    card.tag1.textContent = "Metadata unavailable";
    card.tag2.textContent = error;
  }

  applyRestoredTags(card, item) {
    const [tag1, tag2] = restoredTagText(item);
    card.codec.textContent = "MKV";
    card.tag1.textContent = tag1;
    card.tag2.textContent = tag2;
  }

  createCardElement(item, epNum) {
    const el = document.createElement("div");
    el.className = "tokyo-ep-card";
    const subAssignBtn = this.mode === "remux" ? `<button type="button" class="sub-assign-btn"></button>` : "";
    el.innerHTML = `
      <div class="ep-card-top">
        <div class="ep-card-left">
          <input type="checkbox" class="ep-select-check" title="Include in batch" />
          <div class="ep-reorder-group">
            <button type="button" class="ep-reorder-btn ep-up-btn" title="Move up">▲</button>
            <button type="button" class="ep-reorder-btn ep-down-btn" title="Move down">▼</button>
          </div>
          <div class="ep-badge">${episodeBadgeText(epNum)}</div>
        </div>
        <span class="ep-codec"></span>
        <span class="ep-duration-tag status-tag">Queued</span>
      </div>
      <div class="ep-title"></div>
      <div class="ep-tracks">
        <span class="track-tag cyan"></span>
        <span class="track-tag magenta"></span>
        ${subAssignBtn}
      </div>
      <div class="ep-progress-bar">
        <div class="ep-progress-fill" style="width: 0%;"></div>
      </div>
    `;

    const card = {
      el,
      statusTag: el.querySelector(".status-tag"),
      badge: el.querySelector(".ep-badge"),
      fill: el.querySelector(".ep-progress-fill"),
      codec: el.querySelector(".ep-codec"),
      tag1: el.querySelector(".track-tag.cyan"),
      tag2: el.querySelector(".track-tag.magenta"),
      subBtn: el.querySelector(".sub-assign-btn"),
      checkbox: el.querySelector(".ep-select-check"),
      epNum,
      // The status renderCardState last drew; the markup above is the queued look.
      renderedStatus: "queued",
    };
    el.querySelector(".ep-title").textContent = item.name;
    card.checkbox.checked = item.selected !== false;
    for (const control of [card.checkbox, el.querySelector(".ep-up-btn"), el.querySelector(".ep-down-btn"), card.subBtn]) {
      if (control) control.dataset.itemId = item.id;
    }
    return card;
  }

  applyAssignmentToCard(id) {
    const card = this.cards.get(id);
    if (!card?.subBtn) return;
    const assigned = this.assignments.get(id);
    if (assigned) {
      const assetPath = assigned.asset_path || "";
      const base = assetPath.split("/").pop().split("\\").pop();
      card.subBtn.textContent = `Sub: ${base}`;
      card.subBtn.classList.add("assigned");
      card.subBtn.title = `Manually assigned: ${assetPath}`;
    } else {
      card.subBtn.textContent = "+ Sub";
      card.subBtn.classList.remove("assigned");
      card.subBtn.title = "Manually assign a subtitle file";
    }
  }

  restoreFromSnapshot(snapshot) {
    this.stopHeartbeat();
    this.isScanning = false;
    this.cancelRender();
    this.scanToken++;
    this.cards.clear();
    this.lastManifest = null;
    if (this.listEl) this.listEl.innerHTML = "";

    this.state = createQueueStateFromSnapshot(snapshot);
    if (snapshot.input_dir) this.setDirectory(snapshot.input_dir);
    this.renderQueueCount();
    this.renderCardsIncrementally();
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
      let card = this.cards.get(item.id);
      // While an incremental render is pending it will mount this card from current state.
      if (!card && this.pendingRenderToken !== this.scanToken) {
        card = this.mountCard(item, this.cards.size + 1, this.getBitrate());
        if (this.listEl) {
          this.listEl.querySelector(".empty-state-card")?.remove();
          this.listEl.appendChild(card.el);
        }
      }
      if (card) this.renderCardState(card, item);
    }

    this.renderQueueCount();
  }

  // Classes and badge are written only when the status differs from the one
  // the card last drew, so the stream of progress events on a processing card
  // writes just the fill and the tag text. The heartbeat is not stopped here:
  // tickHeartbeat stops itself once nothing is processing.
  renderCardState(card, item) {
    const statusChanged = card.renderedStatus !== item.status;
    card.renderedStatus = item.status;

    if (item.status === "processing") {
      if (statusChanged) {
        card.el.className = "tokyo-ep-card processing";
        card.badge.className = "ep-badge purple";
        card.badge.textContent = "PROCESSING";
        card.statusTag.className = "ep-duration-tag yellow status-tag";
      }
      card.statusTag.textContent = this.formatProcessingTag(item);
      // Shimmer until the first progress value, then a determinate bar.
      const determinate = typeof item.progress === "number";
      const fillClass = determinate ? "ep-progress-fill" : "ep-progress-fill indeterminate";
      if (card.fill.className !== fillClass) card.fill.className = fillClass;
      card.fill.style.width = determinate ? `${item.progress}%` : "";
      this.startHeartbeat();
      return;
    }
    if (!statusChanged) return;

    if (item.status === "queued") {
      // The freshly created look, for a card kept across a re-inspection.
      card.el.className = "tokyo-ep-card";
      card.badge.className = "ep-badge";
      card.badge.textContent = episodeBadgeText(card.epNum);
      card.statusTag.className = "ep-duration-tag status-tag";
      card.statusTag.textContent = "Queued";
      card.fill.className = "ep-progress-fill";
      card.fill.style.width = "0%";
      return;
    }

    const style = TERMINAL_CARD_STYLES[item.status];
    if (style) {
      card.el.className = `tokyo-ep-card ${style.card}`;
      card.badge.className = `ep-badge ${style.color}`;
      card.badge.textContent = style.badge;
      card.statusTag.className = `ep-duration-tag ${style.color} status-tag`;
      card.statusTag.textContent = style.text(item);
      card.fill.className = `ep-progress-fill ${style.color}`;
      card.fill.style.width = "100%";
    }
  }

  finalize(exitCode, cancelled = false) {
    this.stopHeartbeat();
    this.state = finalizeQueueState(this.state, exitCode, cancelled);
    for (const [id, card] of this.cards) {
      this.renderCardState(card, this.state.items.get(id));
    }
    this.renderQueueCount();
  }

  setCountText(text) {
    if (!this.countEl) return;
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
    this.onSelectionChange?.();
  }

  reorderCard(id, dir) {
    if (this.selectionLocked) return;
    reorderItem(this.state, id, dir);
    const card = this.cards.get(id);
    if (card) {
      if (dir === "up" && card.el.previousElementSibling) {
        card.el.previousElementSibling.before(card.el);
      } else if (dir === "down" && card.el.nextElementSibling) {
        card.el.nextElementSibling.after(card.el);
      }
    }
    this.updateEpisodeBadges();
    this.onSelectionChange?.();
  }

  updateEpisodeBadges() {
    this.state.order.forEach((id, idx) => {
      const card = this.cards.get(id);
      if (!card) return;
      card.epNum = idx + 1;
      card.badge.textContent = episodeBadgeText(card.epNum);
    });
  }

  renderQueueCount() {
    if (!this.countEl) return;
    if (this.state.batchStartTime === null && !this.state.batchCompleted) {
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
