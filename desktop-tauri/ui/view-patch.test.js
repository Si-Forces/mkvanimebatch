import assert from "node:assert/strict";
import test from "node:test";
import { ALL_ROWS, nextState } from "./view-patch.js";
import { IDLE_RUN, jobView, modeView, row } from "./fake-core.js";

/** A whole View State at `seq` with the remux queue holding `ids`. */
function whole(seq, ids = ["01", "02", "03"]) {
  return {
    seq,
    job: null,
    run: { ...IDLE_RUN },
    busy: false,
    defaults: null,
    presets: [],
    remux: modeView(ids.map(id => row(id)), { directory: "/anime/show", manifest_rev: 1 }),
    extract: modeView([], {}, "extract"),
    telemetry: null,
  };
}

test("a payload not newer than the applied state is dropped, whole or patch", () => {
  const applied = whole(5);
  assert.deepEqual(nextState(applied, whole(5)), { action: "drop" });
  assert.deepEqual(nextState(applied, { seq: 4, base: 3 }), { action: "drop" });
  assert.deepEqual(nextState(applied, { seq: 5, base: 4 }), { action: "drop" });
});

test("a payload with no base is a whole state, with or without one applied", () => {
  const state = whole(2);
  assert.deepEqual(nextState(null, state), { action: "full", state, rows: ALL_ROWS });
  assert.deepEqual(nextState(whole(1), state), { action: "full", state, rows: ALL_ROWS });
});

test("a patch of another state, or of none, resyncs", () => {
  assert.deepEqual(nextState(whole(5), { seq: 7, base: 6 }), { action: "resync" });
  assert.deepEqual(nextState(null, { seq: 1, base: 0 }), { action: "resync" });
});

test("a patch with no change only moves seq", () => {
  const applied = whole(5);
  const { action, state, rows } = nextState(applied, { seq: 6, base: 5 });
  assert.equal(action, "merge");
  assert.deepEqual(state, { ...applied, seq: 6 });
  assert.equal(state.remux, applied.remux);
  assert.deepEqual(rows, { remux: [], extract: [] });
  assert.equal("base" in state, false);
});

test("a top-level key replaces its value, and a job sent as null clears it", () => {
  const applied = { ...whole(5), job: jobView(), busy: true };
  const run = { phase: "running", mode: "remux", settings: null };
  let { state } = nextState(applied, { seq: 6, base: 5, run });
  assert.deepEqual(state.run, run);
  assert.equal(state.job, applied.job);
  ({ state } = nextState(state, { seq: 7, base: 6, job: null, busy: false }));
  assert.equal(state.job, null);
  assert.equal(state.busy, false);
});

test("row_changes replace the rows at their indices and name them, the other mode untouched", () => {
  const applied = whole(5);
  const before = structuredClone(applied);
  const changed = row("02", { status: "processing", progress: 40 });
  const { state, rows } = nextState(applied, { seq: 6, base: 5, remux: { row_changes: [[1, changed]] } });
  assert.deepEqual(rows, { remux: [1], extract: [] });
  assert.deepEqual(state.remux.rows, [applied.remux.rows[0], changed, applied.remux.rows[2]]);
  assert.equal(state.remux.rows[0], applied.remux.rows[0]);
  assert.equal(state.extract, applied.extract);
  // The applied state and its rows array are as they were.
  assert.deepEqual(applied, before);
  assert.notEqual(state.remux.rows, applied.remux.rows);
});

test("rows sent whole replace the list, and a ModeView key changes alone", () => {
  const applied = whole(5);
  const next = whole(6, ["03", "01"]).remux.rows;
  let { state, rows } = nextState(applied, { seq: 6, base: 5, remux: { manifest_rev: 2, rows: next } });
  assert.deepEqual(rows, { remux: "all", extract: [] });
  assert.equal(state.remux.rows, next);
  assert.equal(state.remux.manifest_rev, 2);
  assert.equal(state.remux.directory, "/anime/show");

  ({ state, rows } = nextState(state, { seq: 7, base: 6, extract: { scan: { state: "scanning", directory: "/x" } } }));
  assert.deepEqual(rows, { remux: [], extract: [] });
  assert.deepEqual(state.extract.scan, { state: "scanning", directory: "/x" });
  assert.equal(state.extract.rows, applied.extract.rows);
});
