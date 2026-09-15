import {
  carryOverAssignments,
  createQueueState,
  createQueueStateFromSnapshot,
  finalizeQueueState,
  formatElapsedDuration,
  formatOutcomeDuration,
  reduceQueueEvent,
} from "./queue-state.js";

// ─── Real-Time Episode Queue & Progress/Duration Tracker ───────────────────

export class QueueTracker {
  constructor(mode) {
    this.mode = mode;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.state = createQueueState({ version: 2, items: [] }, { mode });
    this.cards = new Map();
    this.scanToken = 0;
    this.lastScannedPath = null;
    this.lastManifest = null;
    // GUI manual subtitle selection (remux only): item id -> candidate path
    this.assignments = new Map();
    // v2 manifest: candidates are stored once per release directory.
    this.subtitleCandidates = {};
    this.heartbeatTimer = null;
  }

  startHeartbeat() {
    if (this.heartbeatTimer === null) {
      this.heartbeatTimer = setInterval(() => this.tickHeartbeat(), 1000);
      if (typeof this.heartbeatTimer?.unref === "function") {
        this.heartbeatTimer.unref();
      }
    }
  }

  stopHeartbeat() {
    if (this.heartbeatTimer !== null) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  hasProcessingItems() {
    if (!this.state?.items) return false;
    for (const item of this.state.items.values()) {
      if (item.status === "processing") {
        return true;
      }
    }
    return false;
  }

  formatProcessingTag(item) {
    const elapsed = Math.floor((Date.now() - (item.startTime || Date.now())) / 1000);
    if (typeof item.progress === "number") {
      return `${Math.round(item.progress)}% (${formatElapsedDuration(elapsed)})`;
    }
    return `Processing (${formatElapsedDuration(elapsed)})`;
  }

  tickHeartbeat() {
    let hasProcessing = false;
    for (const [id, card] of this.cards.entries()) {
      const item = this.state?.items?.get(id);
      if (item && item.status === "processing" && card.statusTag) {
        hasProcessing = true;
        card.statusTag.textContent = this.formatProcessingTag(item);
      }
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

  startScan() {
    this.stopHeartbeat();
    const token = ++this.scanToken;
    this.cards.clear();
    // Assignments and candidates survive a rescan: populateFromInspection
    // rebuilds candidates and carries over assignments whose items persist.
    this.state = createQueueState({ version: 2, items: [] }, { mode: this.mode });
    if (this.listEl) {
      this.listEl.innerHTML = `
        <div class="empty-state-card scanning-state">
          <div class="scan-spinner" aria-hidden="true"></div>
          <div class="empty-state-title">Scanning Directory</div>
          <div class="empty-state-desc">Inspecting MKV files and reading track metadata…</div>
        </div>`;
    }
    if (this.countEl) this.countEl.textContent = "Scanning…";
    return token;
  }

  isCurrentScan(token) {
    return token === this.scanToken;
  }

  reuseInspection(path) {
    if (this.lastScannedPath !== path) return false;
    this.scanToken++;
    this.populateFromInspection(this.lastManifest);
    return true;
  }

  acceptInspection(token, path, manifest) {
    if (!this.isCurrentScan(token)) return false;
    if (manifest?.version !== 2 || !Array.isArray(manifest.items)) {
      this.showScanError(token);
      this.lastScannedPath = null;
      return false;
    }
    this.populateFromInspection(manifest);
    this.lastScannedPath = path;
    return true;
  }

  rejectInspection(token) {
    if (!this.isCurrentScan(token)) return;
    this.showScanError(token);
    this.lastScannedPath = null;
  }

  showScanError(token) {
    if (!this.isCurrentScan(token)) return;
    if (this.listEl) {
      this.listEl.innerHTML = this.emptyStateMarkup(
        "Scan failed",
        "That release directory could not be inspected. Choose another directory and try again.",
        "↻ Choose Another Directory"
      );
    }
    if (this.countEl) this.countEl.textContent = "Scan failed";
  }

  reset(title = "No Episodes Queued", description = "Choose an input release directory to scan MKV episodes into this batch.") {
    this.stopHeartbeat();
    this.scanToken++;
    this.cards.clear();
    this.assignments.clear();
    this.subtitleCandidates = {};
    this.state = createQueueState({ version: 2, items: [] }, { mode: this.mode });
    if (this.listEl) this.listEl.innerHTML = this.emptyStateMarkup(title, description);
    if (this.countEl) this.countEl.textContent = "Ready";
  }

  populateFromInspection(manifest) {
    this.stopHeartbeat();
    this.lastManifest = manifest;
    this.cards.clear();
    if (this.listEl) this.listEl.innerHTML = "";

    const bitrate = this.getBitrate();
    this.state = createQueueState(manifest, { mode: this.mode });

    // Refresh candidate lists; keep assignments whose item still exists so a
    // rescan right before Start does not silently drop manual selections.
    this.assignments = carryOverAssignments(this.assignments, manifest);
    this.subtitleCandidates = manifest?.subtitle_candidates ?? {};

    if (!manifest || !manifest.items || manifest.items.length === 0) {
      this.reset(
        "No MKV files found",
        "This release directory does not contain any MKV episodes. Choose another input directory to continue."
      );
      return;
    }

    if (this.countEl) this.countEl.textContent = `${this.state.totalCount} Episodes Queued`;

    if (this.state && this.listEl) {
      this.renderCardsIncrementally(this.scanToken, bitrate);
    }
  }

  getBitrate() {
    return document.getElementById(`${this.mode}-bitrate`)?.value?.trim() || "192k";
  }

  // Creates card DOM in requestAnimationFrame batches under a ~8 ms frame
  // budget so large manifests don't block the main thread. Each tick aborts
  // when scanToken has moved on (reset/startScan/reuseInspection bump it).
  // Cards render from the item's CURRENT reducer state, so events that
  // arrived before a card existed are reflected once it is created.
  renderCardsIncrementally(token, bitrate) {
    const order = this.state.order;
    let index = 0;

    const renderBatch = () => {
      if (!this.isCurrentScan(token)) return;
      const frag = document.createDocumentFragment();
      const deadline = performance.now() + 8;
      do {
        const id = order[index];
        const item = this.state.items.get(id);
        index++;
        if (!item) continue;
        const card = this.createCardElement(item.id, item.name, index);
        this.cards.set(id, card);
        this.applyAssignmentToCard(id);
        if (item.inspectionError) {
          this.applyInspectionError(card, item.inspectionError);
        } else if (item.metadata) {
          this.applyMetadata(card, item.metadata, bitrate);
        } else {
          this.applyRestoredTags(card, item);
        }
        this.renderCardState(card, item);
        frag.appendChild(card.el);
      } while (index < order.length && performance.now() < deadline);
      this.listEl.appendChild(frag);
      if (index < order.length) requestAnimationFrame(renderBatch);
    };

    requestAnimationFrame(renderBatch);
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

  applyInspectionError(card, error) {
    if (!card?.el) return;
    card.el.classList.add("inspection-failed");
    if (card.codec) card.codec.textContent = "Inspection Failed";
    if (card.tag1) card.tag1.textContent = "Metadata unavailable";
    if (card.tag2) card.tag2.textContent = error;
  }

  applyRestoredTags(card, item) {
    if (!card || !card.el || !item) return;
    if (card.codec) card.codec.textContent = "MKV";
    if (item.status === "completed") {
      if (card.tag1) card.tag1.textContent = "Processed";
      if (card.tag2) card.tag2.textContent = item.durationSec != null ? `Done in ${formatOutcomeDuration(item.durationSec)}` : "Complete";
    } else if (item.status === "skipped") {
      if (card.tag1) card.tag1.textContent = "Skipped";
      if (card.tag2) card.tag2.textContent = item.reason || "Already processed";
    } else if (item.status === "cancelled") {
      if (card.tag1) card.tag1.textContent = "Cancelled";
      if (card.tag2) card.tag2.textContent = item.reason || "Cancelled";
    } else if (item.status === "error") {
      if (card.tag1) card.tag1.textContent = "Failed";
      if (card.tag2) card.tag2.textContent = item.error || "Error";
    } else {
      if (card.tag1) card.tag1.textContent = "Queued";
      if (card.tag2) card.tag2.textContent = "";
    }
  }

  createCardElement(id, name, epNum = 1) {
    const el = document.createElement("div");
    el.className = "tokyo-ep-card";
    const numStr = String(epNum).padStart(2, "0");

    const codecTag = "MKV";
    const trackTag1 = "Inspecting tracks...";
    const trackTag2 = "Reading headers...";
    const subAssignBtn = this.mode === "remux"
      ? `<button type="button" class="sub-assign-btn" title="Manually assign a subtitle file">+ Sub</button>`
      : "";

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
        ${subAssignBtn}
      </div>
      <div class="ep-progress-bar">
        <div class="ep-progress-fill" style="width: 0%;"></div>
      </div>
    `;

    const titleEl = el.querySelector(".ep-title");
    if (titleEl) titleEl.textContent = name;

    const subBtn = el.querySelector(".sub-assign-btn");
    if (subBtn) subBtn.dataset.itemId = id;

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
      subBtn,
    };
  }

  applyAssignmentToCard(id) {
    const card = this.cards.get(id);
    if (!card?.subBtn) return;
    const assigned = this.assignments.get(id);
    if (assigned) {
      const base = assigned.split("/").pop();
      card.subBtn.textContent = `Sub: ${base}`;
      card.subBtn.classList.add("assigned");
      card.subBtn.title = `Manually assigned: ${assigned}`;
    } else {
      card.subBtn.textContent = "+ Sub";
      card.subBtn.classList.remove("assigned");
      card.subBtn.title = "Manually assign a subtitle file";
    }
  }

  restoreFromSnapshot(snapshot) {
    this.stopHeartbeat();
    if (!snapshot || !snapshot.items) return;
    this.scanToken++;
    this.cards.clear();
    this.lastManifest = null;
    if (this.listEl) this.listEl.innerHTML = "";

    this.state = createQueueStateFromSnapshot(snapshot, { mode: this.mode });

    if (this.countEl) {
      this.renderQueueCount();
    }

    if (this.state && this.listEl) {
      const bitrate = this.getBitrate();
      this.renderCardsIncrementally(this.scanToken, bitrate);
    }
  }

  handleEvent(event) {
    if (!this.state) return;
    this.state = reduceQueueEvent(this.state, event);

    if (event.id) {
      const item = this.state.items.get(event.id);
      let card = this.cards.get(event.id);
      if (!card && item) {
        const index = this.cards.size + 1;
        card = this.createCardElement(item.id, item.name, index);
        this.cards.set(item.id, card);
        this.applyAssignmentToCard(item.id);
        if (item.inspectionError) {
          this.applyInspectionError(card, item.inspectionError);
        } else if (item.metadata) {
          this.applyMetadata(card, item.metadata, this.getBitrate());
        } else {
          this.applyRestoredTags(card, item);
        }
        if (this.listEl) {
          const empty = this.listEl.querySelector(".empty-state-card");
          if (empty && typeof empty.remove === "function") {
            empty.remove();
          }
          this.listEl.appendChild(card.el);
        }
      }
      if (item && card) {
        this.renderCardState(card, item);
      }
    }

    this.renderQueueCount();
  }

  renderCardState(card, item) {
    if (!card || !card.el || !item) return;

    switch (item.status) {
      case "processing": {
        card.el.className = "tokyo-ep-card processing";
        card.badge.className = "ep-badge purple";
        card.badge.textContent = "PROCESSING";
        card.statusTag.className = "ep-duration-tag yellow status-tag";
        card.statusTag.textContent = this.formatProcessingTag(item);
        if (typeof item.progress === "number") {
          card.fill.className = "ep-progress-fill";
          card.fill.style.width = `${Math.min(100, Math.max(0, item.progress))}%`;
        } else {
          card.fill.className = "ep-progress-fill indeterminate";
          card.fill.style.width = "";
        }
        break;
      }
      case "completed": {
        card.el.className = "tokyo-ep-card completed";
        card.badge.className = "ep-badge green";
        card.badge.textContent = "COMPLETED";
        card.statusTag.className = "ep-duration-tag green status-tag";
        card.statusTag.textContent = item.durationSec != null ? `Done in ${formatOutcomeDuration(item.durationSec)}` : "Done";
        card.fill.className = "ep-progress-fill green";
        card.fill.style.width = "100%";
        break;
      }
      case "skipped": {
        card.el.className = "tokyo-ep-card skipped";
        card.badge.className = "ep-badge grey";
        card.badge.textContent = "SKIPPED";
        card.statusTag.className = "ep-duration-tag grey status-tag";
        card.statusTag.textContent = item.reason || "Skipped";
        card.fill.className = "ep-progress-fill grey";
        card.fill.style.width = "100%";
        break;
      }
      case "cancelled": {
        card.el.className = "tokyo-ep-card skipped";
        card.badge.className = "ep-badge grey";
        card.badge.textContent = "CANCELLED";
        card.statusTag.className = "ep-duration-tag grey status-tag";
        card.statusTag.textContent = item.reason || "Cancelled";
        card.fill.className = "ep-progress-fill grey";
        card.fill.style.width = "100%";
        break;
      }
      case "planned": {
        card.el.className = "tokyo-ep-card dry-run";
        card.badge.className = "ep-badge cyan";
        card.badge.textContent = "DRY RUN";
        card.statusTag.className = "ep-duration-tag cyan status-tag";
        card.statusTag.textContent = "Planned (Dry Run)";
        card.fill.className = "ep-progress-fill cyan";
        card.fill.style.width = "100%";
        break;
      }
      case "error": {
        card.el.className = "tokyo-ep-card failed";
        card.badge.className = "ep-badge red";
        card.badge.textContent = "FAILED";
        card.statusTag.className = "ep-duration-tag red status-tag";
        card.statusTag.textContent = item.error || "Failed";
        card.fill.className = "ep-progress-fill red";
        card.fill.style.width = "100%";
        break;
      }
    }

    if (item.status === "processing") {
      this.startHeartbeat();
    } else if (!this.hasProcessingItems()) {
      this.stopHeartbeat();
    }
  }

  finalize(exitCode, cancelled = false) {
    this.stopHeartbeat();
    if (!this.state) return;
    this.state = finalizeQueueState(this.state, exitCode, cancelled);

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
    const { totalCount, completedCount, skippedCount, cancelledCount = 0, errorCount, batchCompleted } = this.state;
    const processed = completedCount + skippedCount + cancelledCount + errorCount;

    if (batchCompleted) {
      const parts = [`${completedCount} Completed`];
      if (skippedCount > 0) parts.push(`${skippedCount} Skipped`);
      if (cancelledCount > 0) parts.push(`${cancelledCount} Cancelled`);
      if (errorCount > 0) parts.push(`${errorCount} Failed`);
      this.countEl.textContent = `${parts.join(", ")} (${totalCount} Total)`;
    } else {
      this.countEl.textContent = `${processed} / ${totalCount} Processed`;
    }
  }
}
