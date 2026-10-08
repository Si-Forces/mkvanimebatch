import assert from "node:assert/strict";
import test from "node:test";
import { createSettingsChips } from "./settings-chips.js";

/** The Effective Settings `get_default_settings` answers in these tests. */
const DEFAULTS = { workers: 6, io_jobs: 4, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };
/** What a running job runs with: nothing like `DEFAULTS`. */
const JOB_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

const flush = () => new Promise(resolve => setImmediate(resolve));

/** A promise the test settles by hand. */
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

/**
 * A page with the workers and Disk I/O inputs of both views and one chip of each kind. `answer`
 * is what `get_default_settings` returns (a promise or an `Error` instance, which is thrown).
 * `jobActive` is the flag `isJobActive()` reports; a test flips it by assigning `state.jobActive`.
 * `commands` lists every command invoked: `loadDefaults` logs a failed call rather than throwing, so
 * a wrong command shows up here, not as a failed assertion inside `invoke`.
 */
function chipsHarness(answer) {
  const state = { jobActive: false };
  const commands = [];
  const inputs = new Map();
  for (const mode of ["remux", "extract"]) {
    for (const id of ["workers", "io-jobs"]) inputs.set(`${mode}-${id}`, { dataset: {} });
  }
  const workerChip = {};
  const ioChip = {};
  const doc = {
    getElementById: id => inputs.get(id) ?? null,
    querySelectorAll: selector => {
      if (selector === ".metric-workers-chip") return [workerChip];
      if (selector === ".metric-iojobs-chip") return [ioChip];
      return [];
    },
  };
  const invoke = async command => {
    commands.push(command);
    const result = await answer;
    if (result instanceof Error) throw result;
    return result;
  };
  const chips = createSettingsChips({ invoke, doc, isJobActive: () => state.jobActive });
  return { chips, state, inputs, workerChip, ioChip, commands };
}

test("loadDefaults puts auto (N) on the workers and Disk I/O inputs and the defaults on the idle chips", async () => {
  const page = chipsHarness(DEFAULTS);
  await page.chips.loadDefaults();
  assert.deepEqual(page.commands, ["get_default_settings"]);
  for (const mode of ["remux", "extract"]) {
    const workers = page.inputs.get(`${mode}-workers`);
    assert.equal(workers.placeholder, "auto (6)");
    assert.equal(workers.dataset.autoValue, "6");
    const ioJobs = page.inputs.get(`${mode}-io-jobs`);
    assert.equal(ioJobs.placeholder, "auto (4)");
    assert.equal(ioJobs.dataset.autoValue, "4");
  }
  assert.equal(page.workerChip.innerText, "⚡ Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "🔀 Idle (4 IO Jobs)");
});

test("a failed get_default_settings leaves the inputs on plain auto and does not write the chips", async t => {
  const logged = t.mock.method(console, "error", () => {});
  const page = chipsHarness(new Error("unavailable"));
  await page.chips.loadDefaults();
  assert.deepEqual(page.commands, ["get_default_settings"]);
  for (const mode of ["remux", "extract"]) {
    const input = page.inputs.get(`${mode}-workers`);
    assert.equal(input.placeholder, undefined);
    assert.equal("autoValue" in input.dataset, false);
  }
  assert.equal(page.workerChip.innerText, undefined);
  assert.equal(page.ioChip.innerText, undefined);
  assert.equal(logged.mock.calls.some(({ arguments: [label] }) => label === "[get_default_settings] failed:"), true);
});

for (const [label, answer] of [
  ["no answer", null],
  ["no workers count", { io_jobs: 4 }],
  ["zero workers", { ...DEFAULTS, workers: 0 }],
  ["fractional workers", { ...DEFAULTS, workers: 2.5 }],
  ["workers as a string", { ...DEFAULTS, workers: "6" }],
]) {
  test(`get_default_settings with ${label} is refused: nothing is written and the failure is logged`, async t => {
    const logged = t.mock.method(console, "error", () => {});
    const page = chipsHarness(answer);
    await page.chips.loadDefaults();
    assert.equal(page.inputs.get("remux-workers").placeholder, undefined);
    assert.equal("autoValue" in page.inputs.get("remux-io-jobs").dataset, false);
    assert.equal(page.workerChip.innerText, undefined);
    // Logged by the validation, not by a later TypeError on the missing answer.
    assert.equal(logged.mock.calls.length, 1);
    assert.match(logged.mock.calls[0].arguments[1].message, /^unexpected value /);

    // The refused answer must not have become the defaults.
    page.chips.update(false);
    assert.equal(page.workerChip.innerText, "⚡ Standby");
    assert.equal(page.ioChip.innerText, "🔀 Idle");
  });
}

test("update(true, settings) shows the job's own Effective Settings, not the defaults", async () => {
  const page = chipsHarness(DEFAULTS);
  await page.chips.loadDefaults();
  page.chips.update(true, JOB_SETTINGS);
  assert.equal(page.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(page.ioChip.innerText, "🔀 5 IO Jobs Active");
});

test("with get_default_settings failed, update(true, settings) still shows the job's own settings", async t => {
  t.mock.method(console, "error", () => {});
  const page = chipsHarness(new Error("unavailable"));
  await page.chips.loadDefaults();
  page.chips.update(true, JOB_SETTINGS);
  assert.equal(page.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(page.ioChip.innerText, "🔀 5 IO Jobs Active");
});

test("with no settings known anywhere, the chips name their state and invent no counts", async t => {
  t.mock.method(console, "error", () => {});
  const page = chipsHarness(new Error("unavailable"));
  await page.chips.loadDefaults();
  // A job no `run_job` began, so it carries no Effective Settings: `null`, not the defaults.
  page.chips.update(true, null);
  assert.equal(page.workerChip.innerText, "⚡ Workers Active");
  assert.equal(page.ioChip.innerText, "🔀 IO Jobs Active");

  page.chips.update(false);
  assert.equal(page.workerChip.innerText, "⚡ Standby");
  assert.equal(page.ioChip.innerText, "🔀 Idle");
});

test("null settings stay unknown even when the defaults are known", async () => {
  const page = chipsHarness(DEFAULTS);
  await page.chips.loadDefaults();
  page.chips.update(true, null);
  assert.equal(page.workerChip.innerText, "⚡ Workers Active");
  assert.equal(page.ioChip.innerText, "🔀 IO Jobs Active");
});

test("defaults that arrive while a job is active leave its chips alone, and idle chips return the defaults", async () => {
  const reply = deferred();
  const page = chipsHarness(reply.promise);
  const loaded = page.chips.loadDefaults();
  page.state.jobActive = true;
  page.chips.update(true, JOB_SETTINGS);

  reply.resolve(DEFAULTS);
  await loaded;
  await flush();
  assert.equal(page.workerChip.innerText, "⚡ 3 Workers Active");
  assert.equal(page.ioChip.innerText, "🔀 5 IO Jobs Active");
  // The inputs still learn the defaults; only the chips belong to the job.
  assert.equal(page.inputs.get("remux-workers").placeholder, "auto (6)");

  page.state.jobActive = false;
  page.chips.update(false);
  assert.equal(page.workerChip.innerText, "⚡ Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "🔀 Idle (4 IO Jobs)");
});

test("loadDefaults skips an auto input the page does not have", async () => {
  const page = chipsHarness(DEFAULTS);
  page.inputs.delete("extract-io-jobs");
  await page.chips.loadDefaults();
  assert.equal(page.inputs.get("remux-io-jobs").placeholder, "auto (4)");
  assert.equal(page.inputs.get("extract-workers").placeholder, "auto (6)");
  assert.equal(page.workerChip.innerText, "⚡ Standby (6 Workers)");
});
