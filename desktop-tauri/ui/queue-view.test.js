import test from "node:test";
import assert from "node:assert/strict";
import { QueueTracker, escapeAttr } from "./queue-view.js";
import { createQueueState, toggleItemSelection } from "./queue-state.js";

class FakeElement {
  constructor(value = "") {
    this.value = value;
    this.checked = false;
    this.disabled = false;
    this.textContent = "";
    this.className = "";
    this.innerHTML = "";
    this.dataset = {};
    this.style = {};
    this.children = [];
    this.queried = new Map();
    this.classList = {
      _classes: new Set(),
      add(cls) { this._classes.add(cls); },
      remove(cls) { this._classes.delete(cls); },
      toggle(cls, force) {
        if (force === undefined) {
          if (this._classes.has(cls)) this._classes.delete(cls);
          else this._classes.add(cls);
        } else if (force) {
          this._classes.add(cls);
        } else {
          this._classes.delete(cls);
        }
      },
      has(cls) { return this._classes.has(cls); },
    };
  }

  set innerHTML(markup) {
    this._innerHTML = markup;
    this.children = [];
  }

  get innerHTML() { return this._innerHTML; }

  appendChild(child) {
    if (child.isFragment) this.children.push(...child.children);
    else this.children.push(child);
    return child;
  }

  addEventListener() {}
  setAttribute() {}
  getAttribute() { return null; }

  querySelector(selector) {
    if (!this.queried.has(selector)) {
      const el = new FakeElement();
      if (selector.startsWith(".")) el.className = selector.slice(1);
      this.queried.set(selector, el);
    }
    return this.queried.get(selector);
  }
}

function setupMockEnvironment(t) {
  const previousDocument = globalThis.document;
  const previousRaf = globalThis.requestAnimationFrame;

  const elements = new Map([
    ["remux-episode-list", new FakeElement()],
    ["remux-queue-count", new FakeElement()],
    ["remux-bitrate", new FakeElement("192k")],
  ]);

  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    createElement: () => new FakeElement(),
    createDocumentFragment: () => Object.assign(new FakeElement(), { isFragment: true }),
  };
  globalThis.requestAnimationFrame = cb => cb();

  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.requestAnimationFrame = previousRaf;
  });

  return { elements };
}

test("restoreFromSnapshot handles null or empty snapshot gracefully", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  // Should not throw
  tracker.restoreFromSnapshot(null);
  assert.equal(tracker.state.items.size, 0);

  tracker.restoreFromSnapshot({});
  assert.equal(tracker.state.items.size, 0);

  tracker.restoreFromSnapshot({ items: null });
  assert.equal(tracker.state.items.size, 0);
});

test("restoreFromSnapshot correctly restores items, outcomes, and batch completion", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const snapshot = {
    job_id: 1,
    mode: "remux",
    status: "completed",
    duration_sec: 25.5,
    total_items: 4,
    ordered_item_ids: ["ep1", "ep2", "ep3", "ep4"],
    completed_items: 2,
    items: {
      ep1: {
        id: "ep1",
        file: "Show - 01.mkv",
        status: "completed",
        duration_sec: 10.2,
        reason: null,
        error: null,
      },
      ep2: {
        id: "ep2",
        file: "Show - 02.mkv",
        status: "skipped",
        duration_sec: null,
        reason: "Already up to date",
        error: null,
      },
      ep3: {
        id: "ep3",
        file: "Show - 03.mkv",
        status: "cancelled",
        duration_sec: null,
        reason: "User cancelled",
        error: null,
      },
      ep4: {
        id: "ep4",
        file: "Show - 04.mkv",
        status: "error",
        duration_sec: null,
        reason: null,
        error: "FFmpeg encoding failed",
      },
    },
  };

  tracker.restoreFromSnapshot(snapshot);

  assert.equal(tracker.state.items.size, 4);
  assert.equal(tracker.state.completedCount, 1);
  assert.equal(tracker.state.skippedCount, 1);
  assert.equal(tracker.state.cancelledCount, 1);
  assert.equal(tracker.state.errorCount, 1);
  assert.equal(tracker.state.batchCompleted, true);
  assert.equal(tracker.state.batchSuccess, true);
  assert.equal(tracker.state.batchCancelled, false);

  const ep1 = tracker.state.items.get("ep1");
  assert.equal(ep1.status, "completed");
  assert.equal(ep1.durationSec, 10.2);

  const ep2 = tracker.state.items.get("ep2");
  assert.equal(ep2.status, "skipped");
  assert.equal(ep2.reason, "Already up to date");

  const ep3 = tracker.state.items.get("ep3");
  assert.equal(ep3.status, "cancelled");
  assert.equal(ep3.reason, "User cancelled");

  const ep4 = tracker.state.items.get("ep4");
  assert.equal(ep4.status, "error");
  assert.equal(ep4.error, "FFmpeg encoding failed");

  // Verify cards exist and their state was rendered
  assert.equal(tracker.cards.size, 4);
  const card1 = tracker.cards.get("ep1");
  assert.ok(card1.el.className.includes("completed"));
  assert.equal(card1.badge.textContent, "COMPLETED");

  const card4 = tracker.cards.get("ep4");
  assert.ok(card4.el.className.includes("failed"));
  assert.equal(card4.badge.textContent, "FAILED");
});

test("restoreFromSnapshot handles cancelled and error batch status", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const snapshotCancelled = {
    job_id: 2,
    mode: "remux",
    status: "cancelled",
    ordered_item_ids: ["ep1"],
    items: {
      ep1: { id: "ep1", file: "01.mkv", status: "cancelled", reason: "Stopped by user" },
    },
  };

  tracker.restoreFromSnapshot(snapshotCancelled);
  assert.equal(tracker.state.batchCompleted, true);
  assert.equal(tracker.state.batchSuccess, false);
  assert.equal(tracker.state.batchCancelled, true);
});

test("restoreFromSnapshot sets meaningful restored tags instead of placeholder inspection text", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const snapshot = {
    job_id: 1,
    mode: "remux",
    status: "completed",
    ordered_item_ids: ["ep1", "ep2"],
    items: {
      ep1: { id: "ep1", file: "01.mkv", status: "completed", duration_sec: 4.5 },
      ep2: { id: "ep2", file: "02.mkv", status: "skipped", reason: "Up to date" },
    },
  };

  tracker.restoreFromSnapshot(snapshot);
  const card1 = tracker.cards.get("ep1");
  const card2 = tracker.cards.get("ep2");

  assert.equal(card1.tag1.textContent, "Processed");
  assert.equal(card1.tag2.textContent, "Done in 4.5s");
  assert.equal(card2.tag1.textContent, "Skipped");
  assert.equal(card2.tag2.textContent, "Up to date");
});

test("QueueTracker handles item_start (indeterminate shimmer), item_progress (determinate width), item_complete (green fill + stop heartbeat)", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const manifest = {
    version: 2,
    items: [
      { id: "ep1", name: "Show - 01.mkv", release: "Season 1" },
    ],
  };

  tracker.populateFromInspection(manifest);
  const card = tracker.cards.get("ep1");
  assert.ok(card);
  assert.equal(tracker.heartbeatTimer, null);

  // 1. item_start -> indeterminate shimmer + ticking text + starts heartbeat
  tracker.handleEvent({ version: 1, type: "batch_start", total: 1 });
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });

  assert.ok(card.el.className.includes("processing"));
  assert.equal(card.badge.textContent, "PROCESSING");
  assert.equal(card.fill.className, "ep-progress-fill indeterminate");
  assert.equal(card.fill.style.width, "");
  assert.match(card.statusTag.textContent, /^Processing \(\d+s\)$/);
  assert.notEqual(tracker.heartbeatTimer, null);

  // Tick heartbeat advances clock text
  tracker.state.items.get("ep1").startTime = Date.now() - 5000;
  tracker.tickHeartbeat();
  assert.equal(card.statusTag.textContent, "Processing (5s)");

  // 2. item_progress -> determinate width percentage
  tracker.handleEvent({
    version: 1,
    type: "item_progress",
    id: "ep1",
    progress: 64.2,
  });

  assert.equal(card.fill.className, "ep-progress-fill");
  assert.equal(card.fill.style.width, "64.2%");
  assert.equal(card.statusTag.textContent, "64% (5s)");

  // Tick heartbeat with progress still active
  tracker.state.items.get("ep1").startTime = Date.now() - 12000;
  tracker.tickHeartbeat();
  assert.equal(card.statusTag.textContent, "64% (12s)");

  // 3. item_complete -> green fill, Done in X.Xs, stops heartbeat
  tracker.handleEvent({
    version: 1,
    type: "item_complete",
    id: "ep1",
    status: "completed",
    duration_sec: 14.8,
  });

  assert.equal(card.el.className, "tokyo-ep-card completed");
  assert.equal(card.badge.textContent, "COMPLETED");
  assert.equal(card.fill.className, "ep-progress-fill green");
  assert.equal(card.fill.style.width, "100%");
  assert.equal(card.statusTag.textContent, "Done in 14.8s");
  assert.equal(tracker.heartbeatTimer, null);
});

test("QueueTracker stops heartbeat on finalize, reset, startScan, restoreFromSnapshot, and populateFromInspection", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const manifest = {
    version: 2,
    items: [{ id: "ep1", name: "01.mkv" }],
  };

  // 1. finalize stops heartbeat
  tracker.populateFromInspection(manifest);
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.finalize(0);
  assert.equal(tracker.heartbeatTimer, null);

  // 2. reset stops heartbeat
  tracker.populateFromInspection(manifest);
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.reset();
  assert.equal(tracker.heartbeatTimer, null);

  // 3. startScan stops heartbeat
  tracker.populateFromInspection(manifest);
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.startScan();
  assert.equal(tracker.heartbeatTimer, null);

  // 4. restoreFromSnapshot stops previous heartbeat
  tracker.populateFromInspection(manifest);
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.restoreFromSnapshot({
    job_id: 1,
    mode: "remux",
    status: "completed",
    ordered_item_ids: ["ep1"],
    items: {
      ep1: { id: "ep1", file: "01.mkv", status: "completed", duration_sec: 4.5 },
    },
  });
  assert.equal(tracker.heartbeatTimer, null);

  // 5. populateFromInspection stops previous heartbeat
  tracker.populateFromInspection(manifest);
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.populateFromInspection(manifest);
  assert.equal(tracker.heartbeatTimer, null);
});

test("restoreFromSnapshot formats duration with formatOutcomeDuration (>60s minutes formatting)", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const snapshot = {
    job_id: 1,
    mode: "remux",
    status: "completed",
    ordered_item_ids: ["ep1"],
    items: {
      ep1: { id: "ep1", file: "01.mkv", status: "completed", duration_sec: 75.4 },
    },
  };

  tracker.restoreFromSnapshot(snapshot);
  const card1 = tracker.cards.get("ep1");
  assert.equal(card1.tag2.textContent, "Done in 1m 15.4s");
});

test("active snapshot renders only submitted cards in order and keeps live updates during incremental rendering", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const list = elements.get("remux-episode-list");
  tracker.populateFromInspection({ version: 2, items: [
    { id: "01", name: "01.mkv" },
    { id: "02", name: "02.mkv" },
    { id: "03", name: "03.mkv" },
  ] });
  assert.equal(list.children.length, 3);

  const oldNow = Date.now;
  const oldPerformance = globalThis.performance;
  Date.now = () => 30_000;
  let clock = 0;
  globalThis.performance = { now: () => (clock += 10) }; // One card per animation frame.
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  t.after(() => {
    Date.now = oldNow;
    globalThis.performance = oldPerformance;
  });

  tracker.restoreFromSnapshot({
    job_id: 7,
    mode: "remux",
    status: "running",
    start_time_ms: 1000,
    ordered_item_ids: ["02", "01"],
    items: {
      "01": { id: "01", file: "01.mkv", status: "queued" },
      "03": { id: "03", file: "03.mkv", status: "queued" },
      "02": { id: "02", file: "02.mkv", status: "processing", progress: 70, start_time_ms: 25_000 },
    },
  });
  assert.equal(list.children.length, 0);
  frames.shift()();
  assert.deepEqual(list.children.map(el => el.querySelector(".ep-title").textContent), ["02.mkv"]);
  assert.equal(tracker.cards.get("02").fill.style.width, "70%");
  assert.equal(tracker.cards.get("02").statusTag.textContent, "70% (5s)");

  tracker.handleEvent({ version: 1, type: "item_complete", id: "01", status: "completed", duration_sec: 75.4 });
  tracker.handleEvent({ version: 1, type: "item_progress", id: "02", progress: 76 });
  frames.shift()();
  assert.deepEqual(list.children.map(el => el.querySelector(".ep-title").textContent), ["02.mkv", "01.mkv"]);
  assert.equal(tracker.cards.size, 2);
  assert.equal(tracker.cards.get("01").statusTag.textContent, "Done in 1m 15.4s");
  assert.equal(tracker.cards.get("02").fill.style.width, "76%");

  tracker.handleEvent({ version: 1, type: "item_complete", id: "02", status: "completed", duration_sec: 4.2 });
  assert.equal(tracker.cards.get("02").statusTag.textContent, "Done in 4.2s");
  assert.equal(tracker.cards.get("02").fill.style.width, "100%");
  assert.equal(tracker.heartbeatTimer, null);
});

test("QueueTracker renders <0.1s for instantaneous completed items with 0s duration", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const manifest = {
    version: 2,
    items: [{ id: "ep1", name: "01.mkv" }],
  };

  tracker.populateFromInspection(manifest);
  const card = tracker.cards.get("ep1");

  tracker.handleEvent({
    version: 1,
    type: "item_complete",
    id: "ep1",
    status: "completed",
    duration_sec: 0,
  });

  assert.equal(card.statusTag.textContent, "Done in <0.1s");
});

test("QueueTracker dynamically mounts card element in handleEvent if card not present but state item exists", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const manifest = {
    version: 2,
    items: [{ id: "ep1", name: "01.mkv" }],
  };
  tracker.state = createQueueState(manifest, { mode: "remux" });
  assert.equal(tracker.cards.has("ep1"), false);

  tracker.handleEvent({
    version: 1,
    type: "item_start",
    id: "ep1",
  });

  assert.equal(tracker.cards.has("ep1"), true);
  const card = tracker.cards.get("ep1");
  assert.equal(card.name, "01.mkv");
  const listEl = elements.get("remux-episode-list");
  assert.ok(listEl.children.includes(card.el));
  assert.equal(card.badge.textContent, "PROCESSING");
});

test("QueueTracker selection and card reordering", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  const manifest = {
    version: 2,
    items: [
      { id: "ep1", name: "01.mkv" },
      { id: "ep2", name: "02.mkv" },
      { id: "ep3", name: "03.mkv" },
    ],
  };

  tracker.populateFromInspection(manifest);
  const countEl = elements.get("remux-queue-count");
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1", "ep2", "ep3"]);

  // deselect all
  tracker.deselectAll();
  assert.equal(countEl.textContent, "0 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), []);

  // select all
  tracker.selectAll();
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1", "ep2", "ep3"]);

  // reorder
  tracker.reorderCard("ep2", "up");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep2", "ep1", "ep3"]);
  assert.equal(tracker.cards.get("ep2").badge.textContent, "EPISODE 01");
  assert.equal(tracker.cards.get("ep1").badge.textContent, "EPISODE 02");
});

test("QueueTracker reconciles same-directory replacements, cached reuse, stale scans, and fresh directories", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  const renderFrames = () => { while (frames.length) frames.shift()(); };
  const manifest = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const tracker = new QueueTracker("remux");
  const dir = "/input/show";
  const first = manifest(["Season/01.mkv", "Season/02.mkv", "Season/gone.mkv"]);
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, first), true);
  renderFrames();

  tracker.reorderCard("Season/02.mkv", "up");
  tracker.state.items.get("Season/01.mkv").selected = false;
  tracker.assignments.set("Season/02.mkv", "Season/02.ass");
  tracker.assignments.set("Season/gone.mkv", "Season/gone.ass");
  const stale = tracker.startScan();
  const current = tracker.startScan();
  assert.equal(tracker.acceptInspection(stale, dir, manifest(["Season/gone.mkv"])), false);
  assert.equal(tracker.acceptInspection(current, dir, manifest(["Season/01.mkv", "Season/03.mkv", "Season/02.mkv"])), true);
  renderFrames();

  assert.deepEqual(tracker.state.order, ["Season/02.mkv", "Season/01.mkv", "Season/03.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv"]);
  assert.equal(elements.get("remux-queue-count").textContent, "1 / 3 Selected");
  assert.equal(tracker.cards.get("Season/02.mkv").checkbox.checked, true);
  assert.equal(tracker.cards.get("Season/01.mkv").checkbox.checked, false);
  assert.equal(tracker.cards.get("Season/03.mkv").checkbox.checked, false);
  assert.deepEqual([...tracker.assignments.keys()], ["Season/02.mkv"]);

  tracker.reorderCard("Season/03.mkv", "up");
  tracker.state.items.get("Season/03.mkv").selected = true;
  assert.equal(tracker.reuseInspection(dir), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv", "Season/03.mkv"]);
  assert.equal(tracker.cards.get("Season/03.mkv").checkbox.checked, true);

  assert.equal(tracker.acceptInspection(tracker.startScan(), "/input/other", manifest(["Season/02.mkv", "Other/04.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv", "Other/04.mkv"]);
  assert.equal(tracker.cards.get("Other/04.mkv").checkbox.checked, true);
  assert.equal(tracker.assignments.size, 0);
});
test("QueueTracker preserves same-directory selection after failed scan and snapshot reload", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  const renderFrames = () => { while (frames.length) frames.shift()(); };
  const manifest = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const tracker = new QueueTracker("remux");
  const dir = "/input/show";

  // Initial scan of /input/show
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifest(["01.mkv", "02.mkv", "03.mkv"])), true);
  renderFrames();
  tracker.reorderCard("02.mkv", "up");
  tracker.state.items.get("01.mkv").selected = false;
  tracker.assignments.set("02.mkv", "02.ass");

  // Scan fails (e.g. invalid manifest or rejection)
  const failedScan = tracker.startScan();
  tracker.rejectInspection(failedScan);
  assert.equal(tracker.currentDirectory, dir);

  // Subsequent scan of the same directory must preserve surviving selection and order
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifest(["01.mkv", "02.mkv", "04.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "04.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);
  assert.deepEqual([...tracker.assignments.keys()], ["02.mkv"]);

  // Snapshot restore carries directory identity
  tracker.restoreFromSnapshot({
    job_id: 1,
    mode: "remux",
    status: "completed",
    input_dir: dir,
    ordered_item_ids: ["02.mkv", "01.mkv"],
    items: {
      "02.mkv": { id: "02.mkv", file: "02.mkv", status: "completed" },
      "01.mkv": { id: "01.mkv", file: "01.mkv", status: "completed" },
    },
  });
  assert.equal(tracker.currentDirectory, dir);

  // Re-scanning same directory after snapshot reload preserves selection
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifest(["01.mkv", "02.mkv", "05.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "05.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv", "01.mkv"]);
});

test("QueueTracker.reuseInspection returns false after restoreFromSnapshot when lastManifest is null", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const dir = "/input/show";
  tracker.restoreFromSnapshot({
    job_id: 1,
    mode: "remux",
    status: "running",
    input_dir: dir,
    ordered_item_ids: ["01.mkv"],
    items: {
      "01.mkv": { id: "01.mkv", file: "01.mkv", status: "processing" },
    },
  });
  assert.equal(tracker.cards.size, 1);
  assert.equal(tracker.lastManifest, null);
  assert.equal(tracker.reuseInspection(dir), false);
  assert.equal(tracker.cards.size, 1);
});

test("QueueTracker renders cards when BatchStart arrives after restoring empty running snapshot", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  const renderFrames = () => { while (frames.length) frames.shift()(); };
  const tracker = new QueueTracker("remux");

  // Reload before BatchStart arrives: running snapshot is empty
  tracker.restoreFromSnapshot({
    job_id: 99,
    mode: "remux",
    status: "running",
    input_dir: "/anime/show",
    ordered_item_ids: [],
    items: {},
  });
  renderFrames();
  assert.equal(tracker.cards.size, 0);

  // BatchStart arrives with items
  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 2,
    input_dir: "/anime/show",
    items: [
      { id: "02.mkv", file: "02.mkv" },
      { id: "01.mkv", file: "01.mkv" },
    ],
  });
  renderFrames();

  // Cards must be rendered from the ordered state
  assert.equal(tracker.cards.size, 2);
  assert.ok(tracker.cards.has("02.mkv"));
  assert.ok(tracker.cards.has("01.mkv"));
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv"]);
});
test("QueueTracker does not render duplicate cards when BatchStart arrives before initial snapshot frame flushes", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => {
    frames.push(callback);
    return frames.length;
  };
  globalThis.cancelAnimationFrame = id => {
    const idx = id - 1;
    if (frames[idx]) {
      frames[idx] = () => {};
    }
  };
  const renderFrames = () => {
    while (frames.length) frames.shift()();
  };
  const tracker = new QueueTracker("remux");

  // Restore running snapshot with 0 items without flushing frames yet
  tracker.restoreFromSnapshot({
    job_id: 100,
    mode: "remux",
    status: "running",
    input_dir: "/anime/show",
    ordered_item_ids: [],
    items: {},
  });

  // BatchStart arrives immediately while initial restore frame is still pending
  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 2,
    input_dir: "/anime/show",
    items: [
      { id: "02.mkv", file: "02.mkv" },
      { id: "01.mkv", file: "01.mkv" },
    ],
  });

  // Now flush all queued animation frames
  renderFrames();

  // Exactly two cards must exist in tracker.cards and in the DOM list, with no duplicates
  const listEl = elements.get("remux-episode-list");
  assert.equal(tracker.cards.size, 2);
  assert.equal(listEl.children.length, 2);
  assert.ok(tracker.cards.has("02.mkv"));
  assert.ok(tracker.cards.has("01.mkv"));
});
test("QueueTracker preserves selection across symlinked directory rescan and BatchStart", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  const renderFrames = () => { while (frames.length) frames.shift()(); };
  const tracker = new QueueTracker("remux");

  // In real runtime, a symlink alias resolves to the canonical directory on disk
  const realDir = "/var/media/anime/show";

  // Initial scan passes canonical_dir as realDir
  const manifest = ids => ({
    version: 2,
    canonical_dir: realDir,
    items: ids.map(id => ({ id, name: id })),
  });

  // User scanned via symlink alias; acceptedPath was resolved to realDir
  assert.equal(tracker.acceptInspection(tracker.startScan(), realDir, manifest(["01.mkv", "02.mkv"])), true);
  renderFrames();

  // User reorders and deselects 01.mkv
  tracker.reorderCard("02.mkv", "up");
  tracker.state.items.get("01.mkv").selected = false;
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);

  // BatchStart arrives carrying the canonical input_dir
  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 2,
    input_dir: realDir,
    items: [
      { id: "02.mkv", file: "02.mkv" },
      { id: "01.mkv", file: "01.mkv" },
    ],
  });
  assert.equal(tracker.currentDirectory, realDir);

  // Subsequent rescan with newly discovered item 03.mkv
  assert.equal(
    tracker.acceptInspection(
      tracker.startScan(),
      realDir,
      manifest(["01.mkv", "02.mkv", "03.mkv"])
    ),
    true
  );
  renderFrames();

  // Order and selection preserved across BatchStart: 02.mkv first (selected), 01.mkv (unselected), 03.mkv (appended unselected)
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "03.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);
  assert.equal(tracker.state.items.get("01.mkv").selected, false);
  assert.equal(tracker.state.items.get("03.mkv").selected, false);

  // Snapshot restore carries canonical directory identity
  tracker.restoreFromSnapshot({
    job_id: 10,
    mode: "remux",
    status: "completed",
    input_dir: realDir,
    ordered_item_ids: ["02.mkv", "01.mkv"],
    items: {
      "02.mkv": { id: "02.mkv", file: "02.mkv", status: "completed" },
      "01.mkv": { id: "01.mkv", file: "01.mkv", status: "completed" },
    },
  });
  assert.equal(tracker.currentDirectory, realDir);

  // Rescan after snapshot restore preserves directory identity and appends new items unselected
  assert.equal(
    tracker.acceptInspection(
      tracker.startScan(),
      realDir,
      manifest(["01.mkv", "02.mkv", "04.mkv"])
    ),
    true
  );
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "04.mkv"]);
  assert.equal(tracker.state.items.get("04.mkv").selected, false);
});

test("escapeAttr escapes &, \", ', <, and >", () => {
  assert.equal(escapeAttr("hello"), "hello");
  assert.equal(
    escapeAttr('"><script>alert(1)</script><input attr=\'evil\'&'),
    '&quot;&gt;&lt;script&gt;alert(1)&lt;/script&gt;&lt;input attr=&#39;evil&#39;&amp;'
  );
});

test("subset batch progress counts only batch participants", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const countEl = elements.get("remux-queue-count");
  const inputDir = "/anime/show";

  const manifest = {
    version: 2,
    items: [
      { id: "ep1", name: "01.mkv" },
      { id: "ep2", name: "02.mkv" },
      { id: "ep3", name: "03.mkv" },
    ],
  };

  tracker.populateFromInspection(manifest);
  toggleItemSelection(tracker.state, "ep2", false);
  toggleItemSelection(tracker.state, "ep3", false);

  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 1,
    input_dir: inputDir,
    items: [{ id: "ep1", file: "ep1" }],
  });
  assert.ok(countEl.textContent.includes("0 / 1 Processed"));

  tracker.handleEvent({
    version: 1,
    type: "item_start",
    id: "ep1",
  });
  tracker.handleEvent({
    version: 1,
    type: "item_complete",
    id: "ep1",
    status: "completed",
    duration_sec: 14.8,
  });
  tracker.handleEvent({
    version: 1,
    type: "batch_complete",
    success: true,
  });
  tracker.finalize(0);

  assert.ok(countEl.textContent.includes("(1 Total)"));
  assert.ok(countEl.textContent.includes("1 Completed"));
});

test("same-directory empty inspection keeps directory identity, vanished selections, and order", t => {
  const { elements } = setupMockEnvironment(t);
  const frames = [];
  globalThis.requestAnimationFrame = callback => frames.push(callback);
  const renderFrames = () => { while (frames.length) frames.shift()(); };
  const manifest = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifest(["ep1", "ep2"])), true);
  renderFrames();
  tracker.reorderCard("ep2", "up");

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, { version: 2, items: [] }), true);
  renderFrames();
  assert.equal(tracker.reuseInspection(dir), true);
  renderFrames();

  assert.equal(tracker.currentDirectory, dir);
  assert.equal(tracker.lastScannedPath, dir);
  assert.deepEqual(tracker.state.vanishedSelectedIds, ["ep2", "ep1"]);

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifest(["ep1", "ep2"])), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep2", "ep1"]);
});

