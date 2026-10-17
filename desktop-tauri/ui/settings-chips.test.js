import assert from "node:assert/strict";
import test from "node:test";
import { createSettingsChips } from "./settings-chips.js";
import { IDLE_RUN, jobView } from "./fake-core.js";

/** The View State's `defaults` in these tests. */
const DEFAULTS = { workers: 6, io_jobs: 4, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };
/** What a start resolved its request to: nothing like `DEFAULTS`. */
const RUN_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

/** The View State fields the chips read, idle with `DEFAULTS` unless `fields` say otherwise. */
const state = (fields = {}) => ({ job: null, run: { ...IDLE_RUN }, busy: false, defaults: DEFAULTS, ...fields });

/** A page with the workers and Disk I/O inputs of both views and one chip of each kind. */
function chipsHarness() {
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
  const chips = createSettingsChips({ doc });
  return { chips, inputs, workerChip, ioChip };
}

test("published defaults put auto (N) on the workers and Disk I/O inputs and the defaults on the idle chips", () => {
  const page = chipsHarness();
  page.chips.render(state());
  for (const mode of ["remux", "extract"]) {
    const workers = page.inputs.get(`${mode}-workers`);
    assert.equal(workers.placeholder, "auto (6)");
    assert.equal(workers.dataset.autoValue, "6");
    const ioJobs = page.inputs.get(`${mode}-io-jobs`);
    assert.equal(ioJobs.placeholder, "auto (4)");
    assert.equal(ioJobs.dataset.autoValue, "4");
  }
  assert.equal(page.workerChip.innerText, "Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "Idle (4 IO Jobs)");
});

test("a View State without defaults yet leaves the inputs on plain auto and the chips without counts", () => {
  const page = chipsHarness();
  page.chips.render(state({ defaults: null }));
  for (const mode of ["remux", "extract"]) {
    const input = page.inputs.get(`${mode}-workers`);
    assert.equal(input.placeholder, undefined);
    assert.equal("autoValue" in input.dataset, false);
  }
  assert.equal(page.workerChip.innerText, "Standby");
  assert.equal(page.ioChip.innerText, "Idle");
});

test("a start shows its resolved run settings, and none before its request resolved", () => {
  const page = chipsHarness();
  page.chips.render(state({ run: { phase: "starting", mode: "remux", settings: null }, busy: true }));
  assert.equal(page.workerChip.innerText, "Workers Active");
  assert.equal(page.ioChip.innerText, "IO Jobs Active");
  page.chips.render(state({ run: { phase: "starting", mode: "remux", settings: RUN_SETTINGS }, busy: true }));
  assert.equal(page.workerChip.innerText, "3 Workers Active");
  assert.equal(page.ioChip.innerText, "5 IO Jobs Active");
  // The inputs still learn the defaults; only the chips belong to the batch.
  assert.equal(page.inputs.get("remux-workers").placeholder, "auto (6)");
});

test("an active job with no run settings shows the Job View's own Effective Settings", () => {
  const page = chipsHarness();
  page.chips.render(state({ job: jobView(), busy: true }));
  assert.equal(page.workerChip.innerText, "3 Workers Active");
  // A job that ended no longer owns the chips: they return to the defaults.
  page.chips.render(state({ job: jobView([], { active: false, status: "completed" }) }));
  assert.equal(page.workerChip.innerText, "Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "Idle (4 IO Jobs)");
});

test("the same defaults published again rewrite nothing, and an input the page does not have is skipped", () => {
  const page = chipsHarness();
  page.inputs.delete("extract-io-jobs");
  page.chips.render(state());
  assert.equal(page.inputs.get("remux-io-jobs").placeholder, "auto (4)");
  assert.equal(page.inputs.get("extract-workers").placeholder, "auto (6)");
  page.inputs.get("remux-workers").placeholder = "edited";
  page.chips.render(state({ defaults: { ...DEFAULTS } }));
  assert.equal(page.inputs.get("remux-workers").placeholder, "edited");
});
