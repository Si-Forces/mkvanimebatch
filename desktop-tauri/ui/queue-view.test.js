import test from "node:test";
import assert from "node:assert/strict";
import { QueueTracker, RING_CIRCUMFERENCE } from "./queue-view.js";
import { createQueueState, setItemSelected } from "./queue-state.js";

/** The status ring's dash offset at `progress` percent. */
const ringOffset = progress => (RING_CIRCUMFERENCE * (1 - progress / 100)).toFixed(2);

class FakeElement {
  constructor(value = "") {
    this.value = value;
    this.checked = false;
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
      contains(cls) { return this._classes.has(cls); },
    };
  }

  append(...nodes) {
    for (const node of nodes) this.appendChild(node);
  }

  /** Like the DOM's, this moves `node` when it is already in the tree. */
  before(node) {
    node.remove();
    const siblings = this.parent.children;
    node.parent = this.parent;
    siblings.splice(siblings.indexOf(this), 0, node);
  }

  set innerHTML(markup) {
    this._innerHTML = markup;
    this.children = [];
  }

  get innerHTML() { return this._innerHTML; }

  appendChild(child) {
    const added = child.isFragment ? child.children : [child];
    for (const el of added) el.parent = this;
    this.children.push(...added);
    return child;
  }

  replaceWith(other) {
    const siblings = this.parent.children;
    siblings[siblings.indexOf(this)] = other;
    other.parent = this.parent;
    this.parent = null;
  }

  addEventListener(type, handler) { (this.handlers ??= new Map()).set(type, handler); }

  remove() {
    const siblings = this.parent?.children;
    if (siblings?.includes(this)) siblings.splice(siblings.indexOf(this), 1);
    this.parent = null;
  }

  querySelector(selector) {
    if (!this.queried.has(selector)) {
      const el = new FakeElement();
      if (selector.startsWith(".")) el.className = selector.slice(1);
      this.queried.set(selector, el);
    }
    return this.queried.get(selector);
  }
}

/**
 * Install a fake DOM. With `deferFrames`, animation frames queue until
 * `renderFrame()`/`renderFrames()` run them; otherwise they run synchronously.
 */
function setupMockEnvironment(t, { deferFrames = false } = {}) {
  const previous = {
    document: globalThis.document,
    requestAnimationFrame: globalThis.requestAnimationFrame,
    cancelAnimationFrame: globalThis.cancelAnimationFrame,
  };

  const elements = new Map([
    ["remux-episode-list", new FakeElement()],
    ["remux-queue-count", new FakeElement()],
    ["remux-bitrate", new FakeElement("192k")],
    ["remux-filter-empty", new FakeElement()],
  ]);

  globalThis.document = {
    getElementById: id => elements.get(id) ?? null,
    createElement: () => new FakeElement(),
    createDocumentFragment: () => Object.assign(new FakeElement(), { isFragment: true }),
  };

  const frames = new Map();
  let nextFrameId = 0;
  if (deferFrames) {
    globalThis.requestAnimationFrame = callback => {
      frames.set(++nextFrameId, callback);
      return nextFrameId;
    };
    globalThis.cancelAnimationFrame = id => frames.delete(id);
  } else {
    globalThis.requestAnimationFrame = callback => callback();
  }
  const renderFrame = () => {
    const [id, callback] = frames.entries().next().value;
    frames.delete(id);
    callback();
  };
  const renderFrames = () => {
    while (frames.size) renderFrame();
  };

  t.after(() => Object.assign(globalThis, previous));

  return { elements, renderFrame, renderFrames };
}

const manifestOf = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });

/** A manual subtitle assignment in the shape app.js stores in `QueueTracker.assignments`. */
const subtitle = (id, path) => ({
  target_video: id,
  asset_path: path,
  track_type: "subtitle",
  language: "ind",
  title: null,
  offset_ms: null,
});

const cardTitles = list => list.children.map(el => el.querySelector(".ep-title").textContent);

/** A manifest whose items carry the track metadata a card renders. */
const detailedManifest = ids => ({
  version: 2,
  subtitle_candidates: {},
  items: ids.map(id => ({
    id,
    name: `Show - ${id}`,
    release: ".",
    metadata: {
      resolution: "1920x1080",
      video_codec: "HEVC",
      video_tag: "1080p HEVC",
      audio_summary: "FLAC 2.0",
      has_flac: true,
      sub_summary: "ASS (ind)",
      fonts_count: 3,
    },
  })),
});

/** Log the name of every write to `props` on `target`; reads keep returning the last value. */
function recordWrites(target, label, props, log) {
  for (const prop of props) {
    let value = target[prop];
    Object.defineProperty(target, prop, {
      configurable: true,
      get: () => value,
      set: next => {
        log.push(`${label}.${prop}`);
        value = next;
      },
    });
  }
}

test("restoreFromSnapshot renders a card per restored item in its outcome state", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  tracker.restoreFromSnapshot({
    job_id: 1,
    mode: "remux",
    status: "completed",
    ordered_item_ids: ["ep1", "ep2", "ep3", "ep4"],
    items: {
      ep1: { id: "ep1", file: "Show - 01.mkv", status: "completed", duration_sec: 10.2 },
      ep2: { id: "ep2", file: "Show - 02.mkv", status: "skipped", reason: "Already up to date" },
      ep3: { id: "ep3", file: "Show - 03.mkv", status: "cancelled", reason: "User cancelled" },
      ep4: { id: "ep4", file: "Show - 04.mkv", status: "error", error: "FFmpeg encoding failed" },
    },
  });

  assert.equal(tracker.cards.size, 4);
  const card1 = tracker.cards.get("ep1");
  assert.equal(card1.el.dataset.status, "completed");
  assert.equal(card1.status.className, "ep-status tone-ok");
  assert.equal(card1.statusTag.textContent, "Done in 10.2s");

  const card2 = tracker.cards.get("ep2");
  assert.equal(card2.status.className, "ep-status tone-mute");
  assert.equal(card2.statusTag.textContent, "Skipped");
  assert.equal(card2.status.title, "Already up to date");

  const card4 = tracker.cards.get("ep4");
  assert.equal(card4.el.dataset.status, "error");
  assert.equal(card4.status.className, "ep-status tone-err");
  assert.equal(card4.statusTag.textContent, "Failed");
  assert.equal(card4.status.title, "FFmpeg encoding failed");
});

test("restoreFromSnapshot sets meaningful restored tags instead of placeholder inspection text", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  tracker.restoreFromSnapshot({
    job_id: 1,
    mode: "remux",
    status: "completed",
    ordered_item_ids: ["ep1", "ep2"],
    items: {
      ep1: { id: "ep1", file: "01.mkv", status: "completed", duration_sec: 4.5 },
      ep2: { id: "ep2", file: "02.mkv", status: "skipped", reason: "Up to date" },
    },
  });
  const card1 = tracker.cards.get("ep1");
  const card2 = tracker.cards.get("ep2");

  assert.equal(card1.meta.textContent, "Processed · Done in 4.5s");
  assert.equal(card2.meta.textContent, "Skipped · Up to date");
});

test("QueueTracker handles item_start (indeterminate shimmer), item_progress (determinate width), item_complete (green fill; heartbeat stops on its next tick)", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  tracker.populateFromInspection({
    version: 2,
    items: [{ id: "ep1", name: "Show - 01.mkv", release: "Season 1" }],
  });
  const card = tracker.cards.get("ep1");
  assert.ok(card);
  assert.equal(tracker.heartbeatTimer, null);

  // 1. item_start -> blue spinning ring + ticking text + starts heartbeat
  tracker.handleEvent({ version: 1, type: "batch_start", total: 1 });
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });

  assert.equal(card.el.dataset.status, "processing");
  assert.equal(card.status.className, "ep-status tone-info is-spinning");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.match(card.statusTag.textContent, /^Processing · \d+s$/);
  assert.notEqual(tracker.heartbeatTimer, null);

  // Tick heartbeat advances clock text
  tracker.state.items.get("ep1").startTime = Date.now() - 5000;
  tracker.tickHeartbeat();
  assert.equal(card.statusTag.textContent, "Processing · 5s");

  // 2. item_progress -> determinate ring
  tracker.handleEvent({
    version: 1,
    type: "item_progress",
    id: "ep1",
    progress: 64.2,
  });

  assert.equal(card.status.className, "ep-status tone-info");
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(64.2));
  assert.equal(card.statusTag.textContent, "64% · 5s");

  tracker.state.items.get("ep1").startTime = Date.now() - 12000;
  tracker.tickHeartbeat();
  assert.equal(card.statusTag.textContent, "64% · 12s");

  // 3. item_complete -> green, Done in X.Xs, stops heartbeat
  tracker.handleEvent({
    version: 1,
    type: "item_complete",
    id: "ep1",
    status: "completed",
    duration_sec: 14.8,
  });

  assert.equal(card.el.dataset.status, "completed");
  assert.equal(card.status.className, "ep-status tone-ok");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.equal(card.statusTag.textContent, "Done in 14.8s");
  // The heartbeat stops itself on its next tick once nothing is processing.
  assert.notEqual(tracker.heartbeatTimer, null);
  tracker.tickHeartbeat();
  assert.equal(tracker.heartbeatTimer, null);
});

test("QueueTracker stops heartbeat on endBatch, reset, startScan, restoreFromSnapshot, and populateFromInspection", t => {
  setupMockEnvironment(t);
  // The item below is left open when endBatch runs, which the Item Settlement guard warns about.
  t.mock.method(console, "warn", () => {});
  const tracker = new QueueTracker("remux");
  const manifest = manifestOf(["ep1"]);
  const startProcessing = () => {
    tracker.populateFromInspection(manifest);
    tracker.handleEvent({ version: 1, type: "batch_start", total: 1, items: [{ id: "ep1", file: "ep1" }] });
    tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
    assert.notEqual(tracker.heartbeatTimer, null);
  };

  startProcessing();
  tracker.endBatch(false);
  assert.equal(tracker.heartbeatTimer, null);
  assert.equal(tracker.state.items.get("ep1").status, "error");

  startProcessing();
  tracker.reset();
  assert.equal(tracker.heartbeatTimer, null);

  startProcessing();
  tracker.startScan();
  assert.equal(tracker.heartbeatTimer, null);

  startProcessing();
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

  startProcessing();
  tracker.populateFromInspection(manifest);
  assert.equal(tracker.heartbeatTimer, null);
});

test("active snapshot renders only submitted cards in order and keeps live updates during incremental rendering", t => {
  const { elements, renderFrame, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const list = elements.get("remux-episode-list");
  tracker.populateFromInspection(manifestOf(["01", "02", "03"]));
  renderFrames();
  assert.equal(list.children.length, 3);

  const oldNow = Date.now;
  const oldPerformance = globalThis.performance;
  Date.now = () => 30_000;
  let clock = 0;
  globalThis.performance = { now: () => (clock += 10) }; // One card per animation frame.
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
  renderFrame();
  assert.deepEqual(cardTitles(list), ["02.mkv"]);
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(70));
  assert.equal(tracker.cards.get("02").statusTag.textContent, "70% · 5s");

  tracker.handleEvent({ version: 1, type: "item_complete", id: "01", status: "completed", duration_sec: 75.4 });
  tracker.handleEvent({ version: 1, type: "item_progress", id: "02", progress: 76 });
  renderFrame();
  assert.deepEqual(cardTitles(list), ["02.mkv", "01.mkv"]);
  assert.equal(tracker.cards.size, 2);
  assert.equal(tracker.cards.get("01").statusTag.textContent, "Done in 1m 15.4s");
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(76));

  tracker.handleEvent({ version: 1, type: "item_complete", id: "02", status: "completed", duration_sec: 4.2 });
  assert.equal(tracker.cards.get("02").statusTag.textContent, "Done in 4.2s");
  assert.equal(tracker.cards.get("02").status.className, "ep-status tone-ok");
  tracker.tickHeartbeat();
  assert.equal(tracker.heartbeatTimer, null);
});

test("QueueTracker renders <0.1s for instantaneous completed items with 0s duration", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  tracker.populateFromInspection(manifestOf(["ep1"]));
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

  tracker.state = createQueueState({ version: 2, items: [{ id: "ep1", name: "01.mkv" }] });
  assert.equal(tracker.cards.has("ep1"), false);

  tracker.handleEvent({
    version: 1,
    type: "item_start",
    id: "ep1",
  });

  assert.equal(tracker.cards.has("ep1"), true);
  const card = tracker.cards.get("ep1");
  assert.equal(card.el.querySelector(".ep-title").textContent, "01.mkv");
  assert.ok(elements.get("remux-episode-list").children.includes(card.el));
  assert.equal(card.el.dataset.status, "processing");
});

test("QueueTracker selection and card reordering", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");

  tracker.populateFromInspection(manifestOf(["ep1", "ep2", "ep3"]));
  const countEl = elements.get("remux-queue-count");
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1", "ep2", "ep3"]);

  tracker.deselectAll();
  assert.equal(countEl.textContent, "0 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), []);
  assert.equal(tracker.cards.get("ep1").checkbox.checked, false);

  tracker.selectAll();
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1", "ep2", "ep3"]);
  assert.equal(tracker.cards.get("ep1").checkbox.checked, true);

  tracker.reorderCard("ep2", "up");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep2", "ep1", "ep3"]);
  assert.equal(tracker.cards.get("ep2").num.textContent, "01");
  assert.equal(tracker.cards.get("ep1").num.textContent, "02");
});

test("QueueTracker reconciles same-directory replacements, cached reuse, stale scans, and fresh directories", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/input/show";
  const first = manifestOf(["Season/01.mkv", "Season/02.mkv", "Season/gone.mkv"]);
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, first), true);
  renderFrames();

  tracker.reorderCard("Season/02.mkv", "up");
  tracker.state.items.get("Season/01.mkv").selected = false;
  tracker.assignments.set("Season/02.mkv", subtitle("Season/02.mkv", "Season/02.ass"));
  tracker.assignments.set("Season/gone.mkv", subtitle("Season/gone.mkv", "Season/gone.ass"));
  const stale = tracker.startScan();
  const current = tracker.startScan();
  assert.equal(tracker.acceptInspection(stale, dir, manifestOf(["Season/gone.mkv"])), false);
  assert.equal(tracker.acceptInspection(current, dir, manifestOf(["Season/01.mkv", "Season/03.mkv", "Season/02.mkv"])), true);
  renderFrames();

  assert.deepEqual(tracker.state.order, ["Season/02.mkv", "Season/01.mkv", "Season/03.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv"]);
  assert.equal(elements.get("remux-queue-count").textContent, "1 / 3 Selected");
  assert.equal(tracker.cards.get("Season/02.mkv").checkbox.checked, true);
  assert.equal(tracker.cards.get("Season/01.mkv").checkbox.checked, false);
  assert.equal(tracker.cards.get("Season/03.mkv").checkbox.checked, false);
  assert.deepEqual([...tracker.assignments.keys()], ["Season/02.mkv"]);
  // The carried assignment is what the inspector's ladder reports as used.
  assert.equal(tracker.ladderFor("Season/02.mkv")[0].tag, "Used");
  assert.equal(tracker.ladderFor("Season/01.mkv")[0].tag, "None");

  tracker.reorderCard("Season/03.mkv", "up");
  tracker.state.items.get("Season/03.mkv").selected = true;
  assert.equal(tracker.reuseInspection(dir), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv", "Season/03.mkv"]);
  assert.equal(tracker.cards.get("Season/03.mkv").checkbox.checked, true);

  assert.equal(tracker.acceptInspection(tracker.startScan(), "/input/other", manifestOf(["Season/02.mkv", "Other/04.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["Season/02.mkv", "Other/04.mkv"]);
  assert.equal(tracker.cards.get("Other/04.mkv").checkbox.checked, true);
  assert.equal(tracker.assignments.size, 0);
});

test("QueueTracker preserves same-directory selection after failed scan and snapshot reload", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/input/show";

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "03.mkv"])), true);
  renderFrames();
  tracker.reorderCard("02.mkv", "up");
  tracker.state.items.get("01.mkv").selected = false;
  tracker.assignments.set("02.mkv", subtitle("02.mkv", "02.ass"));

  tracker.rejectInspection(tracker.startScan());
  assert.equal(tracker.currentDirectory, dir);

  // A later scan of the same directory preserves surviving selection and order.
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "04.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "04.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);
  assert.deepEqual([...tracker.assignments.keys()], ["02.mkv"]);

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

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "05.mkv"])), true);
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
  assert.equal(tracker.reuseInspection(dir), false);
  assert.equal(tracker.cards.size, 1);
});

test("QueueTracker renders cards when BatchStart arrives after restoring empty running snapshot", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
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

  assert.deepEqual(cardTitles(elements.get("remux-episode-list")), ["02.mkv", "01.mkv"]);
  assert.equal(tracker.cards.size, 2);
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv"]);
});

test("QueueTracker does not render duplicate cards when BatchStart arrives before initial snapshot frame flushes", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");

  tracker.restoreFromSnapshot({
    job_id: 100,
    mode: "remux",
    status: "running",
    input_dir: "/anime/show",
    ordered_item_ids: [],
    items: {},
  });

  // BatchStart arrives while the restore's frames are still unflushed.
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

  assert.equal(tracker.cards.size, 2);
  assert.equal(elements.get("remux-episode-list").children.length, 2);
  assert.ok(tracker.cards.has("02.mkv"));
  assert.ok(tracker.cards.has("01.mkv"));
});

test("QueueTracker preserves selection and order across BatchStart and snapshot restore for same-directory rescans", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/var/media/anime/show";

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv"])), true);
  renderFrames();

  tracker.reorderCard("02.mkv", "up");
  tracker.state.items.get("01.mkv").selected = false;
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);

  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 2,
    input_dir: dir,
    items: [
      { id: "02.mkv", file: "02.mkv" },
      { id: "01.mkv", file: "01.mkv" },
    ],
  });
  assert.equal(tracker.currentDirectory, dir);

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "03.mkv"])), true);
  renderFrames();

  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "03.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv"]);
  assert.equal(tracker.state.items.get("01.mkv").selected, false);
  assert.equal(tracker.state.items.get("03.mkv").selected, false);

  tracker.restoreFromSnapshot({
    job_id: 10,
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

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "04.mkv"])), true);
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "04.mkv"]);
  assert.equal(tracker.state.items.get("04.mkv").selected, false);
});

test("QueueTracker keeps a reload-then-BatchStart Active Batch as the ordered, selected base for the next same-directory rescan", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";

  // Reload lands before BatchStart, so the snapshot knows nothing about the batch.
  tracker.restoreFromSnapshot({
    job_id: 7,
    mode: "remux",
    status: "running",
    input_dir: dir,
    ordered_item_ids: [],
    items: {},
  });
  renderFrames();
  assert.equal(tracker.state.order.length, 0);

  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 2,
    input_dir: dir,
    items: [
      { id: "02.mkv", file: "02.mkv" },
      { id: "01.mkv", file: "01.mkv" },
    ],
  });
  renderFrames();
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv"]);
  assert.deepEqual(tracker.state.knownOrder, ["02.mkv", "01.mkv"]);

  // The batch settles, then the same directory is rescanned with one more episode.
  tracker.handleEvent({
    version: 1,
    type: "batch_complete",
    total: 2,
    success: true,
    cancelled: false,
  });
  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["01.mkv", "02.mkv", "03.mkv"])), true);
  renderFrames();

  // Submitter order survives, both participants stay selected, the newcomer does not.
  assert.deepEqual(tracker.state.order, ["02.mkv", "01.mkv", "03.mkv"]);
  assert.deepEqual(tracker.state.knownOrder, ["02.mkv", "01.mkv", "03.mkv"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["02.mkv", "01.mkv"]);
  assert.equal(tracker.state.items.get("03.mkv").selected, false);
});

test("subset batch progress counts only batch participants", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const countEl = elements.get("remux-queue-count");

  tracker.populateFromInspection(manifestOf(["ep1", "ep2", "ep3"]));
  setItemSelected(tracker.state, "ep2", false);
  setItemSelected(tracker.state, "ep3", false);

  tracker.handleEvent({
    version: 1,
    type: "batch_start",
    total: 1,
    input_dir: "/anime/show",
    items: [{ id: "ep1", file: "ep1" }],
  });
  assert.equal(countEl.textContent, "0 of 1 · 0 done");

  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  tracker.handleEvent({
    version: 1,
    type: "item_complete",
    id: "ep1",
    status: "completed",
    duration_sec: 14.8,
  });
  tracker.handleEvent({ version: 1, type: "batch_complete", success: true });
  tracker.endBatch(false);

  assert.equal(countEl.textContent, "1 of 1 · 1 done");
});

test("a job that fails before BatchStart leaves the queue's cards, selection and count as they were", t => {
  const { elements } = setupMockEnvironment(t);
  const warn = t.mock.method(console, "warn", () => {});
  const tracker = new QueueTracker("remux");
  const countEl = elements.get("remux-queue-count");
  tracker.populateFromInspection(manifestOf(["ep1", "ep2"]));
  setItemSelected(tracker.state, "ep2", false);

  // The Engine Bridge's terminal for a request the engine refused, then the job's end.
  tracker.handleEvent({ version: 1, type: "batch_complete", success: false, cancelled: false, error: "bad request" });
  tracker.endBatch(false);

  assert.deepEqual([...tracker.state.items.values()].map(item => item.status), ["queued", "queued"]);
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1"]);
  assert.equal(countEl.textContent, "1 / 2 Selected");
  assert.equal(warn.mock.callCount(), 0);
});

test("same-directory empty inspection keeps directory identity, vanished selections, and order", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["ep1", "ep2"])), true);
  renderFrames();
  tracker.reorderCard("ep2", "up");

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf([])), true);
  renderFrames();
  assert.equal(tracker.reuseInspection(dir), true);
  renderFrames();

  assert.equal(tracker.currentDirectory, dir);
  assert.equal(tracker.lastScannedPath, dir);
  assert.deepEqual(tracker.state.vanishedSelectedIds, ["ep2", "ep1"]);

  assert.equal(tracker.acceptInspection(tracker.startScan(), dir, manifestOf(["ep1", "ep2"])), true);
  renderFrames();
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep2", "ep1"]);
});

test("a same-directory re-inspection with an identical manifest keeps every card and never shows the scanning placeholder", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const list = elements.get("remux-episode-list");
  const dir = "/anime/show";
  const ids = ["01.mkv", "02.mkv", "03.mkv"];
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(ids)), true);
  renderFrames();

  // A finished batch leaves a terminal card; a deselected card shows its checkbox off.
  tracker.handleEvent({ version: 1, type: "item_complete", id: "01.mkv", status: "completed", duration_sec: 4.2 });
  tracker.cards.get("02.mkv").checkbox.checked = false;
  tracker.state.items.get("02.mkv").selected = false;
  const cardsBefore = new Map(tracker.cards);
  const elementsBefore = [...list.children];
  const listWrites = [];
  recordWrites(list, "list", ["innerHTML"], listWrites);

  // Every Run forces a re-inspection of the shown directory; a deep-equal copy arrives.
  const token = tracker.startScan(dir);
  elementsBefore.forEach((el, i) => assert.equal(list.children[i], el));
  assert.equal(list.children.length, elementsBefore.length);
  assert.equal(elements.get("remux-queue-count").textContent, "Scanning…");
  assert.equal(tracker.acceptInspection(token, dir, structuredClone(detailedManifest(ids))), true);
  renderFrames();

  assert.deepEqual(listWrites, []);
  elementsBefore.forEach((el, i) => assert.equal(list.children[i], el));
  assert.equal(list.children.length, elementsBefore.length);
  for (const id of ids) assert.equal(tracker.cards.get(id), cardsBefore.get(id));
  assert.equal(tracker.cards.get("02.mkv").checkbox.checked, false);
  // The re-inspected item is queued again, so its kept card is back to the queued look.
  const card1 = tracker.cards.get("01.mkv");
  assert.equal(card1.el.dataset.status, "queued");
  assert.equal(card1.status.className, "ep-status tone-mute");
  assert.equal(card1.num.textContent, "01");
  assert.equal(card1.statusTag.textContent, "Queued");
  assert.equal(card1.arc.style.strokeDashoffset, "");
});

test("startScan keeps the cards of the same Directory Identity and clears them for any other", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const list = elements.get("remux-episode-list");
  const dir = "/anime/show";
  const showing = () => {
    const tracker = new QueueTracker("remux");
    assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv", "02.mkv"])), true);
    renderFrames();
    return tracker;
  };

  const same = showing();
  const cardsBefore = [...list.children];
  same.startScan(dir);
  assert.equal(same.cards.size, 2);
  assert.equal(list.children.length, cardsBefore.length);
  cardsBefore.forEach((el, i) => assert.equal(list.children[i], el));

  // Identities compare byte for byte: another spelling of the folder is another directory.
  for (const other of ["/anime/other", "/anime/show/"]) {
    const tracker = showing();
    tracker.startScan(other);
    assert.equal(tracker.cards.size, 0);
    assert.equal(list.children.length, 0);
    assert.match(list.innerHTML, /Scanning folder/);
  }
});

test("a freshly mounted card is drawn in the queued look by its first renderCardState", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv", "02.mkv"])), true);
  renderFrames();

  const card = tracker.cards.get("02.mkv");
  assert.equal(card.renderedStatus, "queued");
  assert.equal(card.el.dataset.status, "queued");
  assert.equal(card.status.className, "ep-status tone-mute");
  assert.equal(card.num.textContent, "02");
  assert.equal(card.statusTag.textContent, "Queued");
});

test("a Remux card names the bitrate control's value, and only that, for a FLAC track", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv"])), true);
  assert.equal(tracker.cards.get("01.mkv").meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 3 fonts");

  // With no bitrate to show, the row omits it rather than assume one.
  elements.get("remux-bitrate").value = "";
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv", "02.mkv"])), true);
  assert.equal(tracker.cards.get("02.mkv").meta.textContent, "FLAC 2.0 → Opus · Embedded subs · 3 fonts");

  // A bitrate change redraws every row's meta line in place.
  elements.get("remux-bitrate").value = "256k";
  tracker.refreshMeta();
  assert.equal(tracker.cards.get("01.mkv").meta.textContent, "FLAC 2.0 → Opus 256k · Embedded subs · 3 fonts");
});

test("a same-directory re-inspection remounts only the card whose manifest entry changed, in place", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const list = elements.get("remux-episode-list");
  const dir = "/anime/show";
  const ids = ["01.mkv", "02.mkv", "03.mkv"];
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(ids)), true);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  const changed = detailedManifest(ids);
  changed.items[1].metadata.fonts_count = 5;
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, changed), true);
  renderFrames();

  assert.equal(tracker.cards.get("01.mkv"), cardsBefore.get("01.mkv"));
  assert.equal(tracker.cards.get("03.mkv"), cardsBefore.get("03.mkv"));
  const fresh = tracker.cards.get("02.mkv");
  assert.notEqual(fresh, cardsBefore.get("02.mkv"));
  assert.equal(fresh.meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts");
  assert.deepEqual(list.children, [cardsBefore.get("01.mkv").el, fresh.el, cardsBefore.get("03.mkv").el]);
});

test("a same-directory re-inspection with a different ID set rebuilds every card", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv", "02.mkv"])), true);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv", "02.mkv", "03.mkv"])), true);
  renderFrames();

  assert.equal(tracker.cards.size, 3);
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
  assert.deepEqual(cardTitles(elements.get("remux-episode-list")), ["Show - 01.mkv", "Show - 02.mkv", "Show - 03.mkv"]);
});

test("a failed re-inspection that kept its cards drops them, and the next scan rebuilds", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";
  const ids = ["01.mkv", "02.mkv"];
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(ids)), true);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  tracker.rejectInspection(tracker.startScan(dir));
  assert.equal(tracker.cards.size, 0);
  assert.equal(elements.get("remux-queue-count").textContent, "Scan failed");
  assert.match(elements.get("remux-episode-list").innerHTML, /Scan failed/);

  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(ids)), true);
  renderFrames();
  assert.equal(elements.get("remux-episode-list").children.length, 2);
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
});

test("progress events on a processing row write only the ring and the status text", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  tracker.populateFromInspection(manifestOf(["ep1"]));
  const card = tracker.cards.get("ep1");
  tracker.handleEvent({ version: 1, type: "batch_start", total: 1 });
  tracker.handleEvent({ version: 1, type: "item_start", id: "ep1" });
  tracker.handleEvent({ version: 1, type: "item_progress", id: "ep1", progress: 10 });

  const writes = [];
  recordWrites(card.el.dataset, "row", ["status"], writes);
  recordWrites(card.status, "status", ["className", "title"], writes);
  for (const progress of [20, 30, 40]) {
    tracker.handleEvent({ version: 1, type: "item_progress", id: "ep1", progress });
  }

  assert.deepEqual(writes, []);
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(40));
  assert.match(card.statusTag.textContent, /^40% · \d+s$/);

  // A status change after the run of same-status updates still writes the status and tone.
  tracker.handleEvent({ version: 1, type: "item_complete", id: "ep1", status: "completed", duration_sec: 3 });
  assert.deepEqual(writes, ["row.status", "status.className", "status.title"]);
  assert.equal(card.el.dataset.status, "completed");
  assert.equal(card.status.className, "ep-status tone-ok");

  // Re-rendering the same terminal status, from a repeated item_complete (ignored by the
  // reducer) or from endBatch, writes nothing more, its status text included.
  recordWrites(card.statusTag, "tag", ["textContent"], writes);
  tracker.handleEvent({ version: 1, type: "item_complete", id: "ep1", status: "completed", duration_sec: 3 });
  tracker.endBatch(false);
  assert.deepEqual(writes, ["row.status", "status.className", "status.title"]);
});

test("filter chips hide the rows they do not match, and say so when none match", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  tracker.populateFromInspection(manifestOf(["ep1", "ep2", "ep3"]));
  tracker.handleEvent({ version: 1, type: "batch_start", items: [{ id: "ep1" }, { id: "ep2" }, { id: "ep3" }] });
  tracker.handleEvent({ version: 1, type: "item_complete", id: "ep1", status: "completed" });
  const hidden = () => ["ep1", "ep2", "ep3"].map(id => tracker.cards.get(id).el.hidden);

  tracker.setFilter("done");
  assert.deepEqual(hidden(), [false, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);

  // A status change while filtered re-applies the filter.
  tracker.handleEvent({ version: 1, type: "item_complete", id: "ep2", status: "completed" });
  assert.deepEqual(hidden(), [false, false, true]);

  tracker.setFilter("failed");
  assert.deepEqual(hidden(), [true, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, false);
  assert.ok(elements.get("remux-episode-list").classList.contains("is-filtered"));

  tracker.setFilter("all");
  assert.deepEqual(hidden(), [false, false, false]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);
});

test("a queue spanning release directories gets one header per run of rows, and reordering keeps them in place", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const list = elements.get("remux-episode-list");
  tracker.populateFromInspection({
    version: 2,
    items: [
      { id: "S1/01.mkv", name: "01.mkv", release: "Season 1" },
      { id: "S1/02.mkv", name: "02.mkv", release: "Season 1" },
      { id: "SP/01.mkv", name: "SP1.mkv", release: "Specials" },
    ],
  });
  const layout = () => list.children.map(el => (el.className === "ep-group-head"
    ? `[${el.children.map(part => part.textContent).join(" ")}]`
    : el.querySelector(".ep-title").textContent));
  assert.deepEqual(layout(), ["[Season 1 2]", "01.mkv", "02.mkv", "[Specials 1]", "SP1.mkv"]);

  // Moving a Specials row up past the boundary swaps rows, not a row and a header.
  const row = tracker.cards.get("SP/01.mkv").el;
  Object.defineProperty(row, "previousElementSibling", { get: () => list.children[list.children.indexOf(row) - 1] });
  tracker.reorderCard("SP/01.mkv", "up");
  assert.deepEqual(tracker.state.order, ["S1/01.mkv", "SP/01.mkv", "S1/02.mkv"]);
  assert.deepEqual(layout(), ["[Season 1 1]", "01.mkv", "[Specials 1]", "SP1.mkv", "[Season 1 1]", "02.mkv"]);

  // A single-release queue has no headers at all.
  tracker.populateFromInspection(manifestOf(["a", "b"]));
  assert.deepEqual(list.children.map(el => el.className), ["ep-row", "ep-row"]);
});

test("picking a row puts it in the inspector, which shows the first row until then", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const changes = [];
  tracker.onChange = () => changes.push(tracker.inspectedItem()?.id);
  tracker.populateFromInspection(manifestOf(["ep1", "ep2"]));
  assert.equal(tracker.inspectedItem().id, "ep1");
  assert.ok(tracker.cards.get("ep1").el.classList.contains("is-inspected"), "the default row is highlighted too");

  const pick = tracker.cards.get("ep2").pick;
  elements.get("remux-episode-list").handlers.get("click")({ target: { closest: sel => (sel === ".ep-pick" ? pick : null) } });
  assert.equal(tracker.inspectedId, "ep2");
  assert.equal(tracker.inspectedItem().id, "ep2");
  assert.ok(tracker.cards.get("ep2").el.classList.contains("is-inspected"));
  assert.equal(changes.at(-1), "ep2");

  tracker.inspect("ep1");
  assert.equal(tracker.cards.get("ep2").el.classList.contains("is-inspected"), false);
  assert.ok(tracker.cards.get("ep1").el.classList.contains("is-inspected"));
});

test("a row that fails mid-batch says why in its meta line, and the next Run's re-inspection restores it", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux");
  const dir = "/anime/show";
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv"])), true);
  const card = tracker.cards.get("01.mkv");
  const inspected = card.meta.textContent;

  tracker.handleEvent({ version: 1, type: "batch_start", total: 1, input_dir: dir, items: [{ id: "01.mkv", file: "01.mkv" }] });
  tracker.handleEvent({ version: 1, type: "item_start", id: "01.mkv" });
  tracker.handleEvent({ version: 1, type: "item_complete", id: "01.mkv", status: "error", error: "mkvmerge exited with 2" });
  assert.equal(card.meta.textContent, "Failed · mkvmerge exited with 2");
  assert.equal(card.meta.className, "ep-meta is-error");

  // Every Run re-inspects the folder first, which puts the item back to queued.
  tracker.endBatch(false);
  assert.equal(tracker.acceptInspection(tracker.startScan(dir), dir, detailedManifest(["01.mkv"])), true);
  const reused = tracker.cards.get("01.mkv");
  assert.equal(reused.meta.textContent, inspected);
  assert.equal(reused.meta.className, "ep-meta");
});
