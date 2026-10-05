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

function bootstrapHarness(t, commandResults = {}) {
  const prior = {
    document: globalThis.document,
    window: globalThis.window,
    requestAnimationFrame: globalThis.requestAnimationFrame,
  };
  const elements = new Map();
  for (const mode of ["remux", "extract"]) {
    for (const id of ["run", "stop", "status", "log", "episode-list", "queue-count", "input-dir", "workers"]) {
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
        if (command in commandResults) {
          const result = commandResults[command];
          if (result instanceof Error) throw result;
          return result;
        }
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
  ipc.emit("job-done", { job_id: 42, mode: "remux", code: -1 });
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

test("get_default_workers puts auto (N) on both workers inputs and N in the standby chip", async t => {
  const ipc = bootstrapHarness(t, { get_default_workers: 6 });
  await ipc.mount("default-workers");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  for (const mode of ["remux", "extract"]) {
    const input = ipc.elements.get(`${mode}-workers`);
    assert.equal(input.placeholder, "auto (6)");
    assert.equal(input.dataset.autoValue, "6");
  }
  assert.equal(ipc.workerChip.innerText, "⚡ Standby (6 Workers)");
});

test("a failed get_default_workers leaves the inputs on plain auto and the chip on 8", async t => {
  const ipc = bootstrapHarness(t, { get_default_workers: new Error("unavailable") });
  const logged = [];
  const priorError = console.error;
  console.error = (...args) => logged.push(args);
  t.after(() => { console.error = priorError; });
  await ipc.mount("default-workers-failed");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  for (const mode of ["remux", "extract"]) {
    const input = ipc.elements.get(`${mode}-workers`);
    assert.equal(input.placeholder, undefined);
    assert.equal("autoValue" in input.dataset, false);
  }
  assert.equal(ipc.workerChip.innerText, undefined);
  assert.equal(logged.some(([label]) => label === "[get_default_workers] failed:"), true);
});

test("explicit toggle survives a failed storage write when the OS scheme changes", async t => {
  const ipc = bootstrapHarness(t);
  const priorStorage = globalThis.localStorage;
  const attributes = new Map();
  const schemeHandlers = [];
  globalThis.localStorage = { getItem: () => null, setItem: () => { throw new Error("storage blocked"); } };
  globalThis.document.documentElement = {
    dataset: {},
    setAttribute: (name, value) => attributes.set(name, value),
    getAttribute: name => attributes.get(name) ?? null,
    classList: { toggle() {} },
  };
  globalThis.window.matchMedia = () => ({ matches: false, addEventListener: (name, handler) => schemeHandlers.push(handler) });
  const themeButton = new BootElement();
  ipc.elements.set("btn-theme", themeButton);
  t.after(() => { globalThis.localStorage = priorStorage; });

  await ipc.mount("sticky-theme");
  assert.equal(attributes.get("data-theme"), "light");
  themeButton.handlers.get("click")();
  assert.equal(attributes.get("data-theme"), "dark");
  // The DOM is the single source of truth after a toggle: an explicit choice must be
  // published to dataset.themeExplicit, or storage and the dataset disagree again.
  assert.equal(globalThis.document.documentElement.dataset.themeExplicit, "true");

  schemeHandlers.forEach(handler => handler({ matches: false }));
  assert.equal(attributes.get("data-theme"), "dark");
});

test("an unrecognized stored theme follows the OS scheme live", async t => {
  const ipc = bootstrapHarness(t);
  const priorStorage = globalThis.localStorage;
  const attributes = new Map();
  const schemeHandlers = [];
  globalThis.localStorage = { getItem: () => "blue" };
  globalThis.document.documentElement = {
    setAttribute: (name, value) => attributes.set(name, value),
    getAttribute: name => attributes.get(name) ?? null,
    classList: { toggle() {} },
  };
  globalThis.window.matchMedia = () => ({ matches: false, addEventListener: (name, handler) => schemeHandlers.push(handler) });
  t.after(() => { globalThis.localStorage = priorStorage; });

  await ipc.mount("invalid-stored-theme");
  assert.equal(attributes.get("data-theme"), "light");
  assert.equal(ipc.calls.some(([command]) => command === "set_window_theme"), false);

  schemeHandlers.forEach(handler => handler({ matches: true }));
  assert.equal(attributes.get("data-theme"), "dark");
  ipc.resolveSnapshot(null);
  await ipc.flush();
});

test("app.js never re-derives the boot theme from localStorage when the bootstrap is absent", async t => {
  const ipc = bootstrapHarness(t);
  const priorStorage = globalThis.localStorage;
  const storageReads = [];
  const attributes = new Map();
  globalThis.localStorage = {
    getItem: key => { storageReads.push(key); return "dark"; },
    setItem: () => {},
  };
  // theme-init.js published nothing, so the bootstrap never ran. The stored theme must be
  // resolved by theme-init.js alone; app.js may only fall back to the live OS scheme.
  globalThis.document.documentElement.dataset = {};
  globalThis.document.documentElement.setAttribute = (name, value) => attributes.set(name, value);
  globalThis.document.documentElement.getAttribute = name => attributes.get(name) ?? null;
  t.after(() => { globalThis.localStorage = priorStorage; });

  await ipc.mount("trusts-published-theme");

  assert.deepEqual(storageReads, [], "app.js must not re-derive the boot theme from localStorage");
  assert.equal(attributes.get("data-theme"), "light", "without a published theme the OS scheme wins");
  assert.equal(ipc.calls.some(([command]) => command === "set_window_theme"), false,
    "following the OS must leave the native window unpinned");
});

for (const { label, input, expected } of [
  { label: "empty string", input: "", expected: null },
  { label: "whitespace string", input: "   ", expected: null },
  { label: "exponential notation 1e3", input: "1e3", expected: 1000 },
  { label: "negative fraction -250.9", input: "-250.9", expected: -250 },
  { label: "beyond the safe-integer range 1e20", input: "1e20", expected: null },
]) {
  test(`track-assignment offset parsing: ${label} produces ${expected}`, async t => {
    const ipc = bootstrapHarness(t);
    const modal = new BootElement();
    const saveBtn = new BootElement();
    const customPath = new BootElement();
    const offsetInput = new BootElement();
    const langSelect = new BootElement("ind");
    const titleInput = new BootElement();
    ipc.elements.set("sub-assign-modal", modal);
    ipc.elements.set("sub-assign-save", saveBtn);
    ipc.elements.set("sub-custom-path", customPath);
    ipc.elements.set("sub-offset-input", offsetInput);
    ipc.elements.set("sub-lang-select", langSelect);
    ipc.elements.set("sub-title-input", titleInput);

    const app = await ipc.mount(`sub-offset-${label.replace(/\s+/g, "-")}`);
    assert.equal(await app.scanInputDirectory("/release", "remux"), true);
    ipc.elements.get("remux-input-dir").value = "/release";
    ipc.resolveSnapshot(null);
    await ipc.flush();

    const list = ipc.elements.get("remux-episode-list");
    list.handlers.get("click")({
      target: { closest: () => ({ dataset: { itemId: "one" } }) },
    });
    customPath.value = "/custom/sub.ass";
    offsetInput.value = input;
    saveBtn.handlers.get("click")();

    await ipc.elements.get("remux-run").handlers.get("click")();
    await ipc.flush();

    const runCall = ipc.calls.find(([command]) => command === "run_job");
    assert.ok(runCall, "run_job was invoked");
    const assignment = runCall[1].request.assignments.find(a => a.target_video === "one");
    assert.equal(assignment.offset_ms, expected);
  });
}
