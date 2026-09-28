import assert from "node:assert/strict";
import test from "node:test";

test("app forwards preflight options to Tauri while browse uses defaults", async t => {
  const previousDocument = globalThis.document;
  const previousWindow = globalThis.window;
  const calls = [];
  globalThis.document = {
    readyState: "loading",
    addEventListener() {},
    getElementById: () => null,
  };
  globalThis.window = {
    __TAURI__: { core: { invoke: async (command, args) => {
      calls.push([command, args]);
      return { version: 2, items: [], subtitle_candidates: {} };
    } } },
  };
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.window = previousWindow;
  });
  const { scanInputDirectory } = await import("./app.js");
  assert.equal(await scanInputDirectory("/release", "remux"), true);
  assert.deepEqual(calls[0], ["inspect_directory", { path: "/release", options: undefined }]);

  const options = { noCache: true };
  assert.equal(await scanInputDirectory("/release", "remux", { force: true, options }), true);
  assert.deepEqual(calls[1], ["inspect_directory", { path: "/release", options }]);
});

class BootElement {
  constructor(value = "") {
    this.value = value;
    this.children = [];
    this.style = {};
    this.dataset = {};
    this.handlers = new Map();
    this.queried = new Map();
    const classes = new Set();
    this.classList = {
      add: name => classes.add(name),
      remove: name => classes.delete(name),
      contains: name => classes.has(name),
      toggle: (name, on) => on ? classes.add(name) : classes.delete(name),
    };
  }
  addEventListener(name, handler) { this.handlers.set(name, handler); }
  setAttribute() {}
  getAttribute() { return null; }
  appendChild(child) {
    if (child.fragment) this.children.push(...child.children);
    else this.children.push(child);
    return child;
  }
  remove() {}
  querySelector(selector) {
    if (!this.queried.has(selector)) this.queried.set(selector, new BootElement());
    return this.queried.get(selector);
  }
}

function bootstrapHarness(t) {
  const prior = {
    document: globalThis.document,
    window: globalThis.window,
    requestAnimationFrame: globalThis.requestAnimationFrame,
  };
  const elements = new Map();
  for (const mode of ["remux", "extract"]) {
    for (const id of ["run", "stop", "status", "log", "episode-list", "queue-count", "input-dir"]) {
      elements.set(`${mode}-${id}`, new BootElement());
    }
  }
  const workerChip = new BootElement();
  const ioChip = new BootElement();
  const documentHandlers = new Map();
  const handlers = new Map();
  const removed = [];
  const calls = [];
  let resolveSnapshot;
  let snapshotRequested;
  const snapshotPromise = new Promise(resolve => { resolveSnapshot = resolve; });
  const requestPromise = new Promise(resolve => { snapshotRequested = resolve; });
  globalThis.document = {
    readyState: "loading",
    hidden: true,
    documentElement: new BootElement(),
    getElementById: id => elements.get(id) ?? null,
    querySelector: () => null,
    querySelectorAll: selector => {
      if (selector === ".metric-workers-chip") return [workerChip];
      if (selector === ".metric-iojobs-chip") return [ioChip];
      return [];
    },
    createElement: () => new BootElement(),
    createDocumentFragment: () => Object.assign(new BootElement(), { fragment: true }),
    addEventListener: (name, handler) => documentHandlers.set(name, handler),
    removeEventListener: name => documentHandlers.delete(name),
  };
  globalThis.requestAnimationFrame = callback => callback();
  globalThis.window = {
    addEventListener() {},
    matchMedia: () => ({ matches: false, addEventListener() {} }),
    __TAURI__: {
      core: { invoke: async (command, args) => {
        calls.push([command, args]);
        if (command === "get_job_snapshot") {
          snapshotRequested();
          return snapshotPromise;
        }
        if (command === "inspect_directory") return { version: 2, items: [
          { id: "one", name: "one.mkv" }, { id: "two", name: "two.mkv" }, { id: "three", name: "three.mkv" },
        ] };
        return null;
      } },
      event: { listen: async (name, handler) => {
        assert.equal(handlers.has(name), false, `duplicate listener: ${name}`);
        handlers.set(name, handler);
        return () => { handlers.delete(name); removed.push(name); };
      } },
    },
  };
  t.after(() => {
    globalThis.document = prior.document;
    globalThis.window = prior.window;
    globalThis.requestAnimationFrame = prior.requestAnimationFrame;
  });
  return {
    elements, handlers, removed, calls, workerChip, ioChip, resolveSnapshot,
    emit: (name, payload) => handlers.get(name)({ payload }),
    async mount(name) {
      const app = await import(`./app.js?bootstrap-${name}`);
      documentHandlers.get("DOMContentLoaded")();
      await requestPromise;
      return app;
    },
    async flush() { await new Promise(resolve => setImmediate(resolve)); },
  };
}

function activeSnapshot(status = "running") {
  return {
    job_id: 42, mode: "remux", status, start_time_ms: Date.now() - 2500,
    ordered_item_ids: ["two", "one"], total_items: 2, completed_items: 1,
    items: {
      one: { id: "one", file: "one.mkv", status: "processing", progress: 70, start_time_ms: Date.now() - 1500 },
      two: { id: "two", file: "two.mkv", status: "completed", duration_sec: 1.5 },
    },
  };
}

test("bootstrap binds IPC before snapshot, restores active subset and targets Stop with the same ID", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("active");
  assert.ok(ipc.handlers.has("job-event"));
  assert.ok(ipc.handlers.has("job-done"));
  assert.equal(ipc.calls.some(([command]) => command === "inspect_directory"), false);
  ipc.emit("job-event", { version: 1, type: "item_progress", mode: "remux", job_id: 40, id: "one", progress: 99 });
  ipc.emit("job-event", { version: 1, type: "item_progress", mode: "remux", job_id: 42, id: "one", progress: 40 });
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();

  const list = ipc.elements.get("remux-episode-list");
  assert.deepEqual(list.children.map(card => card.querySelector(".ep-title").textContent), ["two.mkv", "one.mkv"]);
  assert.equal(list.children[1].querySelector(".ep-progress-fill").style.width, "70%");
  assert.equal(ipc.elements.get("remux-stop").disabled, false);
  assert.equal(ipc.elements.get("extract-run").disabled, true);
  assert.match(ipc.workerChip.innerText, /Active/);
  ipc.emit("job-done", { job_id: 40, mode: "remux", code: 0 });
  assert.equal(ipc.elements.get("remux-stop").disabled, false);
  await ipc.elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(ipc.calls.find(([command]) => command === "stop_job"), ["stop_job", { jobId: 42 }]);
  ipc.emit("job-event", { version: 1, type: "batch_complete", job_id: 42, mode: "remux", success: false, cancelled: true });
  ipc.emit("job-done", { job_id: 42, mode: "remux", code: 130 });
  assert.match(ipc.elements.get("remux-status").innerHTML, /Cancelled/);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.deepEqual(ipc.removed.filter(name => name === "job-event" || name === "job-done").sort(), ["job-done", "job-event"]);
});

test("completion buffered during snapshot fetch releases restored controller exactly once", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("early-done");
  ipc.emit("job-event", { version: 1, type: "item_complete", job_id: 42, mode: "remux", id: "one", status: "completed", duration_sec: 2 });
  ipc.emit("job-event", { version: 1, type: "batch_complete", job_id: 42, mode: "remux", success: true, cancelled: false });
  ipc.emit("job-done", { job_id: 41, mode: "remux", code: 0 });
  ipc.emit("job-done", { job_id: 42, mode: "remux", code: 0 });
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();
  assert.match(ipc.elements.get("remux-status").innerHTML, /Done/);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.handlers.has("job-done"), false);
  assert.equal(ipc.handlers.has("job-event"), false);
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".ep-progress-fill").style.width, "100%");
});

test("terminal snapshot restores presentation only and leaves no eager job listeners", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("terminal");
  const snapshot = activeSnapshot("completed");
  snapshot.items.one.status = "completed";
  snapshot.items.one.duration_sec = 8.4;
  ipc.resolveSnapshot(snapshot);
  await ipc.flush();
  assert.match(ipc.elements.get("remux-status").innerHTML, /Done/);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.handlers.has("job-event"), false);
  assert.equal(ipc.handlers.has("job-done"), false);
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".track-tag.magenta").textContent, "Done in 8.4s");
  assert.equal(ipc.calls.some(([command]) => command === "inspect_directory"), false);
});

test("no snapshot preserves pre-run selection and cleans up bootstrap listeners", async t => {
  const ipc = bootstrapHarness(t);
  const app = await ipc.mount("no-snapshot");
  assert.equal(await app.scanInputDirectory("/release", "remux"), true);
  const list = ipc.elements.get("remux-episode-list");
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: "two" }, checked: false }) } });
  ipc.resolveSnapshot(null);
  await ipc.flush();
  assert.equal(ipc.elements.get("remux-queue-count").textContent, "2 / 3 Selected");
  assert.equal(list.children.length, 3);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.handlers.has("job-event"), false);
  assert.equal(ipc.handlers.has("job-done"), false);
  assert.equal(ipc.calls.filter(([command]) => command === "inspect_directory").length, 1);
});
