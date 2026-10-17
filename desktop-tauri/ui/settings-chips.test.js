// Rendering only: which texts the chips and hints show is the Desktop Core's, tested in
// `crates/mkv-desktop-core/src/presentation.rs` (`chips`).
import assert from "node:assert/strict";
import test from "node:test";
import { createSettingsChips } from "./settings-chips.js";

/** Published chips with idle defaults of 6 workers and 4 IO jobs, with `fields` over them. */
const chips = (fields = {}) => ({
  workers: "Standby (6 Workers)",
  io_jobs: "Idle (4 IO Jobs)",
  workers_hint: "auto (6)",
  io_jobs_hint: "auto (4)",
  ...fields,
});

/** A page with the workers and Disk I/O inputs of both views and one chip of each kind. */
function chipsHarness() {
  const inputs = new Map();
  for (const mode of ["remux", "extract"]) {
    for (const id of ["workers", "io-jobs"]) inputs.set(`${mode}-${id}`, {});
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
  return { chips: createSettingsChips({ doc }), inputs, workerChip, ioChip };
}

test("the published chips fill the bottom bar and their hints the workers and Disk I/O inputs of both views", () => {
  const page = chipsHarness();
  page.chips.render({ chips: chips() });
  for (const mode of ["remux", "extract"]) {
    assert.equal(page.inputs.get(`${mode}-workers`).placeholder, "auto (6)");
    assert.equal(page.inputs.get(`${mode}-io-jobs`).placeholder, "auto (4)");
  }
  assert.equal(page.workerChip.innerText, "Standby (6 Workers)");
  assert.equal(page.ioChip.innerText, "Idle (4 IO Jobs)");

  page.chips.render({ chips: chips({ workers: "3 Workers Active", io_jobs: "5 IO Jobs Active" }) });
  assert.equal(page.workerChip.innerText, "3 Workers Active");
  assert.equal(page.ioChip.innerText, "5 IO Jobs Active");
});

test("a null hint leaves the input on its own placeholder, and a View State without chips renders nothing", () => {
  const page = chipsHarness();
  page.chips.render(null);
  assert.equal(page.workerChip.innerText, undefined);
  page.chips.render({ chips: chips({ workers: "Standby", io_jobs: "Idle", workers_hint: null, io_jobs_hint: null }) });
  assert.equal(page.inputs.get("remux-workers").placeholder, undefined);
  assert.equal(page.workerChip.innerText, "Standby");
});

test("the same hints published again rewrite nothing, and an input the page does not have is skipped", () => {
  const page = chipsHarness();
  page.inputs.delete("extract-io-jobs");
  page.chips.render({ chips: chips() });
  assert.equal(page.inputs.get("extract-workers").placeholder, "auto (6)");
  page.inputs.get("remux-workers").placeholder = "edited";
  page.chips.render({ chips: chips({ workers: "Workers Active" }) });
  assert.equal(page.inputs.get("remux-workers").placeholder, "edited");
});
