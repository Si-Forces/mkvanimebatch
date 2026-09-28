import test from "node:test";
import assert from "node:assert/strict";
import { createTelemetryLoop, IDLE_POLL_MS, ACTIVE_POLL_MS } from "./telemetry.js";

function flush() {
  return new Promise((resolve) => setImmediate(() => setImmediate(resolve)));
}

class FakeTimers {
  constructor() {
    this.now = 0;
    this.tasks = [];
    this.nextId = 1;
  }

  setIntervalFn(fn, ms) {
    const id = this.nextId++;
    this.tasks.push({ id, fn, ms, next: this.now + ms });
    return id;
  }

  clearIntervalFn(id) {
    this.tasks = this.tasks.filter((t) => t.id !== id);
  }

  activeInterval() {
    return this.tasks.length > 0 ? this.tasks[0].ms : null;
  }

  async advance(ms) {
    const target = this.now + ms;
    for (;;) {
      const due = this.tasks
        .filter((t) => t.next <= target)
        .sort((a, b) => a.next - b.next)[0];
      if (!due) break;
      this.now = due.next;
      due.fn();
      if (this.tasks.includes(due)) due.next = this.now + due.ms;
      await flush();
    }
    this.now = target;
  }
}

function makeDoc({ initialHidden = false } = {}) {
  return {
    hidden: initialHidden,
    listeners: new Map(),
    addEventListener(type, fn) {
      this.listeners.set(type, fn);
    },
    dispatchVisibility() {
      this.listeners.get("visibilitychange")?.();
    },
  };
}

function makeLoop(timers, options) {
  return createTelemetryLoop({
    onData: () => {},
    onError: () => {},
    setIntervalFn: timers.setIntervalFn.bind(timers),
    clearIntervalFn: timers.clearIntervalFn.bind(timers),
    ...options,
  });
}

test("starts polling at the idle cadence", async () => {
  const timers = new FakeTimers();
  let calls = 0;
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      return { cpu_pct: 5 };
    },
  });

  loop.start();
  await flush();
  assert.equal(calls, 1);
  assert.equal(timers.activeInterval(), IDLE_POLL_MS);

  await timers.advance(IDLE_POLL_MS - 1);
  assert.equal(calls, 1);
  await timers.advance(1);
  assert.equal(calls, 2);
});

test("switches to the fast cadence while a job is active", async () => {
  const timers = new FakeTimers();
  let calls = 0;
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      return {};
    },
  });

  loop.start();
  await flush();
  assert.equal(calls, 1);

  loop.setJobActive(true);
  await flush();
  assert.equal(calls, 2);
  assert.equal(timers.activeInterval(), ACTIVE_POLL_MS);

  await timers.advance(ACTIVE_POLL_MS);
  assert.equal(calls, 3);

  loop.setJobActive(false);
  await flush();
  assert.equal(timers.activeInterval(), IDLE_POLL_MS);
});

test("never overlaps in-flight invokes (regression: 1s pile-up burned ~44% idle CPU)", async () => {
  const timers = new FakeTimers();
  let calls = 0;
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      await gate;
      return {};
    },
  });

  loop.start();
  await flush();
  assert.equal(calls, 1, "first tick fires immediately");

  await timers.advance(60000);
  assert.equal(calls, 1, "no stacked invokes while one is in flight");

  release();
  await flush();
  await timers.advance(0);
  assert.ok(calls <= 2, "at most one catch-up tick after completion");
});

test("pauses while hidden and resumes on visibilitychange", async () => {
  const timers = new FakeTimers();
  const doc = makeDoc({ initialHidden: true });
  let calls = 0;
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      return {};
    },
    doc,
  });

  loop.start();
  await flush();
  assert.equal(calls, 0, "no ticks while hidden");
  assert.equal(timers.activeInterval(), null, "no timer scheduled while hidden");

  doc.hidden = false;
  doc.dispatchVisibility();
  await flush();
  assert.equal(calls, 1);
  assert.equal(timers.activeInterval(), IDLE_POLL_MS);
});

test("pauses active polling when document becomes hidden", async () => {
  const timers = new FakeTimers();
  const doc = makeDoc({ initialHidden: false });
  let calls = 0;
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      return {};
    },
    doc,
  });

  loop.start();
  await flush();
  assert.equal(calls, 1);
  assert.equal(timers.activeInterval(), IDLE_POLL_MS);

  doc.hidden = true;
  doc.dispatchVisibility();
  await flush();
  assert.equal(timers.activeInterval(), null, "timer cleared when hidden");

  await timers.advance(IDLE_POLL_MS * 2);
  assert.equal(calls, 1, "no ticks while hidden");
});

test("forwards errors to onError callback without breaking future ticks", async () => {
  const timers = new FakeTimers();
  let calls = 0;
  const errors = [];
  const loop = makeLoop(timers, {
    invoke: async () => {
      calls += 1;
      if (calls === 1) throw new Error("IPC Failure");
      return { cpu_pct: 12 };
    },
    onError: (err) => errors.push(err.message),
  });

  loop.start();
  await flush();
  assert.equal(calls, 1);
  assert.deepEqual(errors, ["IPC Failure"]);

  await timers.advance(IDLE_POLL_MS);
  assert.equal(calls, 2);
});

