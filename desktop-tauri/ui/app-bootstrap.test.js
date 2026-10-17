import assert from "node:assert/strict";
import test from "node:test";
import { RING_CIRCUMFERENCE } from "./queue-view.js";
import { controls, createFakeCore, draft, inspectorView, jobView, modeView, row } from "./fake-core.js";

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
  focus() {}
  getAttribute() { return null; }
  appendChild(child) {
    for (const node of child.fragment ? child.children : [child]) {
      node.parent = this;
      this.children.push(node);
    }
    return child;
  }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) {
    this.children = [];
    for (const node of nodes) this.appendChild(node);
  }
  get firstElementChild() { return this.children[0] ?? null; }
  remove() {
    if (this.parent) this.parent.children = this.parent.children.filter(node => node !== this);
  }
  querySelector(selector) {
    if (!this.queried.has(selector)) this.queried.set(selector, new BootElement());
    return this.queried.get(selector);
  }
}

/** More `get_state` reads than any test makes. */
const MAX_STATE_READS = 20;

/** The rows a scan of any folder finds. */
const SCANNED = ["one", "two", "three"].map(id => row(id, { name: `${id}.mkv` }));

/**
 * The app over a fake Desktop Core. `answers` overrides what a command returns, read at each call
 * (so a test can set one after boot); a promise holds the answer back until the test settles it.
 * The first `get_state`, the boot's, waits for `resolveState()`; set `core.state` before
 * calling it. A scan accepts `SCANNED` (an empty path resets the queue). `onListen(name)` runs
 * once a listener is bound, and `listen` resolves once it settles.
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
    // The status pill's markup: the dot, then the label's text node.
    elements.get(`${mode}-status`).lastChild = { textContent: " Idle" };
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
  core.onLog(batch => handlers.get("job-log")?.({ payload: batch }));
  let resolveState;
  let stateRequested;
  const stateGate = new Promise(resolve => { resolveState = resolve; });
  const requestPromise = new Promise(resolve => { stateRequested = resolve; });
  let booted = false;
  let stateReads = 0;
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
        // A resync that never converges re-reads get_state in microtasks only, which no bounded
        // wait or test timeout interrupts; refusing past a cap ends it, and the test fails.
        if (command === "get_state" && ++stateReads > MAX_STATE_READS) throw new Error("get_state read in a loop");
        if (command === "get_state" && !booted) {
          booted = true;
          stateRequested();
          await stateGate;
        }
        if (command in answers) return answers[command];
        return core.invoke(command, args);
      } },
      event: { listen: async (name, handler) => {
        assert.equal(handlers.has(name), false, `duplicate listener: ${name}`);
        handlers.set(name, handler);
        await onListen(name);
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
    elements, handlers, removed, calls, core, workerChip, ioChip, documentHandlers,
    resolveState: () => resolveState(),
    emit: (name, payload) => handlers.get(name)({ payload }),
    async mount(name) {
      await import(`./app.js?bootstrap-${name}`);
      documentHandlers.get("DOMContentLoaded")();
      await requestPromise;
    },
    /** Settles what the app sent, and the publication the fake core makes after a draft Intent's answer. */
    async flush() {
      for (let turn = 0; turn < 2; turn++) await new Promise(resolve => setImmediate(resolve));
    },
    /** Types `path` into the mode's input directory and leaves the field, which rescans it. */
    scan(mode, path) {
      const input = elements.get(`${mode}-input-dir`);
      input.value = path;
      return input.handlers.get("change")();
    },
  };
}

/** A remux view's patched outcome, with the controls whose pill the core derives from it. */
const outcomePatch = (outcome, label) => ({
  outcome,
  controls: controls("remux", label ? { status: { class: `status-${outcome === "done" ? "success" : outcome}`, label } } : {}),
});

/** What the running job of `activeJob` runs with: nothing like `DEFAULT_SETTINGS`. */
const JOB_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

/** The outcome a finished job of `status` leaves its mode with. */
const OUTCOMES = { completed: "done", cancelled: "cancelled", error: "failed" };

/**
 * Puts remux job 42 over "two" (done) and "one" (70%) in `core`, as a reload finds it: the rows
 * the Job Snapshot lists, which the reloaded queue's manifest does not. A running one owns the
 * Job Coordinator; a finished one left its outcome.
 */
function activeJob(core, status = "running") {
  const jobOnly = (id, fields) => row(id, { name: `${id}.mkv`, release: "", in_manifest: false, participant: true, ...fields });
  const done = label => ({ status: "completed", tone: "ok", label, bucket: "done" });
  core.state.job = jobView({ job_id: 42, status, active: status === "running", effective_settings: JOB_SETTINGS });
  core.state.remux = modeView([
    jobOnly("two", done("Done in 1.5s")),
    jobOnly("one", status === "running"
      ? { status: "processing", progress: 70, tone: "info", label: "70% · 1s" }
      : done("Done in 8.4s")),
  ], { directory: "/anime/show", manifest_rev: 1, outcome: OUTCOMES[status] ?? null });
  if (status === "running") core.state.run = { phase: "running", mode: "remux", settings: JOB_SETTINGS };
  core.state.seq = 5;
}

/** Ends job 42 in `core` as `status`, settling its open rows, and publishes the state it ended in. */
function settleJob(core, status) {
  for (const entry of core.state.remux.rows) {
    if (entry.status === "processing") entry.status = status === "cancelled" ? "cancelled" : "completed";
  }
  core.end(OUTCOMES[status], status);
}

test("boot listens to view-state and job-log, renders the running job get_state holds, and Stop stops its mode", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("active");
  assert.ok(ipc.handlers.has("view-state"), "view-state is bound before get_state");
  assert.ok(ipc.handlers.has("job-log"), "job-log is bound before get_state");
  assert.equal(ipc.handlers.has("job-event"), false);
  activeJob(ipc.core);
  ipc.resolveState();
  await ipc.flush();
  // The boot reads both Run Logs after the state, and tells the core the window is hidden.
  const commands = ipc.calls.map(([command]) => command);
  assert.ok(commands.indexOf("get_log") > commands.indexOf("get_state"), commands.join(", "));
  assert.deepEqual(ipc.calls.filter(([command]) => command === "get_log").map(([, args]) => args.mode), ["remux", "extract"]);
  assert.equal(ipc.core.visible, false);

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

  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Running/);
  // The page no longer listens to job-done: the published outcome ends the job.
  assert.equal(ipc.handlers.has("job-done"), false);
  // The fake core ends the job at Stop; its row ends with it, as the job's own events settle it.
  ipc.core.state.remux.rows[1].status = "cancelled";
  await ipc.elements.get("remux-stop").handlers.get("click")();
  assert.deepEqual(ipc.core.intents.at(-1), { type: "stop", mode: "remux" });
  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Cancelled/);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
});

test("a finished job sets the status pill and its rows, and binds no job-done", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("terminal");
  activeJob(ipc.core, "completed");
  ipc.resolveState();
  await ipc.flush();
  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Done/);
  assert.equal(ipc.elements.get("remux-run").disabled, false);
  assert.equal(ipc.elements.get("remux-stop").disabled, true);
  assert.equal(ipc.handlers.has("job-done"), false);
  assert.equal(ipc.elements.get("remux-episode-list").children[1].querySelector(".status-tag").textContent, "Done in 8.4s");
  assert.equal(ipc.core.intents.length, 0, "boot scans nothing");
});

test("an extract job is shown in the extract view", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("extract-job");
  ipc.core.state.job = jobView({ mode: "extract", status: "completed", active: false });
  ipc.core.state.extract.outcome = "done";
  ipc.resolveState();
  await ipc.flush();
  assert.equal(globalThis.document.documentElement.dataset.view, "extract");
  assert.match(ipc.elements.get("extract-status").lastChild.textContent, /Done/);
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

test("a view-state patch redraws only the cards of the rows it changes", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("patch-rows");
  activeJob(ipc.core);
  ipc.resolveState();
  await ipc.flush();
  const list = ipc.elements.get("remux-episode-list");
  const check = index => list.children[index].querySelector(".ep-select-check");
  const arc = index => list.children[index].querySelector(".ep-ring-arc");
  // A mark a redraw of the first card would overwrite.
  check(0).checked = "untouched";

  ipc.core.state.remux.rows[1].progress = 85;
  ipc.core.publishPatch({ remux: { row_changes: [[1, ipc.core.state.remux.rows[1]]] } });
  assert.equal(arc(1).style.strokeDashoffset, (RING_CIRCUMFERENCE * 0.15).toFixed(2));
  assert.equal(check(0).checked, "untouched");
  assert.equal(ipc.calls.filter(([command]) => command === "get_state").length, 1);

  // A whole state redraws every card; this one ends the job.
  settleJob(ipc.core, "completed");
  assert.equal(check(0).checked, true);
});

test("a patch of a state the page missed reads get_state once, and the patches meanwhile wait for it", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("patch-resync");
  ipc.resolveState();
  await ipc.flush();
  const reads = () => ipc.calls.filter(([command]) => command === "get_state").length;
  const before = reads();
  // The page missed the patch at seq + 1: the next two cannot apply.
  const seq = ipc.core.state.seq;
  ipc.core.state.seq = seq + 3;
  ipc.core.state.remux.outcome = "done";
  ipc.emit("view-state", { seq: seq + 2, base: seq + 1 });
  ipc.emit("view-state", { seq: seq + 3, base: seq + 2, remux: outcomePatch("done", "Done") });
  await ipc.flush();
  assert.equal(reads(), before + 1);
  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Done/);

  // The next patch builds on the state get_state answered with.
  ipc.core.state.remux.outcome = null;
  ipc.core.publishPatch({ remux: outcomePatch(null) });
  await ipc.flush();
  assert.equal(reads(), before + 1);
  assert.doesNotMatch(ipc.elements.get("remux-status").lastChild.textContent, /Done/);
});

test("a resync whose get_state answer is older than a patch that arrived during the read still applies that patch", async t => {
  const answers = {};
  const ipc = bootstrapHarness(t, answers);
  await ipc.mount("patch-stale-answer");
  ipc.resolveState();
  await ipc.flush();
  const reads = () => ipc.calls.filter(([command]) => command === "get_state").length;
  const before = reads();
  const seq = ipc.core.state.seq;
  // The read is answered with the state at seq + 1, produced before the two patches below.
  let answer;
  answers.get_state = new Promise(resolve => { answer = resolve; });
  const stale = { ...structuredClone(ipc.core.state), seq: seq + 1 };
  ipc.emit("view-state", { seq: seq + 2, base: seq + 1, remux: outcomePatch("cancelled", "Cancelled") });
  // Arrives while the read is in flight, so it is held too.
  ipc.emit("view-state", { seq: seq + 3, base: seq + 2, remux: outcomePatch("done", "Done") });
  answer(stale);
  await ipc.flush();
  assert.equal(reads(), before + 1);
  // Both held patches merged onto the answer, the later one last.
  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Done/);
});

test("a held payload that throws while it is applied is logged, and the next patch still resyncs", async t => {
  const answers = {};
  const ipc = bootstrapHarness(t, answers);
  await ipc.mount("patch-held-throws");
  ipc.resolveState();
  await ipc.flush();
  const errors = t.mock.method(console, "error", () => {});
  const reads = () => ipc.calls.filter(([command]) => command === "get_state").length;
  const before = reads();
  const seq = ipc.core.state.seq;
  answers.get_state = { ...structuredClone(ipc.core.state), seq: seq + 1 };
  // `row_changes` that is not a list makes the merge throw.
  ipc.emit("view-state", { seq: seq + 2, base: seq + 1, remux: { row_changes: 5 } });
  await ipc.flush();
  assert.equal(reads(), before + 1);
  assert.match(String(errors.mock.calls.at(-1)?.arguments[0]), /held payload failed/);

  // The resync is not wedged: the next patch the page cannot apply reads get_state again.
  answers.get_state = { ...structuredClone(ipc.core.state), seq: seq + 4, remux: { ...ipc.core.state.remux, ...outcomePatch("done", "Done") } };
  ipc.emit("view-state", { seq: seq + 3, base: seq + 2 });
  await ipc.flush();
  assert.equal(reads(), before + 2);
  assert.match(ipc.elements.get("remux-status").lastChild.textContent, /Done/);
});

test("a patch that arrives before the boot's get_state answers waits for it, with no second get_state", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("patch-early");
  activeJob(ipc.core);
  ipc.core.state.remux.rows[1].progress = 85;
  ipc.core.publishPatch({ remux: { row_changes: [[1, ipc.core.state.remux.rows[1]]] } });
  ipc.resolveState();
  await ipc.flush();
  assert.equal(ipc.calls.filter(([command]) => command === "get_state").length, 1);
  const list = ipc.elements.get("remux-episode-list");
  assert.equal(list.children[1].querySelector(".ep-ring-arc").style.strokeDashoffset, (RING_CIRCUMFERENCE * 0.15).toFixed(2));
  settleJob(ipc.core, "completed");
});

test("a patch that arrives while boot binds its listeners and resyncs before it reads get_state issues no second get_state", async t => {
  let ipc;
  ipc = bootstrapHarness(t, {}, {
    async onListen(name) {
      if (name === "view-state") {
        activeJob(ipc.core);
        ipc.core.publishPatch({ remux: { outcome: null } });
      } else if (name === "job-log") {
        // The resync the patch started answers before the job-log listener is bound.
        ipc.resolveState();
        await ipc.flush();
      }
    },
  });
  await ipc.mount("patch-listen");
  // Boot goes on once the job-log listener settles.
  // Bounded, so a boot that never reaches get_log fails instead of hanging the suite.
  for (let i = 0; i < 50 && !ipc.calls.some(([command]) => command === "get_log"); i++) await ipc.flush();
  assert.ok(ipc.calls.some(([command]) => command === "get_log"), "boot reads the Run Logs");
  assert.equal(ipc.calls.filter(([command]) => command === "get_state").length, 1);
  assert.equal(ipc.elements.get("remux-episode-list").children.length, 2);
  settleJob(ipc.core, "completed");
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
  settleJob(ipc.core, "completed");
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
  await ipc.flush();
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

test("a draft Intent takes its state from the publication, with no get_state after its answer", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("draft-no-sync");
  ipc.resolveState();
  await ipc.flush();
  const since = ipc.calls.length;
  const commands = () => ipc.calls.slice(since).map(([command]) => command);
  const workers = ipc.elements.get("remux-workers");
  workers.value = "4";
  workers.handlers.get("input")();
  await ipc.flush();
  assert.deepEqual(commands(), ["dispatch"]);
  assert.equal(ipc.elements.get("remux-workers").value, "4");
  assert.equal(ipc.core.state.remux.draft.workers, "4");

  // Any other Intent, here the scan the input folder's edit sends, still reads the state after.
  await ipc.scan("remux", "/anime");
  await ipc.flush();
  assert.ok(commands().slice(1).includes("get_state"), commands().join(", "));
});

test("a picked subtitle goes to the core as assign_subtitle with the inspector's fields as typed", async t => {
  const ipc = bootstrapHarness(t, { pick_subtitle_file: "/custom/sub.ass" });
  const assignSelect = new BootElement();
  const fields = {
    "sub-lang-select": new BootElement("custom"),
    "sub-lang-custom": new BootElement(" may "),
    "sub-title-input": new BootElement(" Signs "),
    "sub-offset-input": new BootElement(" -250.9 "),
  };
  ipc.elements.set("sub-assign-select", assignSelect);
  for (const [id, el] of Object.entries(fields)) ipc.elements.set(id, el);

  await ipc.mount("sub-assign-raw");
  assert.equal(await ipc.scan("remux", "/release"), true);
  ipc.resolveState();
  await ipc.flush();

  // Pick the row into the inspector, then choose a subtitle file from its assign select.
  ipc.elements.get("remux-episode-list").handlers.get("click")({
    target: { closest: selector => (selector === ".ep-pick" ? { dataset: { itemId: "one" } } : null) },
  });
  assignSelect.value = "__browse__";
  await assignSelect.handlers.get("change")({ target: assignSelect });
  await ipc.flush();

  // The core trims and parses them (`inspector::subtitle_assignment`); the view sends them raw.
  assert.deepEqual(ipc.core.intents.find(intent => intent.type === "assign_subtitle"), {
    type: "assign_subtitle",
    mode: "remux",
    id: "one",
    path: "/custom/sub.ass",
    language: "custom",
    custom_language: " may ",
    title: " Signs ",
    offset: " -250.9 ",
  });
});

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

  // The view sends the tag as typed; the core trims it.
  langCustom.value = "  may  ";
  langCustom.handlers.get("change")();
  await ipc.flush();
  assert.equal(ipc.core.intents.at(-1).custom_language, "  may  ");
  assert.equal(ipc.core.state.remux.assignments.one.language, "may");
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
  assert.deepEqual(ipc.core.intents.at(-1), { type: "clear_track", mode: "remux", id: "one" });
  assert.deepEqual(ipc.core.state.remux.assignments, {});
  await ipc.elements.get("remux-run").handlers.get("click")();
  await ipc.flush();
  assert.equal(ipc.core.state.job?.active, true);

  // The job now runs: a pick is refused before the file dialog opens.
  const picks = ipc.calls.filter(([command]) => command === "pick_subtitle_file").length;
  await assignSelect.handlers.get("change")({ target: assignSelect });
  assert.equal(ipc.calls.filter(([command]) => command === "pick_subtitle_file").length, picks);
});

test("a Run the core refuses is logged as ERROR in its mode's console and flags the field it names", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("run-refused");
  ipc.resolveState();
  await ipc.flush();
  await ipc.elements.get("remux-run").handlers.get("click")();
  await ipc.flush();
  assert.deepEqual(ipc.core.intents.at(-1), { type: "run", mode: "remux" });
  assert.deepEqual(ipc.elements.get("remux-log").children.map(line => line.textContent), ["ERROR: Input directory is required."]);
  assert.equal(ipc.elements.get("remux-input-dir").classList.contains("input-error"), true);
  assert.equal(ipc.core.state.run.phase, "idle");
});

test("while a job runs, a refused draft edit puts the published value back into its field", async t => {
  t.mock.method(console, "error", () => {});
  const ipc = bootstrapHarness(t);
  await ipc.mount("draft-refused");
  activeJob(ipc.core);
  ipc.core.state.remux.draft = draft("remux", { workers: "3" });
  ipc.resolveState();
  await ipc.flush();
  const workers = ipc.elements.get("remux-workers");
  assert.equal(workers.value, "3");
  workers.value = "9";
  workers.handlers.get("input")();
  await ipc.flush();
  assert.equal(workers.value, "3");
  assert.equal(ipc.core.state.remux.draft.workers, "3");
  settleJob(ipc.core, "completed");
});

test("the console draws the core's batches: the display text, the class from the kind, the badge from errors, Copy from the raw text", async t => {
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

  ipc.core.log("remux", [
    {
      kind: "succeeded",
      text: "  Completed -> /media/anime/Show/Season 1/Output/Show - 01.mkv",
      display: "  Completed -> media › anime › Show › Season 1 › Output › Show - 01.mkv",
    },
    { kind: "error", text: "ERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv" },
  ]);
  const log = ipc.elements.get("remux-log");
  assert.deepEqual(log.children.map(line => line.textContent), [
    "  Completed -> media › anime › Show › Season 1 › Output › Show - 01.mkv",
    "ERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv",
  ]);
  assert.deepEqual(log.children.map(line => line.className), ["log-line line-ok", "log-line line-err"]);
  assert.equal(badge.hidden, false);
  assert.equal(badge.textContent, "1");

  await copyBtn.handlers.get("click")();
  // Copy is each line's text, as the CLI prints it: an error line gets no added prefix.
  assert.deepEqual(copied, [
    "  Completed -> /media/anime/Show/Season 1/Output/Show - 01.mkv\nERROR: Pipeline failed: mkvmerge failed on Show - 02.mkv",
  ]);
});

test("a reload draws the Run Log get_log holds, and a batch that arrived before it only once", async t => {
  const ipc = bootstrapHarness(t);
  ipc.core.log("extract", [{ kind: "note", text: "probing" }]);
  await ipc.mount("log-reload");
  // A batch sent after the listener is bound but before get_log is part of what get_log answers.
  ipc.core.log("extract", [{ kind: "error", text: "ERROR: x" }]);
  ipc.resolveState();
  await ipc.flush();
  const log = ipc.elements.get("extract-log");
  assert.deepEqual(log.children.map(line => line.textContent), ["probing", "ERROR: x"]);
  assert.deepEqual(ipc.elements.get("remux-log").children.map(line => line.textContent), [
    "No output yet. Run a batch to see its output here.",
  ]);
});

test("the window's visibility goes to the core as view_visible, with no get_state after it", async t => {
  const ipc = bootstrapHarness(t);
  await ipc.mount("visibility");
  ipc.resolveState();
  await ipc.flush();
  const since = ipc.calls.length;
  globalThis.document.hidden = false;
  ipc.documentHandlers.get("visibilitychange")();
  await ipc.flush();
  assert.equal(ipc.core.visible, true);
  assert.deepEqual(ipc.calls.slice(since), [["dispatch", { intent: { type: "view_visible", visible: true } }]]);
});

test("the telemetry shows the published texts and draws the published samples", async t => {
  const ipc = bootstrapHarness(t);
  for (const id of ["tel-cpu", "tel-ram", "tel-disk", "tel-cpu-line", "tel-ram-line", "tel-disk-line"]) ipc.elements.set(id, new BootElement());
  const points = new Map();
  for (const id of ["tel-cpu-line", "tel-ram-line", "tel-disk-line"]) ipc.elements.get(id).setAttribute = (_name, value) => points.set(id, value);
  await ipc.mount("telemetry");
  ipc.core.state.telemetry = { cpu: "37%", ram: "5.12 GB", disk: "Idle", samples: { cpu: [0, 100], ram: [50, 50], disk: [0, 25] } };
  ipc.resolveState();
  await ipc.flush();
  assert.deepEqual(["tel-cpu", "tel-ram", "tel-disk"].map(id => ipc.elements.get(id).textContent), ["37%", "5.12 GB", "Idle"]);
  assert.equal(points.get("tel-cpu-line"), "0.0,18.0 64.0,2.0");
  // The disk line scales to at least 50 MB/s.
  assert.equal(points.get("tel-disk-line"), "0.0,18.0 64.0,10.0");
});

test("each filter chip shows its count from the published filters", async t => {
  const ipc = bootstrapHarness(t);
  const chips = ["all", "todo", "done", "failed"].map(filter => Object.assign(new BootElement(), { dataset: { mode: "remux", filter } }));
  const querySelectorAll = globalThis.document.querySelectorAll;
  globalThis.document.querySelectorAll = selector => (selector === '.filter-chip[data-mode="remux"]' ? chips : querySelectorAll(selector));
  await ipc.mount("filter-chips");
  ipc.resolveState();
  await ipc.flush();

  // The fake lays the counts over the rows' buckets, as `presentation::filter_counts` does.
  ipc.core.accept("remux", "/anime", [
    row("a", { status: "completed", bucket: "done" }),
    row("b", { status: "error", bucket: "failed" }),
    row("c"),
    row("d", { selected: false, bucket: null }),
  ]);
  await ipc.flush();
  assert.deepEqual(chips.map(chip => chip.querySelector(".n").textContent), ["4", "1", "1", "1"]);
});

// Rendering only: every text below is the core's (`presentation.rs`, `inspector.rs`).
test("the list header, the bottom bar, the Library and the inspector draw the published ModeView", async t => {
  const ipc = bootstrapHarness(t);
  const ids = [
    "remux-title", "remux-path", "remux-clock", "library-tree", "insp-body", "insp-empty", "insp-num", "insp-title",
    "insp-path", "insp-error", "insp-status", "insp-video", "insp-audio-label", "insp-audio", "insp-out",
    "insp-fonts-head", "insp-fonts", "insp-ladder", "sub-assign-select", "sub-assign-reset",
  ];
  for (const id of ids) ipc.elements.set(id, new BootElement());
  ipc.elements.set("remux-out-dir", Object.assign(new BootElement(), { placeholder: "Output folder next to the input" }));
  const stack = new BootElement();
  for (let i = 0; i < 4; i++) stack.appendChild(new BootElement());
  ipc.elements.set("remux-stack", stack);
  await ipc.mount("chrome");

  const one = row("Season 1/one.mkv", { name: "one.mkv", release: "Season 1", status: "error", tone: "err", label: "Failed", bucket: "failed" });
  ipc.core.state.remux = modeView([one], {
    directory: "/anime/show",
    manifest_rev: 1,
    title: "show",
    crumbs: ["anime", "show"],
    clock: "5s elapsed",
    progress: { done: 50, skipped: 0, failed: 25, live: 12.5 },
    library: [
      { release: null, label: "show", tooltip: "anime › show", count: 1, group_label: "anime › show" },
      { release: "Season 1", label: "Season 1", tooltip: "Season 1", count: 1, group_label: "Season 1" },
    ],
    inspector: inspectorView(one, 0, {
      crumbs: ["Season 1"],
      error: "mkvmerge exited with 2",
      pill: { tone: "err", label: "Failed" },
      video: "1080p HEVC · copied",
      audio: "FLAC 2.0 → Opus 192k",
      fonts_head: "Fonts · 3",
      fonts: "3 attached fonts · folder: auto",
      ladder: [
        { key: "manual", label: "Manual assignment", detail: "", chosen: false, found: false, tag: "None" },
        { key: "embedded", label: "Embedded track", detail: "ASS (ind)", chosen: true, found: true, tag: "Auto" },
        { key: "assets", label: "Assets directory", detail: "subs", chosen: false, found: true, tag: "Fallback" },
      ],
      options: [
        { value: "", label: "Automatic (follow the ladder)" },
        { value: "Subs/01.ass", label: "Subs › 01.ass (Parent folder)" },
      ],
    }),
  });
  ipc.resolveState();
  await ipc.flush();

  const text = id => ipc.elements.get(id).textContent;
  assert.equal(text("remux-title"), "show");
  assert.deepEqual(ipc.elements.get("remux-path").children.map(el => el.textContent), ["anime", "›", "show"]);
  assert.equal(text("remux-clock"), "5s elapsed");
  assert.deepEqual(stack.children.map(seg => seg.style.width), ["50.00%", "0.00%", "25.00%", "12.50%"]);

  const library = ipc.elements.get("library-tree").children;
  assert.deepEqual(library.map(btn => [btn.className, btn.dataset.release, btn.title]), [
    ["nav lib-root", undefined, "anime › show"],
    ["nav lib-release", "Season 1", "Season 1"],
  ]);

  assert.equal(ipc.elements.get("insp-body").hidden, false);
  assert.equal(ipc.elements.get("insp-empty").hidden, true);
  assert.equal(text("insp-num"), "01");
  assert.equal(text("insp-title"), "one.mkv");
  assert.deepEqual(ipc.elements.get("insp-path").children.map(el => el.textContent), ["Season 1"]);
  assert.equal(ipc.elements.get("insp-error").hidden, false);
  assert.equal(text("insp-error"), "mkvmerge exited with 2");
  assert.equal(ipc.elements.get("insp-status").className, "pill tone-err");
  assert.equal(text("insp-status"), "Failed");
  assert.equal(text("insp-video"), "1080p HEVC · copied");
  assert.equal(text("insp-audio-label"), "Audio");
  assert.equal(text("insp-audio"), "FLAC 2.0 → Opus 192k");
  // No output folder in the draft: the field's own description of its default.
  assert.equal(text("insp-out"), "Output folder next to the input");
  assert.equal(text("insp-fonts-head"), "Fonts · 3");
  assert.equal(text("insp-fonts"), "3 attached fonts · folder: auto");
  const rungs = ipc.elements.get("insp-ladder").children;
  assert.deepEqual(rungs.map(li => li.className), ["", "is-chosen", "is-found"]);
  assert.deepEqual(rungs.map(li => li.children[0].textContent), ["1", "2", "3"]);
  const select = ipc.elements.get("sub-assign-select");
  assert.deepEqual(select.children.map(opt => [opt.value, opt.textContent]), [
    ["", "Automatic (follow the ladder)"],
    ["Subs/01.ass", "Subs › 01.ass (Parent folder)"],
    ["__browse__", "Choose a file…"],
  ]);
  assert.equal(select.value, "");
  assert.equal(ipc.elements.get("sub-assign-reset").hidden, true);

  // A queue with no rows publishes no inspector: the empty state shows.
  ipc.core.state.remux = modeView([], { directory: "/anime/show", manifest_rev: 2 });
  ipc.core.publish();
  assert.equal(ipc.elements.get("insp-body").hidden, true);
  assert.equal(ipc.elements.get("insp-empty").hidden, false);
});

test("a row click sends inspect, and the inspector follows the pick the core publishes", async t => {
  const ipc = bootstrapHarness(t);
  ipc.elements.set("insp-body", new BootElement());
  ipc.elements.set("insp-title", new BootElement());
  await ipc.mount("inspect");
  assert.equal(await ipc.scan("remux", "/release"), true);
  ipc.resolveState();
  await ipc.flush();
  assert.equal(ipc.elements.get("insp-title").textContent, "one.mkv");

  ipc.elements.get("remux-episode-list").handlers.get("click")({
    target: { closest: selector => (selector === ".ep-pick" ? { dataset: { itemId: "two" } } : null) },
  });
  await ipc.flush();
  assert.deepEqual(ipc.core.intents.at(-1), { type: "inspect", mode: "remux", id: "two" });
  assert.equal(ipc.elements.get("insp-title").textContent, "two.mkv");
});

test("a stepper click sends step_draft with the field its input names, and the published draft fills the input", async t => {
  const ipc = bootstrapHarness(t);
  const offset = new BootElement("0");
  offset.id = "remux-sub-offset";
  const plus = new BootElement();
  plus.classList.add("plus");
  plus.parentElement = { querySelector: () => offset };
  ipc.elements.set("remux-sub-offset", offset);
  const querySelectorAll = globalThis.document.querySelectorAll;
  globalThis.document.querySelectorAll = selector => (selector === ".stepper-btn" ? [plus] : querySelectorAll(selector));
  await ipc.mount("stepper");
  ipc.resolveState();
  await ipc.flush();

  const since = ipc.calls.length;
  const intentsSince = ipc.core.intents.length;
  plus.handlers.get("mousedown")({ button: 0 });
  plus.handlers.get("mouseup")();
  // The click a mouse press ends with has `detail` >= 1 (2 on a double click) and must not step again.
  plus.handlers.get("click")({ detail: 1 });
  plus.handlers.get("click")({ detail: 2 });
  // A right-button press does not step.
  plus.handlers.get("mousedown")({ button: 2 });
  plus.handlers.get("mouseup")();
  await ipc.flush();
  assert.deepEqual(ipc.core.intents.slice(intentsSince), [
    { type: "step_draft", mode: "remux", field: "sub_offset", direction: "up" },
  ]);
  // A draft Intent: its state comes from the publication, with no get_state after it.
  assert.deepEqual(ipc.calls.slice(since).map(([command]) => command), ["dispatch"]);
  assert.equal(offset.value, "0.5");
});

test("a keyboard-activated stepper click sends one step_draft", async t => {
  const ipc = bootstrapHarness(t);
  const workers = new BootElement("");
  workers.id = "extract-workers";
  const minus = new BootElement();
  minus.classList.add("minus");
  minus.parentElement = { querySelector: () => workers };
  ipc.elements.set("extract-workers", workers);
  const querySelectorAll = globalThis.document.querySelectorAll;
  globalThis.document.querySelectorAll = selector => (selector === ".stepper-btn" ? [minus] : querySelectorAll(selector));
  await ipc.mount("stepper-keyboard");
  ipc.resolveState();
  await ipc.flush();

  const since = ipc.core.intents.length;
  // Enter or Space fires only a click, with `detail` 0.
  minus.handlers.get("click")({ detail: 0 });
  await ipc.flush();
  assert.deepEqual(ipc.core.intents.slice(since), [{ type: "step_draft", mode: "extract", field: "workers", direction: "down" }]);
});
