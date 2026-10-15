import assert from "node:assert/strict";
import test from "node:test";
import { RING_CIRCUMFERENCE } from "./queue-view.js";

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
  append(...nodes) { this.children.push(...nodes); }
  remove() {}
  querySelector(selector) {
    if (!this.queried.has(selector)) this.queried.set(selector, new BootElement());
    return this.queried.get(selector);
  }
}

/** `answers` overrides what a command returns; a promise holds the answer back until the test settles it. */
function bootstrapHarness(t, answers = {}) {
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
        if (command in answers) return answers[command];
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
      await import(`./app.js?bootstrap-${name}`);
      documentHandlers.get("DOMContentLoaded")();
      await requestPromise;
    },
    async flush() { await new Promise(resolve => setImmediate(resolve)); },
    /** Types `path` into the mode's input directory and leaves the field, which rescans it. */
    scan(mode, path) {
      const input = elements.get(`${mode}-input-dir`);
      input.value = path;
      return input.handlers.get("change")();
    },
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
  assert.equal(list.children[1].querySelector(".ep-ring-arc").style.strokeDashoffset, (RING_CIRCUMFERENCE * 0.3).toFixed(2));
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
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".ep-status").className, "ep-status tone-ok");
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
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".status-tag").textContent, "Done in 8.4s");
  assert.equal(ipc.calls.some(([command]) => command === "inspect_directory"), false);
});

test("no snapshot preserves pre-run selection and cleans up bootstrap listeners", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("no-snapshot");
  assert.equal(await ipc.scan("remux", "/release"), true);
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

// Wiring only: the chips', the preset menu's and the theme's own rules are tested in
// settings-chips.test.js, preset-menu.test.js and theme.test.js. Each assertion below fails when
// app.js drops the call or listener that connects its module.
test("app.js wires the settings chips, the preset menu and the theme to their modules", async t => {
  let answerDefaults;
  const fast = { id: "fast", name: "Fast muxing", bitrate: "128k", complexity: 3 };
  const ipc = bootstrapHarness(t, {
    get_default_settings: new Promise(resolve => { answerDefaults = resolve; }),
    get_preset_profiles: [fast],
  });

  // theme-init.js published a stored dark choice, and localStorage records what the toggle saves.
  const attributes = new Map();
  Object.assign(globalThis.document.documentElement, {
    dataset: { theme: "dark", themeExplicit: "true" },
    setAttribute: (name, value) => attributes.set(name, value),
    getAttribute: name => attributes.get(name) ?? null,
  });
  const stored = [];
  const priorStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  // Like a real Storage, setItem refuses any other receiver ("Illegal invocation"), so passing
  // the bare method as theme storage would silently stop saving the choice and fail here.
  const fakeStorage = {
    setItem(key, value) {
      if (this !== fakeStorage) throw new TypeError("Illegal invocation");
      stored.push([key, value]);
    },
  };
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: fakeStorage });
  t.after(() => {
    if (priorStorage) Object.defineProperty(globalThis, "localStorage", priorStorage);
    else delete globalThis.localStorage;
  });
  const themeButton = new BootElement();
  ipc.elements.set("btn-theme", themeButton);
  const pinnedThemes = () => ipc.calls.filter(([command]) => command === "set_window_theme").map(([, args]) => args.theme);

  // The sidebar's preset rows (rendered into #preset-list once the catalog loads) and the
  // fields a profile fills.
  const presetList = Object.assign(new BootElement(), { querySelectorAll: () => [] });
  ipc.elements.set("preset-list", presetList);
  for (const [id, value] of [["remux-bitrate", "192k"], ["remux-complexity", "5"], ["remux-cplx-val", "5"]]) {
    ipc.elements.set(id, new BootElement(value));
  }

  await ipc.mount("module-wiring");
  assert.deepEqual(pinnedThemes(), ["dark"], "startup applies and pins the published choice");

  // A restored active job owns the chips, and the defaults answer only after it restored.
  ipc.resolveSnapshot(activeSnapshot());
  await ipc.flush();
  answerDefaults(DEFAULT_SETTINGS);
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "3 Workers Active");
  assert.equal(ipc.ioChip.innerText, "5 IO Jobs Active");
  assert.equal(ipc.elements.get("remux-workers").placeholder, "auto (6)");
  // The defaults reached the same chips: they fall back to them when the job ends.
  ipc.emit("job-done", { job_id: 42, mode: "remux", code: 0 });
  assert.equal(ipc.workerChip.innerText, "Standby (6 Workers)");
  assert.equal(ipc.ioChip.innerText, "Idle (2 IO Jobs)");

  themeButton.handlers.get("click")();
  assert.equal(attributes.get("data-theme"), "light");
  assert.deepEqual(stored, [["mkv-theme", "light"]]);
  assert.deepEqual(pinnedThemes(), ["dark", "light"]);

  assert.equal(presetList.children.length, 1, "the catalog's one profile became a row");
  const fastRow = presetList.children[0];
  const matches = selector => selector === ".profile-option-item[data-preset]" &&
    fastRow.className.split(" ").includes("profile-option-item") && Boolean(fastRow.dataset.preset);
  presetList.handlers.get("click")({ target: { closest: selector => (matches(selector) ? fastRow : null) } });
  assert.equal(ipc.elements.get("remux-bitrate").value, "128k");
});

for (const { label, input, expected } of [
  { label: "empty string", input: "", expected: null },
  { label: "whitespace string", input: "   ", expected: null },
  { label: "exponential notation 1e3", input: "1e3", expected: 1000 },
  { label: "negative fraction -250.9", input: "-250.9", expected: -250 },
  { label: "beyond the safe-integer range 1e20", input: "1e20", expected: null },
]) {
  test(`track-assignment offset parsing: ${label} produces ${expected}`, async t => {
    const ipc = bootstrapHarness(t, { pick_subtitle_file: "/custom/sub.ass" });
    const assignSelect = new BootElement();
    const offsetInput = new BootElement();
    const langSelect = new BootElement("ind");
    const titleInput = new BootElement();
    ipc.elements.set("sub-assign-select", assignSelect);
    ipc.elements.set("sub-offset-input", offsetInput);
    ipc.elements.set("sub-lang-select", langSelect);
    ipc.elements.set("sub-title-input", titleInput);

    await ipc.mount(`sub-offset-${label.replace(/\s+/g, "-")}`);
    assert.equal(await ipc.scan("remux", "/release"), true);
    ipc.resolveSnapshot(null);
    await ipc.flush();

    // Pick the row into the inspector, then choose a subtitle file from its assign select.
    const list = ipc.elements.get("remux-episode-list");
    list.handlers.get("click")({
      target: { closest: selector => (selector === ".ep-pick" ? { dataset: { itemId: "one" } } : null) },
    });
    offsetInput.value = input;
    assignSelect.value = "__browse__";
    await assignSelect.handlers.get("change")({ target: assignSelect });

    await ipc.elements.get("remux-run").handlers.get("click")();
    await ipc.flush();

    const runCall = ipc.calls.find(([command]) => command === "run_job");
    assert.ok(runCall, "run_job was invoked");
    const assignment = runCall[1].request.assignments.find(a => a.target_video === "one");
    assert.deepEqual(assignment, {
      target_video: "one",
      asset_path: "/custom/sub.ass",
      track_type: "subtitle",
      language: "ind",
      title: null,
      offset_ms: expected,
    });
  });
}

test("choosing a custom language tag keeps the tag box open until the tag is typed", async t => {
  const ipc = bootstrapHarness(t, { pick_subtitle_file: "/custom/sub.ass" });
  const assignSelect = new BootElement();
  const langSelect = new BootElement("ind");
  const langCustom = new BootElement();
  const customLabel = new BootElement();
  ipc.elements.set("sub-assign-select", assignSelect);
  ipc.elements.set("sub-lang-select", langSelect);
  ipc.elements.set("sub-lang-custom", langCustom);
  ipc.elements.set("sub-lang-custom-label", customLabel);
  // The inspector draws only when its body exists.
  ipc.elements.set("insp-body", new BootElement());
  await ipc.mount("sub-custom-lang");
  assert.equal(await ipc.scan("remux", "/release"), true);
  ipc.resolveSnapshot(null);
  await ipc.flush();

  ipc.elements.get("remux-episode-list").handlers.get("click")({
    target: { closest: selector => (selector === ".ep-pick" ? { dataset: { itemId: "one" } } : null) },
  });
  assignSelect.value = "__browse__";
  await assignSelect.handlers.get("change")({ target: assignSelect });
  await ipc.flush();

  // The assignment briefly has no language while the tag box is empty; the redraw must not
  // read that as the "ind" default and snap the select back.
  langSelect.value = "custom";
  langSelect.handlers.get("change")();
  await ipc.flush();
  assert.equal(langSelect.value, "custom");
  assert.equal(customLabel.hidden, false);

  langCustom.value = "may";
  langCustom.handlers.get("change")();
  await ipc.elements.get("remux-run").handlers.get("click")();
  await ipc.flush();
  const request = ipc.calls.find(([command]) => command === "run_job")[1].request;
  assert.equal(request.assignments.find(a => a.target_video === "one").language, "may");
});

test("the inspector's Reset clears a Track Assignment and a running job refuses a new one", async t => {
  const ipc = bootstrapHarness(t, { pick_subtitle_file: "/custom/sub.ass" });
  const assignSelect = new BootElement();
  const reset = new BootElement();
  ipc.elements.set("sub-assign-select", assignSelect);
  ipc.elements.set("sub-assign-reset", reset);
  await ipc.mount("sub-reset");
  assert.equal(await ipc.scan("remux", "/release"), true);
  ipc.resolveSnapshot(null);
  await ipc.flush();

  assignSelect.value = "__browse__";
  await assignSelect.handlers.get("change")({ target: assignSelect });
  reset.handlers.get("click")();
  await ipc.elements.get("remux-run").handlers.get("click")();
  await ipc.flush();
  const request = ipc.calls.find(([command]) => command === "run_job")[1].request;
  assert.equal(request.assignments, null);

  // The job now runs: a pick is refused before the file dialog opens.
  const picks = ipc.calls.filter(([command]) => command === "pick_subtitle_file").length;
  await assignSelect.handlers.get("change")({ target: assignSelect });
  assert.equal(ipc.calls.filter(([command]) => command === "pick_subtitle_file").length, picks);
});

test("the log shows paths Finder-style, Copy puts the real paths on the clipboard, and errors badge the Log button", async t => {
  const ipc = bootstrapHarness(t);
  const copyBtn = new BootElement();
  const badge = new BootElement();
  ipc.elements.set("log-copy", copyBtn);
  ipc.elements.set("log-badge", badge);
  const copied = [];
  const priorNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: { clipboard: { writeText: async text => { copied.push(text); } } },
  });
  // Node before 21 has no global navigator, so there may be nothing to restore.
  t.after(() => {
    if (priorNavigator) Object.defineProperty(globalThis, "navigator", priorNavigator);
    else delete globalThis.navigator;
  });

  await ipc.mount("log-copy");
  assert.equal(await ipc.scan("remux", "/release"), true);
  ipc.resolveSnapshot(null);
  await ipc.flush();
  await ipc.elements.get("remux-run").handlers.get("click")();
  await ipc.flush();

  ipc.emit("job-log", [
    { kind: "succeeded", text: "  Completed -> /media/anime/Show/Season 1/Output/Show - 01.mkv" },
    { kind: "error", text: "ERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv" },
  ]);
  const log = ipc.elements.get("remux-log");
  assert.deepEqual(log.children.map(line => line.textContent), [
    "  Completed -> media › anime › Show › Season 1 › Output › Show - 01.mkv",
    "ERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv",
  ]);
  assert.deepEqual(log.children.map(line => line.className), ["log-line line-ok", "log-line line-err"]);
  assert.deepEqual(log.children.map(line => line.dataset.kind), ["succeeded", "error"]);
  assert.equal(badge.hidden, false);
  assert.equal(badge.textContent, "1");

  await copyBtn.handlers.get("click")();
  // Copy is each line's text, as the CLI prints it: an error line gets no added prefix.
  assert.deepEqual(copied, [
    "  Completed -> /media/anime/Show/Season 1/Output/Show - 01.mkv\nERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv",
  ]);
});
