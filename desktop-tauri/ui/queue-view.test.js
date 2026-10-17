// Rendering only: the queue rules behind each published view (selection, order, reconcile,
// Directory Identity, the job overlay) are the Desktop Core's, tested in `queue.rs` and `actor.rs`.
import test from "node:test";
import assert from "node:assert/strict";
import { QueueTracker, RING_CIRCUMFERENCE } from "./queue-view.js";
import { jobView, modeView, row } from "./fake-core.js";

/** A dispatch for a tracker whose test sends no Intent. */
const noDispatch = async () => false;

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

  /** Like the DOM's, this moves a node that is already in the tree. */
  append(...nodes) {
    for (const node of nodes) {
      node.remove?.();
      this.appendChild(node);
    }
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
    // Bounded, so a render that keeps scheduling frames fails instead of hanging the suite.
    for (let i = 0; frames.size && i < 1000; i++) renderFrame();
    assert.equal(frames.size, 0, "the render settles within 1000 frames");
  };

  t.after(() => Object.assign(globalThis, previous));

  return { elements, renderFrame, renderFrames };
}

const DIR = "/anime/show";
const PATCHED_IDS = ["01.mkv", "02.mkv", "03.mkv"];

/** Rows with the track metadata a card renders. */
const detailedRow = (id, fields = {}) => row(id, {
  name: `Show - ${id}`,
  metadata: {
    resolution: "1920x1080",
    video_codec: "HEVC",
    video_tag: "1080p HEVC",
    audio_summary: "FLAC 2.0",
    has_flac: true,
    sub_summary: "ASS (ind)",
    fonts_count: 3,
  },
  ...fields,
});

/** A ModeView of `rows` as the accepted inspection `rev` of `DIR`. */
const accepted = (rows, rev = 1, fields = {}) => modeView(rows, { directory: DIR, manifest_rev: rev, ...fields });
const scanning = (previous, directory = DIR) => ({ ...previous, scan: { state: "scanning", directory } });
/** A participant row of a job, at `status`. */
const running = (id, fields = {}) => row(id, { participant: true, status: "processing", ...fields });

const cardTitles = list => list.children.map(el => el.querySelector(".ep-title").textContent);

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

/** A tracker that records the Intents it dispatches. */
function trackerWithIntents(mode = "remux") {
  const intents = [];
  const tracker = new QueueTracker(mode, async intent => intents.push(intent));
  return { tracker, intents };
}

test("rows the Job Snapshot lists but the manifest does not render in their outcome state, with a job meta line", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const job = jobView(["ep1", "ep2", "ep3", "ep4", "ep5"], { status: "completed", active: false });
  const jobOnly = (id, fields) => row(id, { name: `Show - ${id}.mkv`, release: "", metadata: null, in_manifest: false, participant: true, ...fields });

  tracker.applyView(modeView([
    jobOnly("ep1", { status: "completed", duration_sec: 10.2 }),
    jobOnly("ep2", { status: "skipped", reason: "Already up to date" }),
    jobOnly("ep3", { status: "cancelled" }),
    jobOnly("ep4", { status: "error", error: "FFmpeg encoding failed" }),
    jobOnly("ep5", { status: "skipped" }),
  ], { directory: DIR }), job);

  assert.equal(tracker.cards.size, 5);
  const card1 = tracker.cards.get("ep1");
  assert.equal(card1.el.dataset.status, "completed");
  assert.equal(card1.status.className, "ep-status tone-ok");
  assert.equal(card1.statusTag.textContent, "Done in 10.2s");
  assert.equal(card1.meta.textContent, "Processed · Done in 10.2s");

  const card2 = tracker.cards.get("ep2");
  assert.equal(card2.status.className, "ep-status tone-mute");
  assert.equal(card2.statusTag.textContent, "Skipped");
  assert.equal(card2.status.title, "Already up to date");
  assert.equal(card2.meta.textContent, "Skipped · Already up to date");

  // The core keeps only what the engine sent; the view supplies the default labels.
  assert.equal(tracker.cards.get("ep3").status.title, "Cancelled");
  assert.equal(tracker.cards.get("ep5").status.title, "Skipped");

  const card4 = tracker.cards.get("ep4");
  assert.equal(card4.el.dataset.status, "error");
  assert.equal(card4.status.className, "ep-status tone-err");
  assert.equal(card4.statusTag.textContent, "Failed");
  assert.equal(card4.status.title, "FFmpeg encoding failed");
  // None of them are manifest rows, so none is in the Queue Selection.
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), []);
});

test("a failed row with no error text shows the default one", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([running("ep1", { status: "error" })]), jobView(["ep1"]));
  const card = tracker.cards.get("ep1");
  assert.equal(card.status.title, "Failed");
  assert.equal(card.meta.textContent, "Failed · Failed");
});

test("a processing row spins, then shows its progress, then its outcome, which stops the heartbeat", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const job = jobView(["ep1"]);
  tracker.applyView(accepted([row("ep1", { name: "Show - 01.mkv" })]), job);
  const card = tracker.cards.get("ep1");
  assert.equal(tracker.heartbeatTimer, null);

  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", start_time_ms: Date.now() - 5000 })]), job);
  assert.equal(card.el.dataset.status, "processing");
  assert.equal(card.status.className, "ep-status tone-info is-spinning");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.equal(card.statusTag.textContent, "Processing · 5s");
  assert.notEqual(tracker.heartbeatTimer, null);

  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", start_time_ms: Date.now() - 12000, progress: 64.2 })]), job);
  assert.equal(card.status.className, "ep-status tone-info");
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(64.2));
  assert.equal(card.statusTag.textContent, "64% · 12s");
  tracker.state.items.get("ep1").startTime = Date.now() - 20000;
  tracker.tickHeartbeat();
  assert.equal(card.statusTag.textContent, "64% · 20s");

  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", status: "completed", duration_sec: 14.8 })]), job);
  assert.equal(tracker.cards.get("ep1"), card, "a status change redraws the row in place");
  assert.equal(card.el.dataset.status, "completed");
  assert.equal(card.status.className, "ep-status tone-ok");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.equal(card.statusTag.textContent, "Done in 14.8s");
  assert.equal(tracker.heartbeatTimer, null);
});

test("a scan, a failed scan and an empty queue stop the heartbeat", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const processing = accepted([running("ep1")]);
  for (const next of [scanning(processing, "/other"), { ...processing, scan: { state: "failed" } }, modeView()]) {
    tracker.applyView(processing, jobView(["ep1"]));
    assert.notEqual(tracker.heartbeatTimer, null);
    tracker.applyView(next, null);
    assert.equal(tracker.heartbeatTimer, null);
  }
});

test("rows mount over animation frames in published order, each from the latest view", t => {
  const { elements, renderFrame, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const oldPerformance = globalThis.performance;
  let clock = 0;
  globalThis.performance = { now: () => (clock += 10) }; // One card per animation frame.
  t.after(() => { globalThis.performance = oldPerformance; });

  const job = jobView(["02", "01"]);
  tracker.applyView(accepted([running("02", { progress: 70 }), row("01", { participant: true })]), job);
  assert.equal(list.children.length, 0);
  renderFrame();
  assert.deepEqual(cardTitles(list), ["02"]);
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(70));

  // A view applied mid-render reaches the rows still to mount, and the mounted ones.
  tracker.applyView(accepted([running("02", { progress: 76 }), row("01", { participant: true, status: "completed", duration_sec: 75.4 })]), job);
  renderFrames();
  assert.deepEqual(cardTitles(list), ["02", "01"]);
  assert.equal(tracker.cards.size, 2);
  assert.equal(tracker.cards.get("01").statusTag.textContent, "Done in 1m 15.4s");
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(76));
});

test("an instantaneous item shows <0.1s", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([running("ep1", { status: "completed", duration_sec: 0 })]), jobView(["ep1"]));
  assert.equal(tracker.cards.get("ep1").statusTag.textContent, "Done in <0.1s");
});

test("selection and reorder clicks dispatch Intents, and the published view redraws the rows", t => {
  const { elements } = setupMockEnvironment(t);
  const { tracker, intents } = trackerWithIntents();
  const list = elements.get("remux-episode-list");
  const countEl = elements.get("remux-queue-count");
  tracker.applyView(accepted(["ep1", "ep2", "ep3"]));
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep1", "ep2", "ep3"]);
  const cards = new Map(tracker.cards);

  const checkbox = { dataset: { itemId: "ep3" }, checked: false };
  list.handlers.get("change")({ target: { closest: () => checkbox } });
  const up = { dataset: { itemId: "ep2" }, classList: { contains: cls => cls === "ep-up-btn" } };
  list.handlers.get("click")({ target: { closest: sel => (sel === ".ep-reorder-btn" ? up : null) } });
  tracker.deselectAll();
  tracker.selectAll();
  assert.deepEqual(intents, [
    { type: "set_selected", mode: "remux", id: "ep3", selected: false },
    { type: "reorder", mode: "remux", id: "ep2", direction: "up" },
    { type: "set_all_selected", mode: "remux", selected: false },
    { type: "set_all_selected", mode: "remux", selected: true },
  ]);

  tracker.applyView(accepted(["ep2", "ep1", row("ep3", { selected: false })]));
  assert.equal(countEl.textContent, "2 / 3 Selected");
  assert.deepEqual(tracker.getSelectedOrderedItemIds(), ["ep2", "ep1"]);
  assert.equal(tracker.cards.get("ep3").checkbox.checked, false);
  // The rows are moved, not rebuilt.
  assert.deepEqual(list.children, ["ep2", "ep1", "ep3"].map(id => cards.get(id).el));
  assert.equal(tracker.cards.get("ep2").num.textContent, "01");
  assert.equal(tracker.cards.get("ep1").num.textContent, "02");
});

test("a locked queue dispatches nothing and puts a clicked checkbox back", t => {
  const { elements } = setupMockEnvironment(t);
  const { tracker, intents } = trackerWithIntents();
  const list = elements.get("remux-episode-list");
  tracker.applyView(accepted(["ep1", "ep2"]));
  tracker.selectionLocked = true;

  const checkbox = { dataset: { itemId: "ep1" }, checked: false };
  list.handlers.get("change")({ target: { closest: () => checkbox } });
  tracker.reorderCard("ep2", "up");
  tracker.deselectAll();

  assert.deepEqual(intents, []);
  assert.equal(checkbox.checked, true);
});

test("a tick the core refuses or cannot apply goes back to the published selection", async t => {
  const { elements } = setupMockEnvironment(t);
  const list = elements.get("remux-episode-list");
  t.mock.method(console, "error", () => {});
  // A job the core runs refuses the Intent and publishes nothing, so no view corrects the tick.
  for (const dispatch of [async () => false, async () => { throw "A batch is running"; }]) {
    const tracker = new QueueTracker("remux", dispatch);
    tracker.applyView(accepted(["ep1", row("ep2", { selected: false })]));
    for (const [id, published] of [["ep1", true], ["ep2", false]]) {
      const checkbox = { dataset: { itemId: id }, checked: !published };
      await list.handlers.get("change")({ target: { closest: () => checkbox } });
      assert.equal(checkbox.checked, published, id);
    }
  }
  // An applied tick stays.
  const tracker = new QueueTracker("remux", async () => true);
  tracker.applyView(accepted(["ep1"]));
  const checkbox = { dataset: { itemId: "ep1" }, checked: false };
  await list.handlers.get("change")({ target: { closest: () => checkbox } });
  assert.equal(checkbox.checked, false);
});

test("a re-inspection of the directory shown keeps every card and never shows the scanning placeholder", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const ids = ["01.mkv", "02.mkv", "03.mkv"];
  const job = jobView(ids, { status: "completed", active: false });
  // A finished batch left a terminal row; a deselected row shows its checkbox off.
  const before = accepted([detailedRow("01.mkv", { participant: true, status: "completed", duration_sec: 4.2 }), detailedRow("02.mkv", { selected: false }), detailedRow("03.mkv")]);
  tracker.applyView(before, job);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);
  const elementsBefore = [...list.children];
  const listWrites = [];
  recordWrites(list, "list", ["innerHTML"], listWrites);

  // Every Run forces a re-inspection of the shown directory; a deep-equal manifest arrives, and
  // the accepted scan dropped the finished job's snapshot.
  tracker.applyView(scanning(before), job);
  assert.equal(elements.get("remux-queue-count").textContent, "Scanning…");
  elementsBefore.forEach((el, i) => assert.equal(list.children[i], el));
  tracker.applyView(accepted([detailedRow("01.mkv"), detailedRow("02.mkv", { selected: false }), detailedRow("03.mkv")], 2), null);
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

test("a scan keeps the cards of the same Directory Identity and clears them for any other", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const list = elements.get("remux-episode-list");
  const view = accepted([detailedRow("01.mkv"), detailedRow("02.mkv")]);
  const showing = () => {
    const tracker = new QueueTracker("remux", noDispatch);
    tracker.applyView(view);
    renderFrames();
    return tracker;
  };

  const same = showing();
  const cardsBefore = [...list.children];
  same.applyView(scanning(view));
  assert.equal(same.cards.size, 2);
  cardsBefore.forEach((el, i) => assert.equal(list.children[i], el));

  // Identities compare byte for byte: another spelling of the folder is another directory.
  for (const other of ["/anime/other", "/anime/show/"]) {
    const tracker = showing();
    tracker.applyView(scanning(view, other));
    assert.equal(tracker.cards.size, 0);
    assert.equal(list.children.length, 0);
    assert.match(list.innerHTML, /Scanning folder/);
  }
});

test("an empty queue says whether a folder is open", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const title = () => list.querySelector(".empty-state-title").textContent;
  tracker.applyView(modeView());
  assert.equal(title(), "No episodes yet");
  tracker.applyView(modeView([], { directory: DIR, manifest_rev: 1 }));
  assert.equal(title(), "No MKV files found");
  // The text is set as text, never written into the markup.
  assert.doesNotMatch(list.innerHTML, /No MKV files found/);
  assert.equal(elements.get("remux-queue-count").textContent, "Ready");
});

test("a freshly mounted card is drawn in the queued look", t => {
  const { renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([detailedRow("01.mkv"), detailedRow("02.mkv")]));
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
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([detailedRow("01.mkv")]));
  assert.equal(tracker.cards.get("01.mkv").meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 3 fonts");

  // With no bitrate to show, the row omits it rather than assume one.
  elements.get("remux-bitrate").value = "";
  tracker.applyView(accepted([detailedRow("01.mkv"), detailedRow("02.mkv")], 2));
  assert.equal(tracker.cards.get("02.mkv").meta.textContent, "FLAC 2.0 → Opus · Embedded subs · 3 fonts");

  // A bitrate change redraws every row's meta line in place.
  elements.get("remux-bitrate").value = "256k";
  tracker.refreshMeta();
  assert.equal(tracker.cards.get("01.mkv").meta.textContent, "FLAC 2.0 → Opus 256k · Embedded subs · 3 fonts");
});

test("a published Track Assignment redraws the meta line of its row", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([detailedRow("01.mkv")]));
  assert.match(tracker.cards.get("01.mkv").meta.textContent, /Embedded subs/);
  tracker.applyView(accepted([detailedRow("01.mkv")], 1, {
    assignments: { "01.mkv": { target_video: "01.mkv", asset_path: "/subs/01.ass", track_type: "subtitle" } },
  }));
  assert.match(tracker.cards.get("01.mkv").meta.textContent, /Manual subs/);
  assert.equal(tracker.assignments.get("01.mkv").asset_path, "/subs/01.ass");
});

test("a patch's changed-rows indices redraw only those cards, and leave the others untouched", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id))));
  const check = id => tracker.cards.get(id).checkbox;
  // Marks a redraw of these cards would overwrite.
  check("01.mkv").checked = "untouched";
  check("03.mkv").checked = "untouched";

  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id, { selected: id !== "02.mkv" }))), null, [1]);
  assert.equal(check("02.mkv").checked, false);
  assert.equal(check("01.mkv").checked, "untouched");
  assert.equal(check("03.mkv").checked, "untouched");
});

test("a patch that changes the assignments and one row refreshes every card's meta line", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id))));
  for (const id of PATCHED_IDS) tracker.cards.get(id).meta.textContent = "stale";

  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id, { selected: id !== "02.mkv" })), 1, {
    assignments: { "01.mkv": { target_video: "01.mkv", asset_path: "/subs/01.ass", track_type: "subtitle" } },
  }), null, [1]);
  assert.match(tracker.cards.get("01.mkv").meta.textContent, /Manual subs/);
  assert.match(tracker.cards.get("03.mkv").meta.textContent, /Embedded subs/);
});

test("a re-inspection of the directory shown remounts only the card whose manifest entry changed, in place", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  tracker.applyView(accepted(["01.mkv", "02.mkv", "03.mkv"].map(id => detailedRow(id))));
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  const changed = ["01.mkv", "02.mkv", "03.mkv"].map(id => detailedRow(id));
  changed[1].metadata.fonts_count = 5;
  tracker.applyView(accepted(changed, 2));
  renderFrames();

  assert.equal(tracker.cards.get("01.mkv"), cardsBefore.get("01.mkv"));
  assert.equal(tracker.cards.get("03.mkv"), cardsBefore.get("03.mkv"));
  const fresh = tracker.cards.get("02.mkv");
  assert.notEqual(fresh, cardsBefore.get("02.mkv"));
  assert.equal(fresh.meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts");
  assert.deepEqual(list.children, [cardsBefore.get("01.mkv").el, fresh.el, cardsBefore.get("03.mkv").el]);
});

test("a new manifest accepted mid-render restarts the render, so no mounted card keeps the old one", t => {
  const { elements, renderFrame, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const oldPerformance = globalThis.performance;
  let clock = 0;
  globalThis.performance = { now: () => (clock += 10) }; // One card per animation frame.
  t.after(() => { globalThis.performance = oldPerformance; });

  tracker.applyView(accepted(["01.mkv", "02.mkv"].map(id => detailedRow(id))));
  renderFrame();
  assert.equal(list.children.length, 1);

  const changed = ["01.mkv", "02.mkv"].map(id => detailedRow(id));
  changed[0].metadata.fonts_count = 5;
  tracker.applyView(accepted(changed, 2));
  renderFrames();
  assert.equal(tracker.cards.size, 2);
  assert.equal(tracker.cards.get("01.mkv").meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts");
});

test("a re-inspection with a different ID set, or of another directory, rebuilds every card", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const rows = ids => ids.map(id => detailedRow(id));
  tracker.applyView(accepted(rows(["01.mkv", "02.mkv"])));
  renderFrames();
  let cardsBefore = new Map(tracker.cards);

  tracker.applyView(accepted(rows(["01.mkv", "02.mkv", "03.mkv"]), 2));
  renderFrames();
  assert.equal(tracker.cards.size, 3);
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
  assert.deepEqual(cardTitles(elements.get("remux-episode-list")), ["Show - 01.mkv", "Show - 02.mkv", "Show - 03.mkv"]);

  cardsBefore = new Map(tracker.cards);
  tracker.applyView(modeView(rows(["01.mkv", "02.mkv", "03.mkv"]), { directory: "/anime/other", manifest_rev: 3 }));
  renderFrames();
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
});

test("a failed re-inspection that kept its cards drops them, and the next scan rebuilds", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const view = accepted([detailedRow("01.mkv"), detailedRow("02.mkv")]);
  tracker.applyView(view);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  tracker.applyView(scanning(view));
  // The core keeps the previous rows after a failed scan; the list says the scan failed.
  tracker.applyView({ ...view, scan: { state: "failed" } });
  assert.equal(tracker.cards.size, 0);
  assert.equal(elements.get("remux-queue-count").textContent, "Scan failed");
  const list = elements.get("remux-episode-list");
  assert.equal(list.querySelector(".empty-state-title").textContent, "Scan failed");
  assert.equal(list.querySelector(".empty-state-action").textContent, "Choose another folder…");

  tracker.applyView(accepted([detailedRow("01.mkv"), detailedRow("02.mkv")], 2));
  renderFrames();
  assert.equal(elements.get("remux-episode-list").children.length, 2);
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
});

test("progress on a processing row writes only the ring and the status text", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const job = jobView(["ep1"]);
  const at = fields => accepted([running("ep1", { start_time_ms: Date.now(), ...fields })]);
  tracker.applyView(at({ progress: 10 }), job);
  const card = tracker.cards.get("ep1");

  const writes = [];
  recordWrites(card.el.dataset, "row", ["status"], writes);
  recordWrites(card.status, "status", ["className", "title"], writes);
  for (const progress of [20, 30, 40]) tracker.applyView(at({ progress }), job);

  assert.deepEqual(writes, []);
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(40));
  assert.match(card.statusTag.textContent, /^40% · \d+s$/);

  // A status change after the run of same-status updates still writes the status and tone.
  const done = at({ status: "completed", duration_sec: 3 });
  tracker.applyView(done, job);
  assert.deepEqual(writes, ["row.status", "status.className", "status.title"]);
  assert.equal(card.status.className, "ep-status tone-ok");

  // Re-publishing the same terminal status writes nothing more, its status text included.
  recordWrites(card.statusTag, "tag", ["textContent"], writes);
  tracker.applyView(done, { ...job, status: "completed", active: false });
  assert.deepEqual(writes, ["row.status", "status.className", "status.title"]);
});

test("the count follows the job's own batch, and a job that ended before BatchStart leaves the selection count", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const countEl = elements.get("remux-queue-count");
  const rows = fields => [row("ep1", { participant: true, ...fields }), row("ep2", { selected: false }), row("ep3", { selected: false })];

  tracker.applyView(accepted(rows({})), jobView(["ep1"]));
  assert.equal(countEl.textContent, "0 of 1 · 0 done");
  tracker.applyView(accepted(rows({ status: "completed", duration_sec: 14.8 })), jobView(["ep1"], { status: "completed", active: false }));
  assert.equal(countEl.textContent, "1 of 1 · 1 done");

  tracker.applyView(accepted(["ep1", row("ep2", { selected: false })]), jobView([], { status: "error", active: false }));
  assert.equal(countEl.textContent, "1 / 2 Selected");
  assert.deepEqual([...tracker.state.items.values()].map(item => item.status), ["queued", "queued"]);

  // Another mode's job is not this queue's batch.
  tracker.applyView(accepted(["ep1", "ep2"]), jobView(["ep1"], { mode: "extract" }));
  assert.equal(countEl.textContent, "2 / 2 Selected");
  assert.equal(tracker.state.batchItemIds, null);
});

test("filter chips hide the rows they do not match, and say so when none match", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const job = jobView(["ep1", "ep2", "ep3"]);
  const done = id => row(id, { participant: true, status: "completed" });
  tracker.applyView(accepted([done("ep1"), row("ep2", { participant: true }), row("ep3", { participant: true })]), job);
  const hidden = () => ["ep1", "ep2", "ep3"].map(id => tracker.cards.get(id).el.hidden);

  tracker.setFilter("done");
  assert.deepEqual(hidden(), [false, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);

  // A status change while filtered re-applies the filter.
  tracker.applyView(accepted([done("ep1"), done("ep2"), row("ep3", { participant: true })]), job);
  assert.deepEqual(hidden(), [false, false, true]);

  tracker.setFilter("failed");
  assert.deepEqual(hidden(), [true, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, false);
  assert.ok(elements.get("remux-episode-list").classList.contains("is-filtered"));

  tracker.setFilter("all");
  assert.deepEqual(hidden(), [false, false, false]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);
});

test("a queue spanning release directories gets one header per run of rows, and a published reorder keeps them in place", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const s1 = row("S1/01.mkv", { name: "01.mkv", release: "Season 1" });
  const s2 = row("S1/02.mkv", { name: "02.mkv", release: "Season 1" });
  const sp = row("SP/01.mkv", { name: "SP1.mkv", release: "Specials" });
  tracker.applyView(accepted([s1, s2, sp]));
  const layout = () => list.children.map(el => (el.className === "ep-group-head"
    ? `[${el.children.map(part => part.textContent).join(" ")}]`
    : el.querySelector(".ep-title").textContent));
  assert.deepEqual(layout(), ["[Season 1 2]", "01.mkv", "02.mkv", "[Specials 1]", "SP1.mkv"]);

  tracker.applyView(accepted([s1, sp, s2]));
  assert.deepEqual(layout(), ["[Season 1 1]", "01.mkv", "[Specials 1]", "SP1.mkv", "[Season 1 1]", "02.mkv"]);

  // A single-release queue has no headers at all.
  tracker.applyView(accepted(["a", "b"], 2));
  assert.deepEqual(list.children.map(el => el.className), ["ep-row", "ep-row"]);
});

test("picking a row puts it in the inspector, which shows the first row until then", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const changes = [];
  tracker.onChange = () => changes.push(tracker.inspectedItem()?.id);
  tracker.applyView(accepted(["ep1", "ep2"]));
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
  const tracker = new QueueTracker("remux", noDispatch);
  const job = jobView(["01.mkv"]);
  tracker.applyView(accepted([detailedRow("01.mkv")]), job);
  const card = tracker.cards.get("01.mkv");
  const inspected = card.meta.textContent;

  tracker.applyView(accepted([detailedRow("01.mkv", { participant: true, status: "error", error: "mkvmerge exited with 2" })]), job);
  assert.equal(card.meta.textContent, "Failed · mkvmerge exited with 2");
  assert.equal(card.meta.className, "ep-meta is-error");

  // Every Run re-inspects the folder first, which puts the item back to queued.
  tracker.applyView(accepted([detailedRow("01.mkv")], 2), null);
  const reused = tracker.cards.get("01.mkv");
  assert.equal(reused.meta.textContent, inspected);
  assert.equal(reused.meta.className, "ep-meta");
});
