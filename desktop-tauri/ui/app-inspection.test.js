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
      if (command === "canonical_directory") return args.path;
      return { version: 2, items: [], subtitle_candidates: {} };
    } } },
  };
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.window = previousWindow;
  });
  const { scanInputDirectory } = await import("./app.js");
  assert.equal(await scanInputDirectory("/release", "remux"), true);
  assert.deepEqual(calls[0], ["canonical_directory", { path: "/release" }]);
  assert.deepEqual(calls[1], ["inspect_directory", { path: "/release", options: undefined }]);

  const options = { noCache: true };
  assert.equal(await scanInputDirectory("/release", "remux", { force: true, options }), true);
  assert.deepEqual(calls[3], ["inspect_directory", { path: "/release", options }]);
});

/** The Effective Settings the harness answers `get_default_settings` and `resolve_batch_request` with. */
const DEFAULT_SETTINGS = { workers: 6, io_jobs: 2, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };

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
    for (const id of ["run", "stop", "status", "log", "episode-list", "queue-count", "input-dir", "workers", "io-jobs"]) {
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
          return typeof result === "function" ? result(args) : result;
        }
        // A typed path resolves to itself unless a test says otherwise.
        if (command === "canonical_directory") return args.path;
        if (command === "get_default_settings" || command === "resolve_batch_request") return DEFAULT_SETTINGS;
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

/** What the running job of `activeSnapshot` runs with: nothing like `DEFAULT_SETTINGS`. */
const JOB_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

function activeSnapshot(status = "running") {
  return {
    job_id: 42, mode: "remux", status, start_time_ms: Date.now() - 2500,
    ordered_item_ids: ["two", "one"], total_items: 2, completed_items: 1,
    effective_settings: JOB_SETTINGS,
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

/** The harness's `canonical_directory` for a folder whose `/release/` and `/release/.` spellings are one. */
const trimDotSpellings = ({ path }) => path.replace(/\/\.?$/, "");

test("two spellings that resolve to one folder reuse the inspection and keep the cards", async t => {
  const ipc = bootstrapHarness(t, { canonical_directory: trimDotSpellings });
  const app = await ipc.mount("one-folder-two-spellings");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  const inspected = () => ipc.calls.filter(([command]) => command === "inspect_directory");
  const resolved = () => ipc.calls.filter(([command]) => command === "canonical_directory");

  assert.equal(await app.scanInputDirectory(" /release ", "remux"), true);
  assert.equal(await app.scanInputDirectory("/release/", "remux"), true);
  assert.equal(await app.scanInputDirectory("/release/.", "remux"), true);

  // The typed path is trimmed, then resolved; the inspection runs once, on the identity.
  assert.deepEqual(resolved().map(([, args]) => args), [{ path: "/release" }, { path: "/release/" }, { path: "/release/." }]);
  assert.deepEqual(inspected(), [["inspect_directory", { path: "/release", options: undefined }]]);

  // A forced rescan through another spelling keeps the shown cards instead of the scanning placeholder.
  const list = ipc.elements.get("remux-episode-list");
  const writes = [];
  Object.defineProperty(list, "innerHTML", { configurable: true, get: () => "", set: markup => writes.push(markup) });
  assert.equal(await app.scanInputDirectory("/release/.", "remux", { force: true }), true);
  assert.equal(inspected().length, 2);
  assert.equal(writes.some(markup => markup.includes("Scanning Directory")), false);

  // Another folder is a new scan: the list shows the placeholder.
  assert.equal(await app.scanInputDirectory("/other", "remux", { force: true }), true);
  assert.equal(writes.some(markup => markup.includes("Scanning Directory")), true);

  // An empty path resets the queue without asking the bridge.
  const resolvedBefore = resolved().length;
  assert.equal(await app.scanInputDirectory("   ", "remux"), false);
  assert.equal(resolved().length, resolvedBefore);
});

test("concurrent scans of two spellings inspect the identity once, and the newer request is the one accepted", async t => {
  const ipc = bootstrapHarness(t, { canonical_directory: trimDotSpellings });
  const app = await ipc.mount("concurrent-spellings");
  ipc.resolveSnapshot(null);
  await ipc.flush();

  const results = await Promise.all([app.scanInputDirectory("/release/", "remux"), app.scanInputDirectory("/release/.", "remux")]);

  assert.deepEqual(results, [false, true]);
  const inspected = ipc.calls.filter(([command]) => command === "inspect_directory");
  assert.deepEqual(inspected, [["inspect_directory", { path: "/release", options: undefined }]]);
  assert.equal(ipc.elements.get("remux-queue-count").textContent, "3 / 3 Selected");
});

/** A harness whose `canonical_directory` answers `/slow` only once `release()` is called. */
function slowCanonicalHarness(t) {
  let release;
  const slow = new Promise(resolve => { release = () => resolve("/slow"); });
  const ipc = bootstrapHarness(t, { canonical_directory: ({ path }) => (path === "/slow" ? slow : path) });
  const inspected = () => ipc.calls.filter(([command]) => command === "inspect_directory").map(([, args]) => args.path);
  return { ipc, release, inspected };
}

test("a scan whose canonicalization answers late does not overtake a newer request", async t => {
  const { ipc, release, inspected } = slowCanonicalHarness(t);
  const app = await ipc.mount("late-canonicalization");
  ipc.resolveSnapshot(null);
  await ipc.flush();

  const overtaken = app.scanInputDirectory("/slow", "remux");
  assert.equal(await app.scanInputDirectory("/fast", "remux"), true);
  release();

  assert.equal(await overtaken, false);
  assert.deepEqual(inspected(), ["/fast"]);
});

test("clearing the input cancels a scan still waiting for its canonicalization", async t => {
  const { ipc, release, inspected } = slowCanonicalHarness(t);
  const app = await ipc.mount("cleared-while-canonicalizing");
  ipc.resolveSnapshot(null);
  await ipc.flush();

  const pending = app.scanInputDirectory("/slow", "remux");
  assert.equal(await app.scanInputDirectory("   ", "remux"), false);
  release();

  assert.equal(await pending, false);
  assert.deepEqual(inspected(), []);
});

test("a pending scan of one view is not cancelled by a scan of the other", async t => {
  const { ipc, release, inspected } = slowCanonicalHarness(t);
  const app = await ipc.mount("one-queue-per-view");
  ipc.resolveSnapshot(null);
  await ipc.flush();

  const remux = app.scanInputDirectory("/slow", "remux");
  assert.equal(await app.scanInputDirectory("/fast", "extract"), true);
  release();

  assert.equal(await remux, true);
  assert.deepEqual(inspected().sort(), ["/fast", "/slow"]);
});

test("a path the bridge cannot resolve is inspected as typed and fails as before", async t => {
  const failure = new Error("Invalid directory path: No such file or directory (os error 2)");
  const ipc = bootstrapHarness(t, { canonical_directory: failure, inspect_directory: failure });
  const logged = t.mock.method(console, "log", () => {});
  const app = await ipc.mount("unresolvable-path");
  ipc.resolveSnapshot(null);
  await ipc.flush();

  assert.equal(await app.scanInputDirectory("/gone/", "remux"), false);

  assert.deepEqual(
    ipc.calls.filter(([command]) => command === "inspect_directory"),
    [["inspect_directory", { path: "/gone/", options: undefined }]],
  );
  assert.deepEqual(logged.mock.calls.at(-1).arguments, ["[inspect_directory] failed:", failure]);
  assert.match(ipc.elements.get("remux-episode-list").innerHTML, /Scan failed/);
  assert.equal(ipc.elements.get("remux-queue-count").textContent, "Scan failed");
});

test("get_default_settings puts auto (N) on the workers and Disk I/O inputs and the defaults on the idle chips", async t => {
  const ipc = bootstrapHarness(t, { get_default_settings: { ...DEFAULT_SETTINGS, workers: 6, io_jobs: 4 } });
  await ipc.mount("default-settings");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  for (const mode of ["remux", "extract"]) {
    const input = ipc.elements.get(`${mode}-workers`);
    assert.equal(input.placeholder, "auto (6)");
    assert.equal(input.dataset.autoValue, "6");
    const ioJobs = ipc.elements.get(`${mode}-io-jobs`);
    assert.equal(ioJobs.placeholder, "auto (4)");
    assert.equal(ioJobs.dataset.autoValue, "4");
  }
  assert.equal(ipc.workerChip.innerText, "⚡ Standby (6 Workers)");
  assert.equal(ipc.ioChip.innerText, "🔀 Idle (4 IO Jobs)");
});

test("a failed get_default_settings leaves the inputs on plain auto and does not write the chips", async t => {
  const ipc = bootstrapHarness(t, { get_default_settings: new Error("unavailable") });
  const logged = [];
  const priorError = console.error;
  console.error = (...args) => logged.push(args);
  t.after(() => { console.error = priorError; });
  await ipc.mount("default-settings-failed");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  for (const mode of ["remux", "extract"]) {
    const input = ipc.elements.get(`${mode}-workers`);
    assert.equal(input.placeholder, undefined);
    assert.equal("autoValue" in input.dataset, false);
  }
  assert.equal(ipc.workerChip.innerText, undefined);
  assert.equal(ipc.ioChip.innerText, undefined);
  assert.equal(logged.some(([label]) => label === "[get_default_settings] failed:"), true);
});

test("a restored active job's chips show the snapshot's Effective Settings, not the defaults", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("restored-settings");
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(ipc.ioChip.innerText, "🔀 5 IO Jobs Active");
});

test("with get_default_settings failed, a restored active job's chips still show its own settings", async t => {
  const ipc = bootstrapHarness(t, { get_default_settings: new Error("unavailable") });
  const priorError = console.error;
  console.error = () => {};
  t.after(() => { console.error = priorError; });
  await ipc.mount("restored-settings-unknown-defaults");
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(ipc.ioChip.innerText, "🔀 5 IO Jobs Active");
});

test("with no settings known anywhere, the chips name their state and invent no counts", async t => {
  const ipc = bootstrapHarness(t, { get_default_settings: new Error("unavailable") });
  const priorError = console.error;
  console.error = () => {};
  t.after(() => { console.error = priorError; });
  await ipc.mount("chips-without-settings");
  // A snapshot no `run_job` began, so it carries no Effective Settings.
  const { effective_settings: _omitted, ...withoutSettings } = activeSnapshot();
  ipc.resolveSnapshot(withoutSettings);
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "⚡ Workers Active");
  assert.equal(ipc.ioChip.innerText, "🔀 IO Jobs Active");

  ipc.emit("job-done", { job_id: 42, mode: "remux", code: 0 });
  assert.equal(ipc.workerChip.innerText, "⚡ Standby");
  assert.equal(ipc.ioChip.innerText, "🔀 Idle");
});

test("default settings that arrive after a job restored leave its chips alone, and idle chips return the defaults", async t => {
  let answerDefaults;
  const ipc = bootstrapHarness(t, { get_default_settings: new Promise(resolve => { answerDefaults = resolve; }) });
  await ipc.mount("restored-before-defaults");
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();
  answerDefaults({ ...DEFAULT_SETTINGS, workers: 6, io_jobs: 4 });
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(ipc.elements.get("remux-workers").placeholder, "auto (6)");

  ipc.emit("job-done", { job_id: 42, mode: "remux", code: 0 });
  assert.equal(ipc.workerChip.innerText, "⚡ Standby (6 Workers)");
  assert.equal(ipc.ioChip.innerText, "🔀 Idle (4 IO Jobs)");
});

test("the preset menu applies only what get_preset_profiles delivered", async t => {
  const fast = { id: "fast", icon: "⚡", name: "Fast Muxing (128k)", bitrate: "128k", complexity: 3 };
  const ipc = bootstrapHarness(t, { get_preset_profiles: [fast] });
  const options = ["fast", "hifi"].map(preset => Object.assign(new BootElement(), { dataset: { preset } }));
  for (const [id, value] of [["remux-bitrate", "192k"], ["remux-complexity", "5"], ["remux-cplx-val", "5"]]) {
    ipc.elements.set(id, new BootElement(value));
  }
  const profileName = new BootElement();
  const profileButton = Object.assign(new BootElement(), { focus() {} });
  ipc.elements.set("active-profile-name", profileName);
  ipc.elements.set("active-profile-btn", profileButton);
  ipc.elements.set("profile-dropdown-menu", new BootElement());
  const queryAll = globalThis.document.querySelectorAll;
  globalThis.document.querySelectorAll = selector =>
    selector === ".profile-option-item[data-preset]" ? options : queryAll(selector);

  await ipc.mount("preset-catalog");
  ipc.resolveSnapshot(null);
  await ipc.flush();
  const click = option => option.handlers.get("click")({ stopPropagation() {} });

  // "hifi" is not in the catalog, and the page carries no copy of it to fall back on.
  click(options[1]);
  assert.equal(ipc.elements.get("remux-bitrate").value, "192k");
  assert.equal(profileName.textContent, undefined);

  click(options[0]);
  assert.equal(ipc.elements.get("remux-bitrate").value, "128k");
  assert.equal(ipc.elements.get("remux-complexity").value, "3");
  assert.equal(profileName.textContent, "⚡ Fast Muxing (128k)");
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
