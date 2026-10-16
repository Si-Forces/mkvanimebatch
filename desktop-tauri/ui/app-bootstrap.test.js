import assert from "node:assert/strict";
import test from "node:test";
import { RING_CIRCUMFERENCE } from "./queue-view.js";
import { DEFAULTS as DEFAULT_SETTINGS, createFakeCore, draft, jobView, modeView, row } from "./fake-core.js";

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
  /** Like the DOM, every listener of an event runs; `handlers` returns what the last one returned. */
  addEventListener(name, handler) {
    const prior = this.handlers.get(name);
    this.handlers.set(name, prior ? (...args) => { prior(...args); return handler(...args); } : handler);
  }
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

/** The rows a scan of any folder finds. */
const SCANNED = ["one", "two", "three"].map(id => row(id, { name: `${id}.mkv` }));

/**
 * The app over a fake Desktop Core. `answers` overrides what a command returns; a promise holds the
 * answer back until the test settles it. The first `get_state`, the boot's, waits for
 * `resolveState()`; set `core.state` before calling it. A scan accepts `SCANNED` (an empty path
 * resets the queue). `onListen(name)` runs once a listener is bound.
 */
function bootstrapHarness(t, answers = {}, { onListen = () => {} } = {}) {
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
  const core = createFakeCore({
    async onScan({ mode, path }) {
      if (!path.trim()) {
        core.state[mode] = modeView([], { manifest_rev: core.state[mode].manifest_rev + 1, draft: core.state[mode].draft });
        core.publish();
        return false;
      }
      core.accept(mode, path.trim(), structuredClone(SCANNED));
      return true;
    },
  });
  core.onPublish(state => handlers.get("view-state")?.({ payload: state }));
  let resolveState;
  let stateRequested;
  const stateGate = new Promise(resolve => { resolveState = resolve; });
  const requestPromise = new Promise(resolve => { stateRequested = resolve; });
  let booted = false;
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
        if (command === "resolve_batch_request") return DEFAULT_SETTINGS;
        if (command === "get_state" && !booted) {
          booted = true;
          stateRequested();
          await stateGate;
        }
        if (command === "run_job") return 42;
        return core.invoke(command, args);
      } },
      event: { listen: async (name, handler) => {
        assert.equal(handlers.has(name), false, `duplicate listener: ${name}`);
        handlers.set(name, handler);
        onListen(name);
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
    elements, handlers, removed, calls, core, workerChip, ioChip,
    resolveState: () => resolveState(),
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

/** What the running job of `activeJob` runs with: nothing like `DEFAULT_SETTINGS`. */
const JOB_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

/**
 * Puts running remux job 42 over "two" (done) and "one" (70%) in `core`, as a reload finds it: the
 * rows the Job Snapshot lists, which the reloaded queue's manifest does not.
 */
function activeJob(core, status = "running") {
  const jobOnly = (id, fields) => row(id, { name: `${id}.mkv`, release: "", metadata: null, in_manifest: false, participant: true, ...fields });
  core.state.job = jobView(["two", "one"], {
    job_id: 42, status, active: status === "running", start_time_ms: Date.now() - 2500, effective_settings: JOB_SETTINGS,
  });
  core.state.remux = modeView([
    jobOnly("two", { status: "completed", duration_sec: 1.5 }),
    jobOnly("one", status === "running"
      ? { status: "processing", progress: 70, start_time_ms: Date.now() - 1500 }
      : { status: "completed", duration_sec: 8.4 }),
  ], { directory: "/anime/show", manifest_rev: 1 });
  core.state.seq = 5;
}

/** Ends job 42 in `core` as `status`, settling its open rows, and publishes the state it ended in. */
function settleJob(core, status) {
  Object.assign(core.state.job, { status, active: false });
  for (const entry of core.state.remux.rows) {
    if (entry.status === "processing") entry.status = status === "cancelled" ? "cancelled" : "completed";
  }
  core.publish();
}

/** Ends job 42 as the core does: the state it ended in first, then its `job-done`. */
function endJob(ipc, status, code) {
  settleJob(ipc.core, status);
  ipc.emit("job-done", { job_id: 42, mode: "remux", code });
}

test("boot listens to view-state, renders the running job get_state holds, adopts it and targets Stop with its ID", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("active");
  assert.ok(ipc.handlers.has("view-state"), "view-state is bound before get_state");
  assert.equal(ipc.handlers.has("job-event"), false);
  activeJob(ipc.core);
  ipc.resolveState();
  await ipc.flush();

  const list = ipc.elements.get("remux-episode-list");
  assert.deepEqual(list.children.map(card => card.querySelector(".ep-title").textContent), ["two.mkv", "one.mkv"]);
  assert.equal(list.children[1].querySelector(".ep-ring-arc").style.strokeDashoffset, (RING_CIRCUMFERENCE * 0.3).toFixed(2));
  assert.equal(ipc.elements.get("remux-stop").disabled, false);
  assert.equal(ipc.elements.get("extract-run").disabled, true);
  assert.match(ipc.workerChip.innerText, /Active/);

  // A publication carries the job's progress.
  ipc.core.state.remux.rows[1].progress = 85;
  ipc.core.publish();
  assert.equal(list.children[1].querySelector(".ep-ring-arc").style.strokeDashoffset, (RING_CIRCUMFERENCE * 0.15).toFixed(2));

  ipc.emit("job-done", { job_id: 40, mode: "remux", code: 0 });
  assert.equal(ipc.elements.get("remux-stop").disabled, false);
  await ipc.elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(ipc.calls.find(([command]) => command === "stop_job"), ["stop_job", { jobId: 42 }]);
  endJob(ipc, "cancelled", -1);
  assert.match(ipc.elements.get("remux-status").innerHTML, /Cancelled/);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.deepEqual(ipc.removed, ["job-done"]);
});

test("a job that ends while boot binds its job-done ends on the state read after it, once", async t => {
  let ipc;
  ipc = bootstrapHarness(t, {}, {
    onListen: name => {
      // The job ends, and its job-done goes out, before the listener exists.
      if (name === "job-done") settleJob(ipc.core, "completed");
    },
  });
  await ipc.mount("early-done");
  activeJob(ipc.core);
  ipc.resolveState();
  await ipc.flush();
  assert.match(ipc.elements.get("remux-status").innerHTML, /Done/);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.handlers.has("job-done"), false);
});

test("a finished job sets the status pill and its rows, and binds no job-done", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("terminal");
  activeJob(ipc.core, "completed");
  ipc.resolveState();
  await ipc.flush();
  assert.match(ipc.elements.get("remux-status").innerHTML, /Done/);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.handlers.has("job-done"), false);
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".status-tag").textContent, "Done in 8.4s");
  assert.equal(ipc.core.intents.length, 0, "boot scans nothing");
});

test("an extract job is shown in the extract view", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("extract-job");
  ipc.core.state.job = jobView([], { mode: "extract", status: "completed", active: false });
  ipc.resolveState();
  await ipc.flush();
  assert.equal(globalThis.document.documentElement.dataset.view, "extract");
  assert.match(ipc.elements.get("extract-status").innerHTML, /Done/);
});

test("with no job, a scan and a selection made before get_state answers stay, and a stale publication is dropped", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("no-job");
  assert.equal(await ipc.scan("remux", "/release"), true);
  const list = ipc.elements.get("remux-episode-list");
  list.handlers.get("change")({ target: { closest: () => ({ dataset: { itemId: "two" }, checked: false }) } });
  await ipc.flush();
  assert.equal(ipc.elements.get("remux-queue-count").textContent, "2 / 3 Selected");

  // The boot's own get_state answers last, with the state already applied.
  ipc.resolveState();
  await ipc.flush();
  assert.equal(ipc.elements.get("remux-queue-count").textContent, "2 / 3 Selected");
  assert.equal(list.children.length, 3);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.handlers.has("job-done"), false);
  // The typed folder reached the Settings Draft before its scan.
  assert.deepEqual(ipc.core.intents.map(intent => intent.type), ["edit_draft", "scan_directory", "set_selected"]);

  // A publication older than the one applied changes nothing.
  ipc.emit("view-state", { ...structuredClone(ipc.core.state), seq: 1, remux: modeView() });
  assert.equal(list.children.length, 3);
});

// Wiring only: the chips', the preset menu's and the theme's own rules are tested in
// settings-chips.test.js, preset-menu.test.js and theme.test.js. Each assertion below fails when
// app.js drops the call or listener that connects its module.
test("app.js wires the settings chips, the preset menu and the theme to their modules", async t => {
  const fast = { id: "fast", name: "Fast muxing", bitrate: "128k", complexity: 3 };
  const ipc = bootstrapHarness(t);
  ipc.core.state.presets = [fast];

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

  // The sidebar's preset rows (rendered into #preset-list from the published catalog) and the
  // fields a profile fills.
  const presetList = Object.assign(new BootElement(), { querySelectorAll: () => [] });
  ipc.elements.set("preset-list", presetList);
  for (const [id, value] of [["remux-bitrate", "192k"], ["remux-complexity", "5"], ["remux-cplx-val", "5"]]) {
    ipc.elements.set(id, new BootElement(value));
  }

  await ipc.mount("module-wiring");
  assert.deepEqual(pinnedThemes(), ["dark"], "startup applies and pins the published choice");

  // An adopted running job owns the chips, though the state that holds it holds the defaults too.
  activeJob(ipc.core);
  ipc.resolveState();
  await ipc.flush();
  assert.equal(ipc.workerChip.innerText, "3 Workers Active");
  assert.equal(ipc.ioChip.innerText, "5 IO Jobs Active");
  assert.equal(ipc.elements.get("remux-workers").placeholder, "auto (6)");
  // The defaults reached the same chips: they fall back to them when the job ends.
  endJob(ipc, "completed", 0);
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
  // The core applies the profile and the published draft fills the fields.
  await presetList.handlers.get("click")({ target: { closest: selector => (matches(selector) ? fastRow : null) } });
  assert.deepEqual(ipc.core.intents.at(-1), { type: "set_preset", mode: "remux", preset: "fast" });
  assert.equal(ipc.elements.get("remux-bitrate").value, "128k");
  assert.equal(ipc.elements.get("remux-cplx-val").value, "3");
});

test("boot restores each mode's typed Settings Draft from get_state, and every Intent goes out in order", async t => {
  const ipc = bootstrapHarness(t);
  for (const id of ["remux-out-dir", "remux-bitrate", "extract-dry-run"]) ipc.elements.set(id, new BootElement());
  ipc.elements.get("extract-dry-run").type = "checkbox";
  await ipc.mount("draft-restore");
  ipc.core.state.remux.draft = draft("remux", { input_dir: "/anime/typed", out_dir: "/out", bitrate: "256k", preset: "custom" });
  ipc.core.state.extract.draft = draft("extract", { input_dir: "/other", dry_run: true });
  ipc.core.state.seq = 3;
  ipc.resolveState();
  await ipc.flush();
  assert.equal(ipc.elements.get("remux-input-dir").value, "/anime/typed");
  assert.equal(ipc.elements.get("remux-out-dir").value, "/out");
  assert.equal(ipc.elements.get("remux-bitrate").value, "256k");
  assert.equal(ipc.elements.get("extract-input-dir").value, "/other");
  assert.equal(ipc.elements.get("extract-dry-run").checked, true);
  assert.match(ipc.elements.get("extract-run").textContent, /^Plan/, "the restored dry run relabels Run");
  assert.equal(ipc.core.intents.length, 0, "boot scans and edits nothing");

  // Held answers: the second Intent goes out only once the first is answered.
  const held = [];
  const coreInvoke = ipc.core.invoke.bind(ipc.core);
  ipc.core.invoke = (command, args) => (command === "dispatch"
    ? new Promise(resolve => held.push(() => resolve(coreInvoke(command, args))))
    : coreInvoke(command, args));
  const workers = ipc.elements.get("remux-workers");
  workers.value = "4";
  workers.handlers.get("input")();
  const scanned = ipc.scan("extract", "/release");
  await ipc.flush();
  // Each goes out once the one before it was answered: the remux edit, then the extract field's
  // edit, then its rescan.
  for (const sent of [0, 1]) {
    assert.equal(ipc.core.intents.length, sent);
    assert.equal(held.length, 1);
    held.shift()();
    await ipc.flush();
  }
  assert.equal(held.length, 1, "the rescan is sent");
  // A scan holds the chain only until it is sent: an edit during it goes out at once.
  workers.value = "5";
  workers.handlers.get("input")();
  await ipc.flush();
  assert.equal(held.length, 2, "the edit did not wait for the scan's answer");
  held.shift()();
  held.shift()();
  assert.equal(await scanned, true);
  await ipc.flush();
  assert.deepEqual(ipc.core.intents.slice(0, 2), [
    { type: "edit_draft", mode: "remux", changes: { workers: "4" } },
    { type: "edit_draft", mode: "extract", changes: { input_dir: "/release" } },
  ]);
  assert.equal(ipc.core.state.remux.draft.workers, "5");
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
    ipc.resolveState();
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
    // The core publishes a Track Assignment without its null fields; the engine reads them as unset.
    assert.deepEqual(assignment, {
      target_video: "one",
      asset_path: "/custom/sub.ass",
      track_type: "subtitle",
      language: "ind",
      ...(expected === null ? {} : { offset_ms: expected }),
    });
    assert.deepEqual(ipc.core.intents.find(intent => intent.type === "assign_track").assignment.offset_ms, expected);
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
  ipc.resolveState();
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
  // The assignment's Intent goes out after the ones before it.
  await ipc.flush();
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
  ipc.resolveState();
  await ipc.flush();

  assignSelect.value = "__browse__";
  await assignSelect.handlers.get("change")({ target: assignSelect });
  await reset.handlers.get("click")();
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
  ipc.resolveState();
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
