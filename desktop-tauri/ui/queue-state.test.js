import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  createQueueState,
  reconcileQueueSelection,
  reduceQueueEvent,
  guardUnsettledItems,
  carryOverAssignments,
  buildTrackAssignmentsForManifest,
  createQueueStateFromSnapshot,
  formatOutcomeDuration,
  formatElapsedDuration,
  setItemSelected,
  setAllSelected,
  reorderItem,
  getSelectedCount,
  isSameDirectory,
} from "./queue-state.js";

/**
 * The events the Rust Engine Bridge sends to settle a stopped job and a failed job whose terminal
 * carries no error text, as the webview receives them: `{ stopped, failed }`, each
 * `{ terminal, settled }`. A golden fixture, written by the Rust test
 * `test_settlement_wording_matches_the_golden_fixture`.
 */
const fixture = JSON.parse(readFileSync(new URL("./fixtures/settlement-events.json", import.meta.url), "utf8"));

// The wording the bridge settles with, read from the fixture so a change to it is one edit there.
const CANCELLED_REASON = fixture.stopped.settled[0].reason;
const BATCH_FAILED = fixture.failed.settled[0].error;

const manifestOf = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });

/** The `batch_start` event of an Active Batch of `ids`. */
const batchStartOf = ids => ({
  version: 1,
  type: "batch_start",
  total: ids.length,
  input_dir: "/anime",
  items: ids.map(id => ({ id, file: id })),
});

/** A manual subtitle assignment in the shape app.js stores in `QueueTracker.assignments`. */
const subtitle = (id, path) => ({
  target_video: id,
  asset_path: path,
  track_type: "subtitle",
  language: "ind",
  title: null,
  offset_ms: null,
});

test("createQueueState initializes state from manifest v2", () => {
  const manifest = {
    version: 2,
    items: [
      {
        id: "Season 01/Show - 01.mkv",
        name: "Show - 01.mkv",
        release: "Season 01",
        metadata: { resolution: "1080p", video_codec: "HEVC" },
      },
      {
        id: "Season 01/Show - 02.mkv",
        name: "Show - 02.mkv",
        release: "Season 01",
        metadata: { resolution: "1080p", video_codec: "HEVC" },
      },
    ],
  };

  const state = createQueueState(manifest);
  assert.equal(state.totalCount, 2);
  assert.equal(state.completedCount, 0);
  assert.equal(state.items.size, 2);
  assert.ok(state.items.has("Season 01/Show - 01.mkv"));
  assert.ok(state.items.has("Season 01/Show - 02.mkv"));

  const item1 = state.items.get("Season 01/Show - 01.mkv");
  assert.equal(item1.status, "queued");
  assert.equal(item1.name, "Show - 01.mkv");
  assert.equal(item1.release, "Season 01");
  assert.deepEqual(item1.metadata, { resolution: "1080p", video_codec: "HEVC" });
});

test("createQueueState preserves inspection failures for presentation", () => {
  const state = createQueueState({
    version: 2,
    items: [{
      id: "broken.mkv",
      name: "broken.mkv",
      metadata: {
        resolution: null,
        video_codec: null,
        video_tag: null,
        audio_summary: null,
        has_flac: null,
        sub_summary: null,
        fonts_count: null,
      },
      inspection_error: "mkvmerge inspection timed out",
    }],
  });

  assert.equal(state.items.get("broken.mkv").inspectionError, "mkvmerge inspection timed out");
});

test("createQueueState correctly distinguishes duplicate basenames in different directories", () => {
  const manifest = {
    version: 2,
    items: [
      { id: "Season 01/Show - 01.mkv", name: "Show - 01.mkv" },
      { id: "Season 02/Show - 01.mkv", name: "Show - 01.mkv" },
    ],
  };

  const state = createQueueState(manifest);
  assert.equal(state.totalCount, 2);
  assert.ok(state.items.has("Season 01/Show - 01.mkv"));
  assert.ok(state.items.has("Season 02/Show - 01.mkv"));
  assert.notEqual(
    state.items.get("Season 01/Show - 01.mkv"),
    state.items.get("Season 02/Show - 01.mkv")
  );
});

test("reduceQueueEvent handles batch_start, item_start, and item_complete (completed)", () => {
  let state = createQueueState(manifestOf(["ep1.mkv"]));

  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 1,
  });
  assert.ok(state.batchStartTime > 0);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_start",
    id: "ep1.mkv",
  });
  assert.equal(state.items.get("ep1.mkv").status, "processing");

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });
  const item = state.items.get("ep1.mkv");
  assert.equal(item.status, "completed");
  assert.equal(state.completedCount, 1);
  assert.equal(state.errorCount, 0);
});

test("reduceQueueEvent handles item_complete with skipped, planned, and error", () => {
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv", "ep3.mkv"]));

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "skipped",
    reason: "output already exists",
  });
  assert.equal(state.items.get("ep1.mkv").status, "skipped");
  assert.equal(state.items.get("ep1.mkv").reason, "output already exists");
  assert.equal(state.skippedCount, 1);

  // Dry-run plans count as completed.
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "planned",
  });
  assert.equal(state.items.get("ep2.mkv").status, "planned");
  assert.equal(state.completedCount, 1);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep3.mkv",
    status: "error",
    error: "mkvmerge failed",
  });
  assert.equal(state.items.get("ep3.mkv").status, "error");
  assert.equal(state.items.get("ep3.mkv").error, "mkvmerge failed");
  assert.equal(state.errorCount, 1);
});

test("reduceQueueEvent rejects unknown ID and does NOT invent cards", () => {
  let state = createQueueState(manifestOf(["known.mkv"]));

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_start",
    id: "unknown.mkv",
  });
  assert.equal(state.items.size, 1);
  assert.equal(state.items.has("unknown.mkv"), false);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "unknown.mkv",
    status: "completed",
  });
  assert.equal(state.items.size, 1);
  assert.equal(state.completedCount, 0);
});

test("reduceQueueEvent is idempotent against duplicate terminal events", () => {
  let state = createQueueState(manifestOf(["ep1.mkv"]));

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });
  assert.equal(state.completedCount, 1);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });
  assert.equal(state.completedCount, 1);

  // Out-of-order item_start after completion
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_start",
    id: "ep1.mkv",
  });
  assert.equal(state.items.get("ep1.mkv").status, "completed");
});

test("guardUnsettledItems settles open participants as errors with the bridge's wording and warns", t => {
  const warn = t.mock.method(console, "warn", () => {});
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv", "ep3.mkv"]));
  state = reduceQueueEvent(state, batchStartOf(["ep1.mkv", "ep2.mkv", "ep3.mkv"]));
  state = reduceQueueEvent(state, { version: 1, type: "item_complete", id: "ep1.mkv", status: "completed" });
  // ep2 was started but never received item_complete; ep3 was never started.
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep2.mkv" });
  state = reduceQueueEvent(state, { version: 1, type: "batch_complete", success: true });

  assert.equal(guardUnsettledItems(state, false), state, "the guard settles in place");

  assert.equal(state.items.get("ep1.mkv").status, "completed");
  for (const id of ["ep2.mkv", "ep3.mkv"]) {
    assert.equal(state.items.get(id).status, "error");
    assert.equal(state.items.get(id).error, BATCH_FAILED);
    assert.equal(state.items.get(id).reason, null);
  }
  assert.equal(state.completedCount, 1);
  assert.equal(state.errorCount, 2);
  assert.equal(state.batchSuccess, false, "a run that broke the protocol is no success");
  assert.equal(warn.mock.callCount(), 1);
  assert.match(warn.mock.calls[0].arguments[0], /Item Settlement.*ep2\.mkv, ep3\.mkv/);
});

test("the guard settles with the wording the Engine Bridge sends, as the golden fixture pins it", t => {
  t.mock.method(console, "warn", () => {});
  // The guard exists for when bridge events were lost, so it cannot read its wording from them.
  const guarded = cancelled => {
    const state = reduceQueueEvent(createQueueState(manifestOf(["ep1"])), batchStartOf(["ep1"]));
    guardUnsettledItems(state, cancelled);
    return state.items.get("ep1");
  };
  const [stopped] = fixture.stopped.settled;
  const [failed] = fixture.failed.settled;

  const cancelled = guarded(true);
  assert.equal(cancelled.status, stopped.status);
  assert.equal(cancelled.reason, stopped.reason);
  const errored = guarded(false);
  assert.equal(errored.status, failed.status);
  assert.equal(errored.error, failed.error);
});

test("a job that ended before batch_start leaves every queued item and selection alone", t => {
  const warn = t.mock.method(console, "warn", () => {});
  const state = createQueueState(manifestOf(["ep1", "ep2"]));
  setItemSelected(state, "ep2", false);
  // The bridge's own terminal for a validation failure: no batch_start came before it.
  reduceQueueEvent(state, { version: 1, type: "batch_complete", success: false, cancelled: false, error: "bad request" });

  for (const cancelled of [false, true]) guardUnsettledItems(state, cancelled);

  assert.deepEqual([...state.items.values()].map(item => item.status), ["queued", "queued"]);
  assert.deepEqual([...state.items.values()].map(item => item.selected), [true, false]);
  assert.equal(state.errorCount + state.cancelledCount, 0);
  assert.equal(warn.mock.callCount(), 0);
});

test("completion of every item does not mark the batch a success before batch_complete", () => {
  let state = createQueueState(manifestOf(["ep1.mkv"]));

  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 1,
  });
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });

  assert.equal(state.completedCount, 1);
  assert.equal(state.errorCount, 0);
  // No batch_complete yet: not a success.
  assert.equal(state.batchSuccess, null);
});

test("reduceQueueEvent evolves the single owned state object in place", () => {
  const state = createQueueState(manifestOf(["ep1.mkv"]));
  const item = state.items.get("ep1.mkv");

  const started = reduceQueueEvent(state, {
    version: 1,
    type: "item_start",
    id: "ep1.mkv",
  }, 1000);

  // Ownership contract: same state object, same items Map, same item record.
  assert.equal(started, state);
  assert.equal(started.items, state.items);
  assert.equal(started.items.get("ep1.mkv"), item);
  assert.equal(item.status, "processing");
  assert.equal(item.startTime, 1000);

  const completed = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  }, 2000);

  assert.equal(completed, state);
  assert.equal(state.completedCount, 1);
  assert.equal(item.status, "completed");
  assert.equal(item.durationSec, 1);
});

test("reduceQueueEvent handles 10,000 start/complete pairs in linear time", () => {
  const total = 10000;
  const ids = Array.from({ length: total }, (_, i) => `ep${i}.mkv`);
  let state = createQueueState(manifestOf(ids));
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total }, 0);

  const begin = performance.now();
  ids.forEach((id, i) => {
    state = reduceQueueEvent(state, { version: 1, type: "item_start", id }, i);
    state = reduceQueueEvent(state, {
      version: 1,
      type: "item_complete",
      id,
      status: "completed",
    }, i + 200);
  });
  const elapsedMs = performance.now() - begin;

  assert.equal(state.completedCount, total);
  assert.equal(state.errorCount, 0);
  // O(N) map cloning per event would push this into multi-second territory
  // even on slow hardware; a generous bound catches reintroduced O(N²).
  assert.ok(elapsedMs < 2000, `20k events took ${elapsedMs.toFixed(0)}ms (expected < 2000ms)`);
});

test("sub-second completions render as <0.1 instead of 0.0", () => {
  let state = createQueueState(manifestOf(["fast.mkv"]));
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total: 1 }, 1000);
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "fast.mkv" }, 1050);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "fast.mkv",
    status: "completed",
  }, 1099);

  assert.equal(formatOutcomeDuration(state.items.get("fast.mkv").durationSec), "<0.1s");
  assert.equal(state.items.get("fast.mkv").durationSec, 0.049);
});

test("skips and plans report no duration instead of elapsed batch time", () => {
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv"]));
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total: 2 }, 1000);

  // Skipped without a prior item_start (planning-time skip).
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "skipped",
    reason: "subtitle files found but none matched",
  }, 56000);

  assert.equal(state.items.get("ep1.mkv").status, "skipped");
  assert.equal(state.skippedCount, 1);
  assert.equal(state.items.get("ep1.mkv").durationSec, null);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "planned",
  }, 57000);

  assert.equal(state.completedCount, 1);
  assert.equal(state.items.get("ep2.mkv").durationSec, null);
});

test("carryOverAssignments keeps selections for surviving items and drops stale ones", () => {
  const assignments = new Map([
    ["Season 01/ep1.mkv", subtitle("Season 01/ep1.mkv", "Season 01/better.ass")],
    ["Season 01/gone.mkv", subtitle("Season 01/gone.mkv", "Season 01/deleted.ass")],
  ]);

  const carried = carryOverAssignments(assignments, manifestOf(["Season 01/ep1.mkv", "Season 01/ep2.mkv"]));

  assert.deepEqual([...carried.entries()], [
    ["Season 01/ep1.mkv", subtitle("Season 01/ep1.mkv", "Season 01/better.ass")],
  ]);
  // Input map is not mutated.
  assert.equal(assignments.size, 2);
});

test("carryOverAssignments returns empty map for missing or itemless manifests", () => {
  const assignments = new Map([["ep1.mkv", subtitle("ep1.mkv", "sub.ass")]]);

  assert.equal(carryOverAssignments(assignments, null).size, 0);
  assert.equal(carryOverAssignments(assignments, manifestOf([])).size, 0);
});

test("reduceQueueEvent treats cancelled items as terminal and counts them as cancelled", () => {
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv"]));

  // Started then cancelled mid-run: reports how long it ran.
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep1.mkv" }, 1000);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "cancelled",
  }, 3500);
  assert.equal(state.items.get("ep1.mkv").status, "cancelled");
  assert.equal(state.items.get("ep1.mkv").durationSec, 2.5);
  assert.equal(state.cancelledCount, 1);

  // Cancelled before start (queued -> cancelled): no fabricated duration.
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "cancelled",
  }, 3600);
  assert.equal(state.items.get("ep2.mkv").status, "cancelled");
  assert.equal(state.items.get("ep2.mkv").durationSec, null);

  // Terminal: duplicate completion and late starts are ignored.
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep2.mkv" });
  assert.equal(state.items.get("ep1.mkv").status, "cancelled");
  assert.equal(state.items.get("ep2.mkv").status, "cancelled");
  assert.equal(state.completedCount, 0);
});

test("batch_complete carries the cancelled flag and fails batch success", () => {
  let state = createQueueState(manifestOf(["ep1.mkv"]));

  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total: 1 });
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "cancelled",
  });
  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_complete",
    success: false,
    cancelled: true,
  });

  assert.equal(state.batchCompleted, true);
  assert.equal(state.batchSuccess, false);
  assert.equal(state.batchCancelled, true);
});

test("guardUnsettledItems settles open participants as cancelled with the engine's reason after a stop", t => {
  t.mock.method(console, "warn", () => {});
  let state = createQueueState(manifestOf(["ep1.mkv"]));
  state = reduceQueueEvent(state, batchStartOf(["ep1.mkv"]));

  guardUnsettledItems(state, true);

  const item = state.items.get("ep1.mkv");
  assert.equal(item.status, "cancelled");
  assert.equal(item.reason, CANCELLED_REASON);
  assert.equal(item.error, null);
  assert.equal(state.cancelledCount, 1);
  assert.equal(state.errorCount, 0);
});

test("item transitioning directly from queued to error leaves durationSec as null instead of batchStartTime fallback", () => {
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv"]));
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total: 2 }, 1000);

  // ep1 transitions directly from queued to error without prior item_start
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "error",
    error: "probe failed before start",
  }, 6000);

  assert.equal(state.items.get("ep1.mkv").status, "error");
  assert.equal(state.items.get("ep1.mkv").durationSec, null);
  assert.equal(state.errorCount, 1);

  // ep2 starts and then errors: duration is measured from its item_start
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep2.mkv" }, 7000);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "error",
    error: "ffmpeg process crashed",
  }, 9500);

  assert.equal(state.items.get("ep2.mkv").status, "error");
  assert.equal(state.items.get("ep2.mkv").durationSec, 2.5);
  assert.equal(state.errorCount, 2);
});

test("buildTrackAssignmentsForManifest builds track assignments from carried over manual assignments", () => {
  const assignments = new Map([
    ["Season 1/ep1.mkv", subtitle("Season 1/ep1.mkv", "/subs/ep1.ass")],
    ["Season 1/ep2.mkv", { ...subtitle("Season 1/ep2.mkv", "/subs/ep2.ass"), language: "", title: "Signs", offset_ms: -250 }],
  ]);

  const result = buildTrackAssignmentsForManifest(assignments, manifestOf(["Season 1/ep1.mkv", "Season 1/ep2.mkv"]));
  assert.deepEqual(result, [
    subtitle("Season 1/ep1.mkv", "/subs/ep1.ass"),
    {
      target_video: "Season 1/ep2.mkv",
      asset_path: "/subs/ep2.ass",
      track_type: "subtitle",
      language: null,
      title: "Signs",
      offset_ms: -250,
    },
  ]);
});

test("buildTrackAssignmentsForManifest returns null when assignments map is empty", () => {
  assert.equal(buildTrackAssignmentsForManifest(new Map(), manifestOf(["ep1.mkv"])), null);
});

test("buildTrackAssignmentsForManifest ignores assignments for items not in the manifest", () => {
  const manifest = manifestOf(["ep1.mkv"]);
  const assignments = new Map([
    ["ep1.mkv", subtitle("ep1.mkv", "/subs/ep1.ass")],
    ["deleted_ep2.mkv", subtitle("deleted_ep2.mkv", "/subs/ep2.ass")],
  ]);

  const result = buildTrackAssignmentsForManifest(assignments, manifest);
  assert.deepEqual(result, [subtitle("ep1.mkv", "/subs/ep1.ass")]);

  const orphanedAssignments = new Map([
    ["orphaned.mkv", subtitle("orphaned.mkv", "/subs/orphaned.ass")],
  ]);
  assert.equal(buildTrackAssignmentsForManifest(orphanedAssignments, manifest), null);
});

test("createQueueStateFromSnapshot constructs state and counts from JobSnapshot", () => {
  const snapshot = {
    job_id: 42,
    mode: "extract",
    status: "completed",
    start_time_ms: 123456789,
    ordered_item_ids: ["ep1.mkv", "ep2.mkv", "ep3.mkv", "ep4.mkv"],
    items: {
      "ep1.mkv": { id: "ep1.mkv", file: "Show - 01.mkv", status: "completed", duration_sec: 5.4 },
      "ep2.mkv": { id: "ep2.mkv", file: "Show - 02.mkv", status: "skipped", reason: "Already extracted" },
      "ep3.mkv": { id: "ep3.mkv", file: "Show - 03.mkv", status: "cancelled", reason: "Cancelled by user" },
      "ep4.mkv": { id: "ep4.mkv", file: "Show - 04.mkv", status: "error", error: "mkvextract failed" },
    },
  };

  const state = createQueueStateFromSnapshot(snapshot);
  assert.equal(state.totalCount, 4);
  assert.equal(state.completedCount, 1);
  assert.equal(state.skippedCount, 1);
  assert.equal(state.cancelledCount, 1);
  assert.equal(state.errorCount, 1);
  assert.equal(state.batchCompleted, true);
  assert.equal(state.batchSuccess, true);
  assert.equal(state.batchCancelled, false);
  assert.equal(state.items.get("ep1.mkv").durationSec, 5.4);
  assert.equal(state.items.get("ep2.mkv").reason, "Already extracted");
  assert.equal(state.items.get("ep4.mkv").error, "mkvextract failed");
});

test("createQueueStateFromSnapshot guards open items of a cancelled snapshot, settling them as cancelled with a warning", t => {
  const warn = t.mock.method(console, "warn", () => {});
  const snapshot = {
    job_id: 101,
    mode: "remux",
    status: "cancelled",
    start_time_ms: 123456789,
    ordered_item_ids: ["ep1.mkv", "ep2.mkv", "ep3.mkv"],
    items: {
      "ep1.mkv": { id: "ep1.mkv", file: "Show - 01.mkv", status: "completed", duration_sec: 4.2 },
      "ep2.mkv": { id: "ep2.mkv", file: "Show - 02.mkv", status: "processing" },
      "ep3.mkv": { id: "ep3.mkv", file: "Show - 03.mkv", status: "queued" },
    },
  };

  const state = createQueueStateFromSnapshot(snapshot);
  assert.equal(state.batchCompleted, true);
  assert.equal(state.batchCancelled, true);
  assert.equal(state.batchSuccess, false);
  assert.equal(state.totalCount, 3);
  assert.equal(state.completedCount, 1);
  assert.equal(state.cancelledCount, 2);
  assert.equal(state.errorCount, 0);
  assert.equal(state.items.get("ep1.mkv").status, "completed");
  assert.equal(state.items.get("ep2.mkv").status, "cancelled");
  assert.equal(state.items.get("ep3.mkv").status, "cancelled");
  assert.equal(state.items.get("ep3.mkv").reason, CANCELLED_REASON);
  assert.equal(warn.mock.callCount(), 1);
});

test("createQueueStateFromSnapshot guards open items of a failed snapshot, settling them as errors with a warning", t => {
  const warn = t.mock.method(console, "warn", () => {});
  const snapshot = {
    job_id: 102,
    mode: "extract",
    status: "error",
    start_time_ms: 123456789,
    ordered_item_ids: ["ep1.mkv", "ep2.mkv", "ep3.mkv"],
    items: {
      "ep1.mkv": { id: "ep1.mkv", file: "Show - 01.mkv", status: "completed", duration_sec: 5.0 },
      "ep2.mkv": { id: "ep2.mkv", file: "Show - 02.mkv", status: "processing" },
      "ep3.mkv": { id: "ep3.mkv", file: "Show - 03.mkv", status: "queued" },
    },
  };

  const state = createQueueStateFromSnapshot(snapshot);
  assert.equal(state.batchCompleted, true);
  assert.equal(state.batchCancelled, false);
  assert.equal(state.batchSuccess, false);
  assert.equal(state.totalCount, 3);
  assert.equal(state.completedCount, 1);
  assert.equal(state.cancelledCount, 0);
  assert.equal(state.errorCount, 2);
  assert.equal(state.items.get("ep1.mkv").status, "completed");
  assert.equal(state.items.get("ep2.mkv").status, "error");
  assert.equal(state.items.get("ep3.mkv").status, "error");
  assert.equal(state.items.get("ep3.mkv").error, BATCH_FAILED);
  assert.equal(warn.mock.callCount(), 1);
});

test("a restored running snapshot makes its items the participants the guard settles on job-done", t => {
  const warn = t.mock.method(console, "warn", () => {});
  const snapshot = {
    job_id: 104,
    mode: "remux",
    status: "running",
    start_time_ms: 123456789,
    ordered_item_ids: ["ep2.mkv", "ep1.mkv"],
    items: {
      "ep1.mkv": { id: "ep1.mkv", file: "Show - 01.mkv", status: "completed", duration_sec: 3.1 },
      "ep2.mkv": { id: "ep2.mkv", file: "Show - 02.mkv", status: "processing" },
    },
  };
  const state = createQueueStateFromSnapshot(snapshot);
  assert.deepEqual(state.batchItemIds, ["ep2.mkv", "ep1.mkv"]);
  assert.equal(warn.mock.callCount(), 0, "a running snapshot has nothing to guard yet");

  // An item outside the snapshot (a later scan's) is no participant, so the guard skips it.
  state.items.set("other.mkv", { ...state.items.get("ep2.mkv"), id: "other.mkv", status: "queued" });
  guardUnsettledItems(state, true);

  assert.equal(state.items.get("ep2.mkv").status, "cancelled");
  assert.equal(state.items.get("ep2.mkv").reason, CANCELLED_REASON);
  assert.equal(state.items.get("ep1.mkv").status, "completed");
  assert.equal(state.items.get("other.mkv").status, "queued");
  assert.equal(state.cancelledCount, 1);
  assert.equal(warn.mock.callCount(), 1);
});

test("createQueueStateFromSnapshot sets startTime for active in-flight processing items", () => {
  const snapshot = {
    job_id: 103,
    mode: "remux",
    status: "running",
    start_time_ms: 123456789,
    ordered_item_ids: ["ep1.mkv", "ep2.mkv"],
    items: {
      "ep1.mkv": { id: "ep1.mkv", file: "Show - 01.mkv", status: "processing" },
      "ep2.mkv": { id: "ep2.mkv", file: "Show - 02.mkv", status: "queued" },
    },
  };

  const fixedNow = 1700000000000;
  const state = createQueueStateFromSnapshot(snapshot, fixedNow);
  assert.equal(state.batchCompleted, false);
  assert.equal(state.items.get("ep1.mkv").status, "processing");
  assert.equal(state.items.get("ep1.mkv").startTime, fixedNow);
  assert.equal(state.items.get("ep2.mkv").status, "queued");
  assert.equal(state.items.get("ep2.mkv").startTime, null);
});

/**
 * What a card shows of an item: the live reducer and a reloaded snapshot must agree on all of it.
 */
function presentation(state) {
  return {
    items: [...state.items.values()].map(({ id, name, status, reason, error, durationSec }) => ({ id, name, status, reason, error, durationSec })),
    counts: [state.completedCount, state.skippedCount, state.cancelledCount, state.errorCount, state.totalCount],
    flags: [state.batchCompleted, state.batchSuccess, state.batchCancelled],
  };
}

// Each scenario is the event sequence the Engine Bridge emits for a job ending with open items
// (ep1 done by the engine, ep2 and ep3 started, ep4 never started; the bridge settles ep2..ep4 in
// batch order before the terminal), and the JobSnapshot those events leave, written as Rust
// serializes it. The terminal and the settling `item_complete` events are the Rust-produced ones
// in the golden fixture; only the successful scenario, whose wording the guard never repeats,
// overrides a field of the failed one.
const SETTLED_JOBS = [
  { name: "stopped", ...fixture.stopped, snapshotStatus: "cancelled" },
  { name: "failed", ...fixture.failed, snapshotStatus: "error" },
  {
    // The engine called the batch a success but left items open; the bridge's snapshot status is
    // `error`, so a live batch with those errors is no success either.
    name: "successful with open items",
    terminal: { ...fixture.failed.terminal, success: true },
    settled: fixture.failed.settled.map(event => ({ ...event, error: "Protocol error: missing completion event" })),
    snapshotStatus: "error",
  },
];

for (const { name, terminal, settled: settledEvents, snapshotStatus } of SETTLED_JOBS) {
  test(`live ${name} job events and a reloaded snapshot present the same items`, t => {
    const warn = t.mock.method(console, "warn", () => {});
    const ids = ["ep1", "ep2", "ep3", "ep4"];
    assert.deepEqual(settledEvents.map(event => event.id), ids.slice(1), "the fixture settles ep2..ep4 in batch order");
    const [ep2, ep3, ep4] = settledEvents;
    const live = createQueueState(manifestOf([]));
    const events = [
      batchStartOf(ids),
      { version: 1, type: "item_start", id: "ep1" },
      { version: 1, type: "item_complete", id: "ep1", status: "completed", duration_sec: 4.2 },
      { version: 1, type: "item_start", id: "ep2" },
      { version: 1, type: "item_progress", id: "ep2", progress: 40 },
      { version: 1, type: "item_start", id: "ep3" },
      // The fixture holds no durations: the bridge measures them for an item that had started.
      { ...ep2, duration_sec: 1.5 },
      { ...ep3, duration_sec: 0.7 },
      ep4,
      terminal,
    ];
    for (const event of events) reduceQueueEvent(live, event, 1_000);

    const item = (id, fields) => ({
      id, file: id, status: "queued", outcome: null, progress: null, start_time_ms: null,
      duration_sec: null, reason: null, error: null, ...fields,
    });
    const settled = { status: ep2.status, outcome: ep2.status, reason: ep2.reason ?? null, error: ep2.error ?? null };
    const snapshot = {
      job_id: 7, mode: "remux", status: snapshotStatus, start_time_ms: 1_000,
      total_items: 4, completed_items: 4, input_dir: "/anime", effective_settings: null,
      ordered_item_ids: ids,
      items: {
        ep1: item("ep1", { status: "completed", outcome: "completed", start_time_ms: 1_000, duration_sec: 4.2 }),
        ep2: item("ep2", { ...settled, progress: 40, start_time_ms: 1_000, duration_sec: 1.5 }),
        ep3: item("ep3", { ...settled, start_time_ms: 1_000, duration_sec: 0.7 }),
        ep4: item("ep4", settled),
      },
    };

    assert.deepEqual(presentation(createQueueStateFromSnapshot(snapshot)), presentation(live));
    assert.equal(warn.mock.callCount(), 0, "a settled run needs neither guard");
  });
}

test("snapshot projects only ordered Active Batch items, saved progress and per-item elapsed start", () => {
  const snapshot = {
    status: "running",
    start_time_ms: 100,
    total_items: 3,
    ordered_item_ids: ["02", "01", "missing"],
    items: {
      "01": { id: "01", file: "01.mkv", status: "queued" },
      "03": { id: "03", file: "03.mkv", status: "queued" },
      "02": { id: "02", file: "02.mkv", status: "processing", progress: 70, start_time_ms: 2000 },
    },
  };
  const state = createQueueStateFromSnapshot(snapshot, 30000);
  assert.deepEqual(state.order, ["02", "01"]);
  assert.deepEqual([...state.items.keys()], ["02", "01"]);
  assert.equal(state.totalCount, 2);
  assert.equal(state.items.get("02").progress, 70);
  assert.equal(state.items.get("02").startTime, 2000);

  reduceQueueEvent(state, { version: 1, type: "item_complete", id: "02", status: "completed", duration_sec: 4.2 });
  assert.equal(state.items.get("02").durationSec, 4.2);
  assert.equal(state.items.get("02").status, "completed");

  snapshot.items["02"].progress = 125;
  assert.equal(createQueueStateFromSnapshot(snapshot).items.get("02").progress, 100);
});

test("formatOutcomeDuration formats seconds across the sub-0.1s, seconds, and minutes thresholds", () => {
  assert.equal(formatOutcomeDuration(0), "<0.1s");
  assert.equal(formatOutcomeDuration(0.04), "<0.1s");
  assert.equal(formatOutcomeDuration(4.2), "4.2s");
  assert.equal(formatOutcomeDuration(59.94), "59.9s");
  assert.equal(formatOutcomeDuration(59.96), "1m 0.0s");
  assert.equal(formatOutcomeDuration(60.0), "1m 0.0s");
  assert.equal(formatOutcomeDuration(75.4), "1m 15.4s");
  assert.equal(formatOutcomeDuration(119.96), "2m 0.0s");
});

test("formatElapsedDuration formats negative seconds, seconds, and minutes", () => {
  assert.equal(formatElapsedDuration(-5), "0s");
  assert.equal(formatElapsedDuration(-0.1), "0s");
  assert.equal(formatElapsedDuration(0), "0s");
  assert.equal(formatElapsedDuration(4.2), "4s");
  assert.equal(formatElapsedDuration(42), "42s");
  assert.equal(formatElapsedDuration(59.9), "59s");
  assert.equal(formatElapsedDuration(75.4), "1m 15s");
  assert.equal(formatElapsedDuration(130), "2m 10s");
});

test("reduceQueueEvent uses event.duration_sec directly from engine", () => {
  let state = createQueueState(manifestOf(["ep1.mkv", "ep2.mkv"]));

  // Engine duration wins over the locally measured start/complete delta.
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep1.mkv" }, 1000);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
    duration_sec: 0.08,
  }, 9000);
  assert.equal(state.items.get("ep1.mkv").durationSec, 0.08);

  // Engine duration is kept even for an item that never reported item_start.
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "completed",
    duration_sec: 75.4,
  });
  assert.equal(state.items.get("ep2.mkv").durationSec, 75.4);
});

test("reduceQueueEvent clamps item_progress and preserves float precision", () => {
  let state = createQueueState(manifestOf(["ep1.mkv"]));
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep1.mkv" });

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_progress",
    id: "ep1.mkv",
    progress: 42.75,
  });
  assert.equal(state.items.get("ep1.mkv").progress, 42.75);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_progress",
    id: "ep1.mkv",
    progress: 120,
  });
  assert.equal(state.items.get("ep1.mkv").progress, 100);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_progress",
    id: "ep1.mkv",
    progress: -10,
  });
  assert.equal(state.items.get("ep1.mkv").progress, 0);
});

test("reduceQueueEvent populates items from batch_start when provided", () => {
  let state = createQueueState(manifestOf([]));
  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 2,
    items: [
      { id: "e1", file: "Episode 1.mkv" },
      { id: "e2", file: "Episode 2.mkv" },
    ],
  });

  assert.equal(state.items.size, 2);
  assert.equal(state.totalCount, 2);
  assert.equal(state.items.get("e1").status, "queued");
  assert.equal(state.items.get("e1").name, "Episode 1.mkv");
  assert.equal(state.items.get("e2").status, "queued");
  assert.equal(state.items.get("e2").name, "Episode 2.mkv");
});

test("BatchStart from an empty snapshot stays the ordered, selected base for the next same-directory rescan", () => {
  // Reload lands before any BatchStart, so nothing but the engine knows the batch.
  let state = createQueueState(manifestOf([]));
  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 2,
    items: [{ id: "02", file: "Episode 02.mkv" }, { id: "01", file: "Episode 01.mkv" }],
  });
  assert.deepEqual(state.order, ["02", "01"]);
  assert.deepEqual(state.knownOrder, ["02", "01"]);

  // A later rescan of the same directory discovers one more episode.
  state = reconcileQueueSelection(createQueueState(manifestOf(["02", "01", "03"])), state);

  assert.deepEqual(state.order, ["02", "01", "03"]);
  assert.deepEqual(state.knownOrder, ["02", "01", "03"]);
  assert.equal(state.items.get("02").selected, true);
  assert.equal(state.items.get("01").selected, true);
  assert.equal(state.items.get("03").selected, false);
});

test("BatchStart records each participant once even when its item list repeats an ID", () => {
  const state = createQueueState(manifestOf([]));

  reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 2,
    items: [{ id: "01" }, { id: "02" }, { id: "01" }],
  });

  assert.deepEqual(state.order, ["01", "02"]);
  assert.deepEqual(state.knownOrder, ["01", "02"]);
  assert.equal(state.items.size, 2);
});

test("reduceQueueEvent hydrates 10,000 BatchStart items into an empty state in linear time", () => {
  const total = 10000;
  const items = Array.from({ length: total }, (_, i) => ({ id: `ep${i}.mkv`, file: `Episode ${i}.mkv` }));
  const state = createQueueState(manifestOf([]));

  const begin = performance.now();
  reduceQueueEvent(state, { version: 1, type: "batch_start", total, items }, 0);
  const elapsedMs = performance.now() - begin;

  assert.equal(state.items.size, total);
  assert.equal(state.order.length, total);
  assert.equal(new Set(state.order).size, total);
  assert.deepEqual(state.knownOrder, state.order);
  assert.equal(state.batchItemIds.length, total);
  // A repeated linear state.order.includes per item would be quadratic here.
  assert.ok(elapsedMs < 2000, `empty-state BatchStart hydration took ${elapsedMs.toFixed(0)}ms (expected < 2000ms)`);
});

test("item selection and reordering helpers", () => {
  const state = createQueueState(manifestOf(["e1", "e2", "e3"]));
  assert.equal(getSelectedCount(state), 3);
  assert.equal(state.items.get("e1").selected, true);

  setItemSelected(state, "e2", false);
  assert.equal(state.items.get("e2").selected, false);
  assert.equal(getSelectedCount(state), 2);

  setItemSelected(state, "e2", true);
  assert.equal(state.items.get("e2").selected, true);
  assert.equal(getSelectedCount(state), 3);

  setAllSelected(state, false);
  assert.equal(getSelectedCount(state), 0);
  assert.equal(state.items.get("e1").selected, false);
  assert.equal(state.items.get("e2").selected, false);
  assert.equal(state.items.get("e3").selected, false);

  setAllSelected(state, true);
  assert.equal(getSelectedCount(state), 3);
  assert.equal(state.items.get("e1").selected, true);

  assert.deepEqual(state.order, ["e1", "e2", "e3"]);
  reorderItem(state, "e2", "up");
  assert.deepEqual(state.order, ["e2", "e1", "e3"]);
  reorderItem(state, "e2", "up"); // at top, noop
  assert.deepEqual(state.order, ["e2", "e1", "e3"]);

  reorderItem(state, "e2", "down");
  assert.deepEqual(state.order, ["e1", "e2", "e3"]);
  reorderItem(state, "e3", "down"); // at bottom, noop
  assert.deepEqual(state.order, ["e1", "e2", "e3"]);
});

test("reconcileQueueSelection keeps surviving choices and order, appending new IDs unselected", () => {
  const previous = createQueueState(manifestOf(["ep1", "ep2", "gone"]));
  setItemSelected(previous, "ep1", false);
  reorderItem(previous, "ep2", "up");

  const replacement = reconcileQueueSelection(createQueueState(manifestOf(["ep1", "new", "ep2"])), previous);
  assert.deepEqual(replacement.order, ["ep2", "ep1", "new"]);
  assert.deepEqual(replacement.order.map(id => replacement.items.get(id).selected), [true, false, false]);
  assert.equal(replacement.items.has("gone"), false);
  assert.deepEqual(previous.order, ["ep2", "ep1", "gone"]);
});

test("isSameDirectory compares Directory Identities byte for byte", () => {
  assert.equal(isSameDirectory("/anime/show", "/anime/show"), true);
  assert.equal(isSameDirectory("C:\\Anime\\Show", "C:\\Anime\\Show"), true);

  // The UI never normalizes: the Engine Bridge resolves every spelling to one identity first.
  assert.equal(isSameDirectory("/anime/show", "/anime/show/"), false);
  assert.equal(isSameDirectory("/anime/show", "/anime/x/../show"), false);
  assert.equal(isSameDirectory("/anime/Show", "/anime/show"), false);
  assert.equal(isSameDirectory("C:\\Anime\\Show", "c:/anime/show"), false);
  assert.equal(isSameDirectory("/anime/showA", "/anime/showB"), false);

  // Missing or empty identities are never the same, even against themselves.
  assert.equal(isSameDirectory(null, "/anime/show"), false);
  assert.equal(isSameDirectory("/anime/show", null), false);
  assert.equal(isSameDirectory(null, null), false);
  assert.equal(isSameDirectory(undefined, undefined), false);
  assert.equal(isSameDirectory("", "/anime/show"), false);
  assert.equal(isSameDirectory("", ""), false);
});

test("the guard leaves non-participants queued and keeps a successful subset batch successful", t => {
  const warn = t.mock.method(console, "warn", () => {});
  let state = createQueueState(manifestOf(["ep1", "ep2"]));
  state = reduceQueueEvent(state, batchStartOf(["ep1"]));
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep1" });
  state = reduceQueueEvent(state, { version: 1, type: "item_complete", id: "ep1", status: "completed" });
  state = reduceQueueEvent(state, { version: 1, type: "batch_complete", success: true });

  guardUnsettledItems(state, false);

  assert.equal(state.batchSuccess, true);
  assert.equal(state.items.get("ep1").status, "completed");
  assert.equal(state.items.get("ep2").status, "queued");
  assert.equal(warn.mock.callCount(), 0);
});

test("rescan remembers vanished selected ids until selection changes", () => {
  const previous = createQueueState(manifestOf(["ep1", "ep2", "ep3"]));
  setItemSelected(previous, "ep3", false);
  const next = reconcileQueueSelection(createQueueState(manifestOf(["ep1"])), previous);
  assert.deepEqual(next.vanishedSelectedIds, ["ep2"]);

  const again = reconcileQueueSelection(createQueueState(manifestOf(["ep1"])), next);
  assert.deepEqual(again.vanishedSelectedIds, ["ep2"]);

  setItemSelected(again, "ep1", true);
  assert.deepEqual(again.vanishedSelectedIds, []);
});

test("a vanished selected id that reappears is restored selected", () => {
  const previous = createQueueState(manifestOf(["ep1", "ep2"]));
  const gone = reconcileQueueSelection(createQueueState(manifestOf(["ep1"])), previous);
  const back = reconcileQueueSelection(createQueueState(manifestOf(["ep1", "ep2"])), gone);
  assert.equal(back.items.get("ep2").selected, true);
  assert.deepEqual(back.vanishedSelectedIds, []);
});

test("a vanished selected id that reappears preserves its previous relative order", () => {
  const initial = createQueueState(manifestOf(["ep1", "ep2", "ep3"]));
  reorderItem(initial, "ep2", "up");
  assert.deepEqual(initial.order, ["ep2", "ep1", "ep3"]);

  const gone = reconcileQueueSelection(createQueueState(manifestOf(["ep1", "ep3"])), initial);
  assert.deepEqual(gone.order, ["ep1", "ep3"]);
  assert.deepEqual(gone.vanishedSelectedIds, ["ep2"]);

  const back = reconcileQueueSelection(createQueueState(manifestOf(["ep1", "ep2", "ep3"])), gone);
  assert.deepEqual(back.order, ["ep2", "ep1", "ep3"]);
  assert.equal(back.items.get("ep2").selected, true);
  assert.deepEqual(back.vanishedSelectedIds, []);
});

test("setAllSelected clears vanished selected ids", () => {
  const state = createQueueState(manifestOf(["ep1"]));
  state.vanishedSelectedIds = ["x"];
  setAllSelected(state, true);
  assert.deepEqual(state.vanishedSelectedIds, []);

  state.vanishedSelectedIds = ["x"];
  setAllSelected(state, false);
  assert.deepEqual(state.vanishedSelectedIds, []);
});
