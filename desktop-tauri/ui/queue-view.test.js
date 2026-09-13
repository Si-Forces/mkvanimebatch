import test from "node:test";
import assert from "node:assert/strict";
import { QueueTracker } from "./queue-view.js";

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

  appendChild(child) {
    this.children.push(child);
    return child;
  }

  addEventListener() {}
  setAttribute() {}
  getAttribute() { return null; }

  querySelector(selector) {
    const el = new FakeElement();
    if (selector.startsWith(".")) {
      el.className = selector.slice(1);
    }
    return el;
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
    createDocumentFragment: () => new FakeElement(),
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
