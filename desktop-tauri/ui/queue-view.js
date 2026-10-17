/** The status ring: its radius, and the circumference its dash offset is measured against. */
const RING_RADIUS = 8;
export const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

/** A mode's queue before the Desktop Core published one. */
const EMPTY_VIEW = {
  directory: null,
  scan: { state: "idle" },
  manifest_rev: 0,
  rows: [],
  library: [],
  inspector: null,
};

/** Writes `value` into `target[prop]` only when it differs, so an unchanged row costs no DOM write. */
function put(target, prop, value) {
  if (target[prop] !== value) target[prop] = value;
}

function episodeNumberText(epNum) {
  return String(epNum).padStart(2, "0");
}

function sameSequence(a, b) {
  return a.length === b.length && a.every((id, index) => id === b[index]);
}

/**
 * Whether two Directory Identities name the same directory: the Desktop Core publishes them in
 * display-canonical form, so they compare byte for byte, and a missing or empty one never matches.
 */
function sameDirectory(a, b) {
  return Boolean(a) && a === b;
}

const CHECK_ICON = `<svg class="ep-icon ep-icon-ok" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>`;
const CROSS_ICON = `<svg class="ep-icon ep-icon-err" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>`;
const RING = `<svg class="ep-ring" width="20" height="20" viewBox="0 0 20 20" aria-hidden="true"><circle class="ep-ring-track" cx="10" cy="10" r="${RING_RADIUS}"/><circle class="ep-ring-arc" cx="10" cy="10" r="${RING_RADIUS}" transform="rotate(-90 10 10)" stroke-dasharray="${RING_CIRCUMFERENCE.toFixed(2)}"/></svg>`;
const CHEVRON = d => `<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${d}"/></svg>`;

/**
 * The Queue Tracker's view of one mode: renders the queue the Desktop Core publishes
 * (`applyView`) and turns the user's selection, order and inspector clicks into Intents through
 * `dispatch`. Every text a row shows (its label, tone, hover text, meta line and filter bucket) is
 * published (`crates/mkv-desktop-core/src/presentation.rs`); it holds no queue rules of its own.
 */
export class QueueTracker {
  /**
   * @param {(intent: object) => Promise<unknown>} dispatch sends an Intent to the Desktop Core and
   *   resolves to its reply once the state it published is applied
   */
  constructor(mode, dispatch) {
    if (typeof dispatch !== "function") throw new TypeError(`QueueTracker(${mode}) needs a dispatch function`);
    this.mode = mode;
    this.dispatch = dispatch;
    this.listEl = document.getElementById(`${mode}-episode-list`);
    this.countEl = document.getElementById(`${mode}-queue-count`);
    this.filterEmptyEl = document.getElementById(`${mode}-filter-empty`);
    // The last ModeView applied, its rows by ID, and their order.
    this.view = EMPTY_VIEW;
    this.rows = new Map();
    this.order = [];
    this.cards = new Map();
    // What the list shows: "empty", "scanning", "failed", or "rows" (mounted or mounting).
    this.shown = null;
    this.emptyTitle = null;
    // The row IDs in the order they are mounted, once an incremental render completes.
    this.renderedOrder = [];
    // The order an incremental render is mounting, or null when none is pending.
    this.renderingOrder = null;
    this.renderRafId = null;
    this.isScanning = false;
    // Set by the owning JobController from the published `controls.selection_locked`. While true,
    // every user selection or order change is refused: the core captured the ordered selected IDs
    // at Run. The guard lives on the mutation, not only on the disabled controls, because a row
    // re-rendered after the last sync carries a fresh, enabled checkbox.
    this.selectionLocked = false;
    // Presentation only: the list filter chip, a row picked for the inspector whose `inspect`
    // Intent is not answered yet (highlighted at once), the highlighted row, and the release group
    // headers currently mounted between rows.
    this.filter = "all";
    this.pendingInspect = null;
    this.markedId = null;
    this.groupHeads = [];
    // Called after anything the surrounding chrome (inspector, library, bottom bar) shows changed.
    this.onChange = null;
    if (this.listEl) {
      this.listEl.addEventListener("change", async e => {
        const check = e.target.closest(".ep-select-check");
        const id = check?.dataset.itemId;
        if (!id) return;
        // Puts the DOM back in step with the published selection.
        const revert = () => {
          check.checked = this.rows.get(id)?.selected === true;
        };
        // Locked: the request was built from the published selection.
        if (this.selectionLocked) return revert();
        // The core can refuse on its own (a job runs, the mode scans) and then publishes nothing,
        // so no published view would correct the tick.
        let applied = false;
        try {
          applied = await this.dispatch({ type: "set_selected", mode: this.mode, id, selected: check.checked });
        } catch (err) {
          console.error("[set_selected] failed:", err);
        }
        if (!applied) revert();
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

  /** Replaces the list with an empty-state card. Its text is set as text, never parsed as HTML. */
  showEmptyState(title, description, actionLabel = "Open folder…") {
    this.clearList(`
      <div class="empty-state-card">
        <svg class="empty-icon" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>
        <div class="empty-state-title"></div>
        <div class="empty-state-desc"></div>
        <button class="btn btn-accent empty-state-action" type="button" data-target="${this.mode}-input-dir"></button>
      </div>`);
    if (!this.listEl) return;
    this.listEl.querySelector(".empty-state-title").textContent = title;
    this.listEl.querySelector(".empty-state-desc").textContent = description;
    this.listEl.querySelector(".empty-state-action").textContent = actionLabel;
  }

  /**
   * Renders the ModeView the Desktop Core published for this mode.
   *
   * @param {object} view a ModeView
   * @param {"all"|number[]} changedRows the indices of the rows that changed since the last view
   *   (`nextState` in `view-patch.js`), or "all"; only their cards are redrawn
   */
  applyView(view, changedRows = "all") {
    const previous = this.view;
    this.view = view;
    this.rows = new Map(view.rows.map(row => [row.id, row]));
    this.order = view.rows.map(row => row.id);
    this.isScanning = view.scan?.state === "scanning";
    this.setCountText(view.count_text);

    if (this.isScanning) this.showScanning(previous, view.scan.directory);
    else if (view.scan?.state === "failed") this.showFailure();
    else if (this.order.length === 0) this.showEmpty();
    else this.showRows(previous, changedRows);
    // The published inspector may name another row than the last view's.
    this.markInspected();
    this.onChange?.();
  }

  /** Replaces the list with `markup`, dropping its cards and headers and ending any render. */
  clearList(markup) {
    this.cancelRender();
    this.cards.clear();
    this.groupHeads = [];
    this.renderedOrder = [];
    this.markedId = null;
    if (this.listEl) this.listEl.innerHTML = markup;
  }

  showScanning(previous, directory) {
    // Re-inspecting the directory already shown (every Run forces one) leaves its fully rendered
    // rows on screen, so the accepted manifest can reuse them instead of flashing the list empty.
    const keepCards = this.shown === "rows" && this.cards.size > 0 && this.renderingOrder === null
      && sameDirectory(previous.directory, directory);
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
    if (this.shown === "failed") return;
    this.shown = "failed";
    // A scan that kept its rows must not leave them registered once the failure markup replaces them.
    this.showEmptyState(
      "Scan failed",
      "That folder could not be inspected. Choose another folder and try again.",
      "Choose another folder…"
    );
  }

  /** No rows: an empty folder when a directory is shown, else no folder yet. */
  showEmpty() {
    // The title choice is still the view's (out of S4); the core could publish it.
    const [title, description] = this.view.directory
      ? ["No MKV files found", "This folder holds no MKV episodes. Open another release folder to continue."]
      : ["No episodes yet", "Open a release folder to scan its MKV episodes into this batch."];
    if (this.shown === "empty" && this.emptyTitle === title) return;
    this.shown = "empty";
    this.emptyTitle = title;
    this.showEmptyState(title, description);
    if (this.filterEmptyEl) this.filterEmptyEl.hidden = true;
  }

  /**
   * Brings the mounted rows in step with the published ones. A new manifest of the directory
   * shown, with the same IDs in the same order, redraws the rows in place; the same rows in
   * another order are moved; any other row set rebuilds the list.
   */
  showRows(previous, changedRows) {
    const { order } = this;
    const wasRows = this.shown === "rows";
    this.shown = "rows";
    if (!wasRows) {
      this.renderCardsIncrementally();
    } else if (this.renderingOrder !== null) {
      // A pending render mounts each row from the current view, so only another row set or
      // another manifest restarts it; otherwise the rows already mounted are redrawn.
      if (sameSequence(this.renderingOrder, order) && previous.manifest_rev === this.view.manifest_rev) {
        this.updateCards(changedRows);
      } else {
        this.renderCardsIncrementally();
      }
    } else if (previous.manifest_rev !== this.view.manifest_rev) {
      if (sameDirectory(previous.directory, this.view.directory) && sameSequence(this.renderedOrder, order)) {
        this.updateCards("all");
        // The new manifest may put a row in another release.
        this.applyListChrome();
      } else {
        this.renderCardsIncrementally();
      }
    } else if (sameSequence(this.renderedOrder, order)) {
      this.updateCards(changedRows);
    } else if (this.renderedOrder.length === order.length && order.every(id => this.cards.has(id))) {
      this.moveCards();
      this.updateCards("all");
    } else {
      this.renderCardsIncrementally();
    }
  }

  /** Redraws the mounted rows in `changedRows` (indices, or "all"). */
  updateCards(changedRows = "all") {
    let bucketChanged = false;
    const ids = changedRows === "all" ? [...this.cards.keys()] : changedRows.map(index => this.order[index]);
    for (const id of ids) {
      const card = this.cards.get(id);
      const row = this.rows.get(id);
      if (card && row && this.drawCard(card, row)) bucketChanged = true;
    }
    // The filter matches on the bucket, so only a change there redraws the chrome.
    if (bucketChanged && this.filter !== "all") this.applyListChrome();
  }

  /** Puts the mounted rows in the published order. */
  moveCards() {
    // Headers sit between rows; take them out so the rows line up.
    this.clearGroupHeads();
    this.listEl?.append(...this.order.map(id => this.cards.get(id).el));
    this.renderedOrder = [...this.order];
    this.updateEpisodeNumbers();
    this.applyListChrome();
  }

  // Creates rows in requestAnimationFrame batches under a ~8 ms frame budget so a large manifest
  // does not block the main thread. A tick aborts once another render or a cleared list took
  // over. Rows render from the current view, so a view applied before a row exists shows once it
  // is created.
  renderCardsIncrementally() {
    this.cancelRender();
    const order = this.order;
    this.cards.clear();
    this.groupHeads = [];
    this.renderedOrder = [];
    this.markedId = null;
    if (!this.listEl || order.length === 0) return;

    this.listEl.innerHTML = "";
    this.renderingOrder = order;
    let index = 0;

    const renderBatch = () => {
      this.renderRafId = null;
      if (this.renderingOrder !== order) return;
      const frag = document.createDocumentFragment();
      const deadline = performance.now() + 8;
      do {
        const row = this.rows.get(order[index]);
        index++;
        if (!row) continue;
        frag.appendChild(this.mountCard(row, index).el);
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

  /** Create and register a card for `row`, drawn; the caller attaches it. */
  mountCard(row, epNum) {
    const card = this.createCardElement(row, epNum);
    this.cards.set(row.id, card);
    this.drawCard(card, row);
    if (row.id === this.inspectedId()) {
      this.markedId = row.id;
      card.el.classList.add("is-inspected");
    }
    return card;
  }

  /**
   * Draws `row` on `card`: its name, selection, meta line and status. Returns whether its filter
   * bucket changed.
   */
  drawCard(card, row) {
    if (card.title.textContent !== row.name) {
      card.title.textContent = row.name;
      card.checkbox.setAttribute?.("aria-label", `Include ${row.name} in the batch`);
    }
    put(card.checkbox, "checked", row.selected);
    put(card.meta, "className", row.meta_error ? "ep-meta is-error" : "ep-meta");
    put(card.meta, "textContent", row.meta);
    this.renderCardState(card, row);
    const bucketChanged = card.bucket !== row.bucket;
    card.bucket = row.bucket;
    return bucketChanged;
  }

  createCardElement(row, epNum) {
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
      // The filter bucket drawCard last drew; none yet.
      bucket: undefined,
    };
    card.num.textContent = episodeNumberText(epNum);
    for (const control of [card.checkbox, card.pick, el.querySelector(".ep-up-btn"), el.querySelector(".ep-down-btn")]) {
      if (control) control.dataset.itemId = row.id;
    }
    return card;
  }

  /** The row the inspector shows: one just picked, else the published inspector's. */
  inspectedId() {
    return this.pendingInspect ?? this.view.inspector?.id ?? null;
  }

  /**
   * Sends the `inspect` Intent for `id`, highlighting its row at once: the inspector follows once
   * the core publishes the pick.
   */
  async inspect(id) {
    if (!this.rows.has(id)) return;
    this.pendingInspect = id;
    this.markInspected();
    try {
      await this.dispatch({ type: "inspect", mode: this.mode, id });
    } finally {
      // A later pick still pending keeps its own highlight.
      if (this.pendingInspect === id) {
        this.pendingInspect = null;
        this.markInspected();
      }
    }
  }

  /** Highlights the row the inspector shows, the first row included before any pick. */
  markInspected() {
    const id = this.inspectedId();
    if (id === this.markedId) return;
    this.cards.get(this.markedId)?.el.classList.remove("is-inspected");
    this.markedId = id;
    this.cards.get(id)?.el.classList.add("is-inspected");
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
      ?? this.cards.get(this.order.find(id => this.rows.get(id)?.release === release))?.el;
    target?.scrollIntoView?.({ block: "start" });
  }

  setFilter(filter) {
    this.filter = filter;
    this.applyListChrome();
    this.onChange?.();
  }

  /** The published group header of `release`: the input folder's covers `.` and the empty release. */
  groupLabel(release) {
    const key = release || ".";
    return this.view.library?.find(entry => (entry.release ?? ".") === key)?.group_label ?? "Episodes";
  }

  /**
   * Shows the rows the filter chip matches (by their published bucket) and puts a sticky header
   * before each run of rows from one release directory, when the queue spans more than one. Rows
   * stay direct children of the list in queue order, so reordering and incremental rendering
   * never see a header.
   */
  applyListChrome() {
    if (!this.listEl || this.renderingOrder !== null) return;
    this.markInspected();
    this.clearGroupHeads();
    const filtering = this.filter !== "all";
    if (filtering) this.listEl.classList.add("is-filtered");
    else this.listEl.classList.remove("is-filtered");

    const shown = this.order.filter(id => {
      const card = this.cards.get(id);
      const row = this.rows.get(id);
      if (!card || !row) return false;
      // An unselected row has no bucket, so it shows only under All.
      const match = !filtering || row.bucket === this.filter;
      card.el.hidden = !match;
      return match;
    });
    if (this.filterEmptyEl) this.filterEmptyEl.hidden = !(filtering && shown.length === 0 && this.cards.size > 0);

    const releaseOf = id => this.rows.get(id).release;
    if (new Set(this.order.map(releaseOf)).size < 2) return;
    for (let i = 0; i < shown.length; i++) {
      const release = releaseOf(shown[i]);
      if (i > 0 && releaseOf(shown[i - 1]) === release) continue;
      let count = 0;
      while (i + count < shown.length && releaseOf(shown[i + count]) === release) count++;
      const head = document.createElement("div");
      head.className = "ep-group-head";
      head.dataset.release = release;
      const name = document.createElement("span");
      name.textContent = this.groupLabel(release);
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

  // Every write is skipped when the value is unchanged, so the stream of progress events on a
  // processing row writes just the ring and the status text.
  renderCardState(card, row) {
    put(card.el.dataset, "status", row.status);
    // Card Progress: a spinning ring until the first progress value, then a determinate one.
    const processing = row.status === "processing";
    const determinate = processing && typeof row.progress === "number";
    put(card.status, "className", `ep-status tone-${row.tone}${processing && !determinate ? " is-spinning" : ""}`);
    put(card.status, "title", row.title);
    put(card.arc.style, "strokeDashoffset", determinate ? (RING_CIRCUMFERENCE * (1 - row.progress / 100)).toFixed(2) : "");
    put(card.statusTag, "textContent", row.label);
  }

  setCountText(text) {
    if (!this.countEl || this.countEl.textContent === text) return;
    this.countEl.textContent = text;
    this.countEl.title = text;
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
    this.order.forEach((id, idx) => {
      const card = this.cards.get(id);
      if (!card) return;
      card.epNum = idx + 1;
      card.num.textContent = episodeNumberText(card.epNum);
    });
  }
}
