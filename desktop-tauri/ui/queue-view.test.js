// Rendering only: the queue rules behind each published view (selection, order, reconcile,
// Directory Identity, the job overlay) and every text a row shows (its label, tone, hover text,
// meta line and bucket) are the Desktop Core's, tested in `queue.rs`, `presentation.rs`,
// `inspector.rs` and `actor/tests.rs`.
import test from "node:test";
import assert from "node:assert/strict";
import { QueueTracker, RING_CIRCUMFERENCE } from "./queue-view.js";
import { modeView, row } from "./fake-core.js";

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

/** The meta line the core publishes for an inspected FLAC row at 192k with embedded subs. */
const META = "FLAC 2.0 → Opus 192k · Embedded subs · 3 fonts";

/** A row with the meta line the core gives an inspected one. */
const detailedRow = (id, fields = {}) => row(id, { name: `Show - ${id}`, meta: META, ...fields });

/** A ModeView of `rows` as the accepted inspection `rev` of `DIR`. */
const accepted = (rows, rev = 1, fields = {}) => modeView(rows, { directory: DIR, manifest_rev: rev, ...fields });
const scanning = (previous, directory = DIR) => ({ ...previous, scan: { state: "scanning", directory }, count_text: "Scanning…" });
/** A participant row of a job, processing with the label the core gives it. */
const running = (id, fields = {}) => row(id, { participant: true, status: "processing", tone: "info", label: "Processing · 0s", ...fields });
/** A participant row that completed, as the core labels it. */
const completed = (id, fields = {}) => row(id, { participant: true, status: "completed", tone: "ok", label: "Done", bucket: "done", ...fields });

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

test("a row draws its published label, tone, hover text and meta line", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(modeView([
    completed("ep1", { in_manifest: false, label: "Done in 10.2s", meta: "Processed · Done in 10.2s" }),
    row("ep2", { participant: true, status: "skipped", label: "Skipped", title: "Already up to date", meta: "Skipped · Already up to date", bucket: "done" }),
    row("ep3", { participant: true, status: "error", tone: "err", label: "Failed", title: "FFmpeg encoding failed", meta: "Failed · FFmpeg encoding failed", meta_error: true, bucket: "failed" }),
  ], { directory: DIR }));

  assert.equal(tracker.cards.size, 3);
  const card1 = tracker.cards.get("ep1");
  assert.equal(card1.el.dataset.status, "completed");
  assert.equal(card1.status.className, "ep-status tone-ok");
  assert.equal(card1.statusTag.textContent, "Done in 10.2s");
  assert.equal(card1.meta.textContent, "Processed · Done in 10.2s");
  assert.equal(card1.meta.className, "ep-meta");

  const card2 = tracker.cards.get("ep2");
  assert.equal(card2.status.className, "ep-status tone-mute");
  assert.equal(card2.status.title, "Already up to date");

  const card3 = tracker.cards.get("ep3");
  assert.equal(card3.el.dataset.status, "error");
  assert.equal(card3.status.className, "ep-status tone-err");
  assert.equal(card3.status.title, "FFmpeg encoding failed");
  assert.equal(card3.meta.className, "ep-meta is-error");
});

test("a processing row spins, then shows its progress, then its outcome, each with the published label", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([row("ep1", { name: "Show - 01.mkv" })]));
  const card = tracker.cards.get("ep1");

  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", label: "Processing · 5s" })]));
  assert.equal(card.el.dataset.status, "processing");
  assert.equal(card.status.className, "ep-status tone-info is-spinning");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.equal(card.statusTag.textContent, "Processing · 5s");

  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", progress: 64.2, label: "64% · 12s" })]));
  assert.equal(card.status.className, "ep-status tone-info");
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(64.2));
  assert.equal(card.statusTag.textContent, "64% · 12s");
  // A Presentation Heartbeat publication moves only the label.
  tracker.applyView(accepted([running("ep1", { name: "Show - 01.mkv", progress: 64.2, label: "64% · 13s" })]), [0]);
  assert.equal(card.statusTag.textContent, "64% · 13s");

  tracker.applyView(accepted([completed("ep1", { name: "Show - 01.mkv", label: "Done in 14.8s" })]));
  assert.equal(tracker.cards.get("ep1"), card, "a status change redraws the row in place");
  assert.equal(card.el.dataset.status, "completed");
  assert.equal(card.status.className, "ep-status tone-ok");
  assert.equal(card.arc.style.strokeDashoffset, "");
  assert.equal(card.statusTag.textContent, "Done in 14.8s");
});

test("rows mount over animation frames in published order, each from the latest view", t => {
  const { elements, renderFrame, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const oldPerformance = globalThis.performance;
  let clock = 0;
  globalThis.performance = { now: () => (clock += 10) }; // One card per animation frame.
  t.after(() => { globalThis.performance = oldPerformance; });

  tracker.applyView(accepted([running("02", { progress: 70 }), row("01", { participant: true })]));
  assert.equal(list.children.length, 0);
  renderFrame();
  assert.deepEqual(cardTitles(list), ["02"]);
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(70));

  // A view applied mid-render reaches the rows still to mount, and the mounted ones.
  tracker.applyView(accepted([running("02", { progress: 76 }), completed("01", { label: "Done in 1m 15.4s" })]));
  renderFrames();
  assert.deepEqual(cardTitles(list), ["02", "01"]);
  assert.equal(tracker.cards.size, 2);
  assert.equal(tracker.cards.get("01").statusTag.textContent, "Done in 1m 15.4s");
  assert.equal(tracker.cards.get("02").arc.style.strokeDashoffset, ringOffset(76));
});

test("selection and reorder clicks dispatch Intents, and the published view redraws the rows", t => {
  const { elements } = setupMockEnvironment(t);
  const { tracker, intents } = trackerWithIntents();
  const list = elements.get("remux-episode-list");
  const countEl = elements.get("remux-queue-count");
  tracker.applyView(accepted(["ep1", "ep2", "ep3"], 1, { count_text: "3 / 3 Selected" }));
  assert.equal(countEl.textContent, "3 / 3 Selected");
  assert.equal(countEl.title, "3 / 3 Selected");
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

  tracker.applyView(accepted(["ep2", "ep1", row("ep3", { selected: false, bucket: null })], 1, { count_text: "2 / 3 Selected" }));
  assert.equal(countEl.textContent, "2 / 3 Selected");
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
  // A finished batch left a terminal row; a deselected row shows its checkbox off.
  const before = accepted([
    detailedRow("01.mkv", { participant: true, status: "completed", tone: "ok", label: "Done in 4.2s", bucket: "done" }),
    detailedRow("02.mkv", { selected: false, bucket: null }),
    detailedRow("03.mkv"),
  ]);
  tracker.applyView(before);
  renderFrames();
  const cardsBefore = new Map(tracker.cards);
  const elementsBefore = [...list.children];
  const listWrites = [];
  recordWrites(list, "list", ["innerHTML"], listWrites);

  // Every Run forces a re-inspection of the shown directory; a deep-equal manifest arrives, and
  // the accepted scan dropped the finished job's snapshot.
  tracker.applyView(scanning(before));
  assert.equal(elements.get("remux-queue-count").textContent, "Scanning…");
  elementsBefore.forEach((el, i) => assert.equal(list.children[i], el));
  tracker.applyView(accepted([detailedRow("01.mkv"), detailedRow("02.mkv", { selected: false, bucket: null }), detailedRow("03.mkv")], 2));
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
  assert.equal(card.el.dataset.status, "queued");
  assert.equal(card.status.className, "ep-status tone-mute");
  assert.equal(card.num.textContent, "02");
  assert.equal(card.statusTag.textContent, "Queued");
  assert.equal(card.meta.textContent, META);
});

test("a published meta line redraws its card in place, a failure's in the error style and back", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted([detailedRow("01.mkv")]));
  const card = tracker.cards.get("01.mkv");

  // A bitrate edit, or a Track Assignment, changes the published meta line.
  tracker.applyView(accepted([detailedRow("01.mkv", { meta: "FLAC 2.0 → Opus 256k · Manual subs · 3 fonts" })]), [0]);
  assert.equal(card.meta.textContent, "FLAC 2.0 → Opus 256k · Manual subs · 3 fonts");

  tracker.applyView(accepted([detailedRow("01.mkv", { participant: true, status: "error", tone: "err", label: "Failed", meta: "Failed · mkvmerge exited with 2", meta_error: true, bucket: "failed" })]), [0]);
  assert.equal(card.meta.textContent, "Failed · mkvmerge exited with 2");
  assert.equal(card.meta.className, "ep-meta is-error");

  // Every Run re-inspects the folder first, which puts the item back to queued.
  tracker.applyView(accepted([detailedRow("01.mkv")], 2));
  assert.equal(tracker.cards.get("01.mkv"), card);
  assert.equal(card.meta.textContent, META);
  assert.equal(card.meta.className, "ep-meta");
});

test("a patch's changed-rows indices redraw only those cards, and leave the others untouched", t => {
  setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id))));
  const check = id => tracker.cards.get(id).checkbox;
  // Marks a redraw of these cards would overwrite.
  check("01.mkv").checked = "untouched";
  check("03.mkv").checked = "untouched";

  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id, { selected: id !== "02.mkv" }))), [1]);
  assert.equal(check("02.mkv").checked, false);
  assert.equal(check("01.mkv").checked, "untouched");
  assert.equal(check("03.mkv").checked, "untouched");
});

test("a re-inspection of the directory shown redraws a changed row in place and keeps every card", t => {
  const { elements, renderFrames } = setupMockEnvironment(t, { deferFrames: true });
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  tracker.applyView(accepted(PATCHED_IDS.map(id => detailedRow(id))));
  renderFrames();
  const cardsBefore = new Map(tracker.cards);

  const changed = PATCHED_IDS.map(id => detailedRow(id));
  changed[1] = detailedRow("02.mkv", { name: "Show - 02 v2.mkv", meta: "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts" });
  tracker.applyView(accepted(changed, 2));
  renderFrames();

  for (const id of PATCHED_IDS) assert.equal(tracker.cards.get(id), cardsBefore.get(id));
  const card = tracker.cards.get("02.mkv");
  assert.equal(card.title.textContent, "Show - 02 v2.mkv");
  assert.equal(card.meta.textContent, "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts");
  assert.deepEqual(list.children, PATCHED_IDS.map(id => cardsBefore.get(id).el));
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
  changed[0].meta = "FLAC 2.0 → Opus 192k · Embedded subs · 5 fonts";
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

  tracker.applyView(accepted(rows(PATCHED_IDS), 2));
  renderFrames();
  assert.equal(tracker.cards.size, 3);
  for (const [id, card] of cardsBefore) assert.notEqual(tracker.cards.get(id), card);
  assert.deepEqual(cardTitles(elements.get("remux-episode-list")), ["Show - 01.mkv", "Show - 02.mkv", "Show - 03.mkv"]);

  cardsBefore = new Map(tracker.cards);
  tracker.applyView(modeView(rows(PATCHED_IDS), { directory: "/anime/other", manifest_rev: 3 }));
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
  tracker.applyView({ ...view, scan: { state: "failed" }, count_text: "Scan failed" });
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
  const at = fields => accepted([running("ep1", fields)]);
  tracker.applyView(at({ progress: 10, label: "10% · 1s" }));
  const card = tracker.cards.get("ep1");

  const writes = [];
  recordWrites(card.el.dataset, "row", ["status"], writes);
  recordWrites(card.status, "status", ["className", "title"], writes);
  recordWrites(card.meta, "meta", ["className", "textContent"], writes);
  for (const progress of [20, 30, 40]) tracker.applyView(at({ progress, label: `${progress}% · 2s` }));

  assert.deepEqual(writes, []);
  assert.equal(card.arc.style.strokeDashoffset, ringOffset(40));
  assert.equal(card.statusTag.textContent, "40% · 2s");

  // A status change after the run of same-status updates writes the status and tone.
  const done = accepted([completed("ep1", { label: "Done in 3.0s" })]);
  tracker.applyView(done);
  assert.deepEqual(writes, ["row.status", "status.className"]);
  assert.equal(card.status.className, "ep-status tone-ok");

  // Re-publishing the same terminal status writes nothing more, its status text included.
  recordWrites(card.statusTag, "tag", ["textContent"], writes);
  tracker.applyView(done);
  assert.deepEqual(writes, ["row.status", "status.className"]);
});

test("filter chips hide the rows whose published bucket they do not match, and say so when none match", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const todo = id => row(id, { participant: true });
  tracker.applyView(accepted([completed("ep1"), todo("ep2"), row("ep3", { selected: false, bucket: null })]));
  const hidden = () => ["ep1", "ep2", "ep3"].map(id => tracker.cards.get(id).el.hidden);

  tracker.setFilter("done");
  assert.deepEqual(hidden(), [false, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);

  // A bucket change while filtered re-applies the filter.
  tracker.applyView(accepted([completed("ep1"), completed("ep2"), row("ep3", { selected: false, bucket: null })]));
  assert.deepEqual(hidden(), [false, false, true]);

  tracker.setFilter("todo");
  assert.deepEqual(hidden(), [true, true, true], "an unselected row shows only under All");

  tracker.setFilter("failed");
  assert.deepEqual(hidden(), [true, true, true]);
  assert.equal(elements.get("remux-filter-empty").hidden, false);
  assert.ok(elements.get("remux-episode-list").classList.contains("is-filtered"));

  tracker.setFilter("all");
  assert.deepEqual(hidden(), [false, false, false]);
  assert.equal(elements.get("remux-filter-empty").hidden, true);
});

test("a queue spanning release directories gets one published header per run of rows, and a published reorder keeps them in place", t => {
  const { elements } = setupMockEnvironment(t);
  const tracker = new QueueTracker("remux", noDispatch);
  const list = elements.get("remux-episode-list");
  const s1 = row("S1/01.mkv", { name: "01.mkv", release: "Season 1" });
  const s2 = row("S1/02.mkv", { name: "02.mkv", release: "Season 1" });
  const sp = row("SP/01.mkv", { name: "SP1.mkv", release: "Specials" });
  const root = row("00.mkv", { release: "." });
  const library = [
    { release: null, label: "show", tooltip: "anime › show", count: 4, group_label: "anime › show" },
    { release: "Season 1", label: "Season 1", tooltip: "Season 1", count: 2, group_label: "Season 1 (published)" },
    { release: "Specials", label: "Specials", tooltip: "Specials", count: 1, group_label: "Specials" },
  ];
  tracker.applyView(accepted([root, s1, s2, sp], 1, { library }));
  const layout = () => list.children.map(el => (el.className === "ep-group-head"
    ? `[${el.children.map(part => part.textContent).join(" ")}]`
    : el.querySelector(".ep-title").textContent));
  assert.deepEqual(layout(), ["[anime › show 1]", "00.mkv", "[Season 1 (published) 2]", "01.mkv", "02.mkv", "[Specials 1]", "SP1.mkv"]);

  tracker.applyView(accepted([root, s1, sp, s2], 1, { library }));
  assert.deepEqual(layout(), ["[anime › show 1]", "00.mkv", "[Season 1 (published) 1]", "01.mkv", "[Specials 1]", "SP1.mkv", "[Season 1 (published) 1]", "02.mkv"]);

  // A single-release queue has no headers at all.
  tracker.applyView(accepted(["a", "b"], 2));
  assert.deepEqual(list.children.map(el => el.className), ["ep-row", "ep-row"]);
});

test("picking a row sends inspect and highlights it at once, and the highlight then follows the published inspector", async t => {
  const { elements } = setupMockEnvironment(t);
  let answer;
  const intents = [];
  const tracker = new QueueTracker("remux", intent => {
    intents.push(intent);
    return new Promise(resolve => { answer = resolve; });
  });
  const inspected = id => tracker.cards.get(id).el.classList.contains("is-inspected");
  const view = pick => accepted(["ep1", "ep2"], 1, { inspector: { id: pick } });
  tracker.applyView(view("ep1"));
  assert.ok(inspected("ep1"), "the published row is highlighted, the first one before any pick");

  const pick = tracker.cards.get("ep2").pick;
  elements.get("remux-episode-list").handlers.get("click")({ target: { closest: sel => (sel === ".ep-pick" ? pick : null) } });
  assert.deepEqual(intents, [{ type: "inspect", mode: "remux", id: "ep2" }]);
  assert.ok(inspected("ep2"), "highlighted before the core answers");
  assert.equal(inspected("ep1"), false);

  // The core published the pick before it answered.
  tracker.applyView(view("ep2"));
  answer(true);
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(inspected("ep2"));

  // A pick the core does not take falls back to the published one once answered.
  tracker.inspect("ep1");
  assert.ok(inspected("ep1"));
  answer(false);
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(inspected("ep2"));
  assert.equal(inspected("ep1"), false);
});
