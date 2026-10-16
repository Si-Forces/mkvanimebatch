import assert from "node:assert/strict";
import test from "node:test";
import { createSettingsChips } from "./settings-chips.js";

/** The View State's `defaults` in these tests. */
const DEFAULTS = { workers: 6, io_jobs: 4, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };
/** What a running job runs with: nothing like `DEFAULTS`. */
const JOB_SETTINGS = { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 };

/**
 * A page with the workers and Disk I/O inputs of both views and one chip of each kind.
 * `jobActive` is the flag `isJobActive()` reports; a test flips it by assigning `state.jobActive`.
 */
function chipsHarness() {
  const state = { jobActive: false };
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
  const chips = createSettingsChips({ doc, isJobActive: () => state.jobActive });
  return { chips, state, inputs, workerChip, ioChip };
}

test("published defaults put auto (N) on the workers and Disk I/O inputs and the defaults on the idle chips", () => {
  const page = chipsHarness();
  page.chips.applyDefaults(DEFAULTS);
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

test("a View State without defaults yet leaves the inputs on plain auto and does not write the chips", () => {
  const page = chipsHarness();
  page.chips.applyDefaults(null);
  for (const mode of ["remux", "extract"]) {
    const input = page.inputs.get(`${mode}-workers`);
    assert.equal(input.placeholder, undefined);
    assert.equal("autoValue" in input.dataset, false);
  }
  assert.equal(page.workerChip.innerText, undefined);
  page.chips.update(false);
  assert.equal(page.workerChip.innerText, "Standby");
  assert.equal(page.ioChip.innerText, "Idle");
});

test("update(true, settings) shows the job's own Effective Settings, not the defaults", () => {
  const page = chipsHarness();
  page.chips.applyDefaults(DEFAULTS);
  page.chips.update(true, JOB_SETTINGS);
  assert.equal(page.workerChip.innerText, "3 Workers Active");
  assert.equal(page.ioChip.innerText, "5 IO Jobs Active");
});

test("null settings stay unknown, with or without the defaults", () => {
  const page = chipsHarness();
  // A job no `run_job` began, so it carries no Effective Settings: `null`, not the defaults.
  page.chips.update(true, null);
  assert.equal(page.workerChip.innerText, "Workers Active");
  assert.equal(page.ioChip.innerText, "IO Jobs Active");
  page.chips.applyDefaults(DEFAULTS);
  page.chips.update(true, null);
  assert.equal(page.workerChip.innerText, "Workers Active");
});

test("defaults published while a job is active leave its chips alone, and idle chips return the defaults", () => {
  const page = chipsHarness();
  page.state.jobActive = true;
  page.chips.update(true, JOB_SETTINGS);
  page.chips.applyDefaults(DEFAULTS);
  assert.equal(page.workerChip.innerText, "3 Workers Active");
  assert.equal(page.ioChip.innerText, "5 IO Jobs Active");
  // The inputs still learn the defaults; only the chips belong to the job.
  assert.equal(page.inputs.get("remux-workers").placeholder, "auto (6)");

  page.state.jobActive = false;
  page.chips.update(false);
  assert.equal(page.workerChip.innerText, "Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "Idle (4 IO Jobs)");
});

test("the same defaults published again rewrite nothing, and an input the page does not have is skipped", () => {
  const page = chipsHarness();
  page.inputs.delete("extract-io-jobs");
  page.chips.applyDefaults(DEFAULTS);
  assert.equal(page.inputs.get("remux-io-jobs").placeholder, "auto (4)");
  assert.equal(page.inputs.get("extract-workers").placeholder, "auto (6)");
  page.chips.update(true, JOB_SETTINGS);
  // Every publication carries the defaults; one that did not change them leaves the job's chips.
  page.chips.applyDefaults({ ...DEFAULTS });
  assert.equal(page.workerChip.innerText, "3 Workers Active");
});
