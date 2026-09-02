import test from "node:test";
import assert from "node:assert/strict";
import {
  createQueueState,
  reduceQueueEvent,
  finalizeQueueState,
  carryOverAssignments,
  serializeAssignmentsForManifest,
} from "./queue-state.js";

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

  const state = createQueueState(manifest, { mode: "remux", bitrate: "192k" });
  assert.equal(state.version, 1);
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
  const manifest = {
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  };
  let state = createQueueState(manifest);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "batch_start",
    total: 1,
  });
  assert.ok(state.batchStartTime > 0);
  assert.equal(state.expectedTotal, 1);

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
  const manifest = {
    version: 2,
    items: [
      { id: "ep1.mkv", name: "ep1.mkv" },
      { id: "ep2.mkv", name: "ep2.mkv" },
      { id: "ep3.mkv", name: "ep3.mkv" },
    ],
  };
  let state = createQueueState(manifest);

  // Skipped
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

  // Planned (dry-run)
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "planned",
  });
  assert.equal(state.items.get("ep2.mkv").status, "planned");
  assert.equal(state.completedCount, 1);

  // Error (crash/failure)
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
  const manifest = {
    version: 2,
    items: [{ id: "known.mkv", name: "known.mkv" }],
  };
  let state = createQueueState(manifest);

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
  const manifest = {
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  };
  let state = createQueueState(manifest);

  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });
  assert.equal(state.completedCount, 1);

  // Duplicate item_complete
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

test("finalizeQueueState marks unresolved items as errors", () => {
  const manifest = {
    version: 2,
    items: [
      { id: "ep1.mkv", name: "ep1.mkv" },
      { id: "ep2.mkv", name: "ep2.mkv" },
      { id: "ep3.mkv", name: "ep3.mkv" },
    ],
  };
  let state = createQueueState(manifest);

  // ep1 completed normally
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  });

  // ep2 was started but never received item_complete
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_start",
    id: "ep2.mkv",
  });

  // ep3 was never started (still queued)

  // Batch finishes with exit code 1
  state = finalizeQueueState(state, 1);

  assert.equal(state.items.get("ep1.mkv").status, "completed");
  assert.equal(state.items.get("ep2.mkv").status, "error");
  assert.equal(state.items.get("ep2.mkv").error, "Aborted (exit 1)");
  assert.equal(state.items.get("ep3.mkv").status, "error");
  assert.equal(state.items.get("ep3.mkv").error, "Aborted (exit 1)");
  assert.equal(state.errorCount, 2);
  assert.equal(state.batchCompleted, true);
  assert.equal(state.batchSuccess, false);
});

test("successful process exit does not hide missing completion events", () => {
  const state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

  const finalState = finalizeQueueState(state, 0);

  assert.equal(finalState.items.get("ep1.mkv").status, "error");
  assert.equal(finalState.batchSuccess, false);
});

test("all items completed without batch_complete event fails batch completion", () => {
  let state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

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
  assert.equal(state.batchSuccess, null);

  // Process exits 0, but no batch_complete was sent
  const finalState = finalizeQueueState(state, 0);
  assert.equal(finalState.batchCompleted, true);
  assert.equal(finalState.batchSuccess, false);
});

test("reduceQueueEvent evolves the single owned state object in place", () => {
  const state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });
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
  assert.equal(item.durationSec, "1.0");
});

test("reduceQueueEvent handles 10,000 start/complete pairs in linear time", () => {
  const total = 10000;
  const items = [];
  for (let i = 0; i < total; i++) {
    items.push({ id: `ep${i}.mkv`, name: `ep${i}.mkv` });
  }
  let state = createQueueState({ version: 2, items });
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total }, 0);

  const begin = performance.now();
  for (let i = 0; i < total; i++) {
    const id = `ep${i}.mkv`;
    state = reduceQueueEvent(state, { version: 1, type: "item_start", id }, i);
    state = reduceQueueEvent(state, {
      version: 1,
      type: "item_complete",
      id,
      status: "completed",
    }, i + 200);
  }
  const elapsedMs = performance.now() - begin;

  assert.equal(state.completedCount, total);
  assert.equal(state.errorCount, 0);
  // O(N) map cloning per event would push this into multi-second territory
  // even on slow hardware; a generous bound catches reintroduced O(N²).
  assert.ok(elapsedMs < 2000, `20k events took ${elapsedMs.toFixed(0)}ms (expected < 2000ms)`);
});

test("sub-second completions render as <0.1 instead of 0.0", () => {
  let state = createQueueState({
    version: 2,
    items: [{ id: "fast.mkv", name: "fast.mkv" }],
  });
  state = reduceQueueEvent(state, { version: 1, type: "batch_start", total: 1 }, 1000);
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "fast.mkv" }, 1050);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "fast.mkv",
    status: "completed",
  }, 1099);

  assert.equal(state.items.get("fast.mkv").durationSec, "<0.1");
});

test("skips and plans report no duration instead of elapsed batch time", () => {
  let state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }, { id: "ep2.mkv", name: "ep2.mkv" }],
  });
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

  // Same for dry-run planned outcomes.
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
    ["Season 01/ep1.mkv", "Season 01/better.ass"],
    ["Season 01/gone.mkv", "Season 01/deleted.ass"],
  ]);
  const manifest = {
    version: 2,
    items: [
      { id: "Season 01/ep1.mkv", name: "ep1.mkv" },
      { id: "Season 01/ep2.mkv", name: "ep2.mkv" },
    ],
  };

  const carried = carryOverAssignments(assignments, manifest);

  assert.deepEqual([...carried.entries()], [
    ["Season 01/ep1.mkv", "Season 01/better.ass"],
  ]);
  // Input map is not mutated.
  assert.equal(assignments.size, 2);
});

test("carryOverAssignments returns empty map for missing or itemless manifests", () => {
  const assignments = new Map([["ep1.mkv", "sub.ass"]]);

  assert.equal(carryOverAssignments(assignments, null).size, 0);
  assert.equal(carryOverAssignments(assignments, { version: 2, items: [] }).size, 0);
});

test("assignment payload excludes items removed by preflight inspection", () => {
  const assignments = new Map([
    ["kept.mkv", "kept.ass"],
    ["removed.mkv", "removed.ass"],
  ]);
  const manifest = { version: 2, items: [{ id: "kept.mkv" }] };

  assert.equal(
    serializeAssignmentsForManifest(assignments, manifest),
    JSON.stringify({ version: 1, assignments: { "kept.mkv": "kept.ass" } })
  );
});

test("reduceQueueEvent treats cancelled items as terminal and counts them as cancelled", () => {
  const manifest = {
    version: 2,
    items: [
      { id: "ep1.mkv", name: "ep1.mkv" },
      { id: "ep2.mkv", name: "ep2.mkv" },
    ],
  };
  let state = createQueueState(manifest);

  // Started then cancelled mid-run: reports how long it ran.
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep1.mkv" }, 1000);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "cancelled",
  }, 3500);
  assert.equal(state.items.get("ep1.mkv").status, "cancelled");
  assert.equal(state.items.get("ep1.mkv").durationSec, "2.5");
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
  let state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

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

test("finalizeQueueState marks unresolved items cancelled after a forced stop", () => {
  const state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

  const final = finalizeQueueState(state, -1, true);

  assert.equal(final.items.get("ep1.mkv").status, "cancelled");
  assert.equal(final.cancelledCount, 1);
  assert.equal(final.errorCount, 0);
  assert.equal(final.batchCancelled, true);
});

test("item transitioning directly from queued to error leaves durationSec as null instead of batchStartTime fallback", () => {
  let state = createQueueState({
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }, { id: "ep2.mkv", name: "ep2.mkv" }],
  });
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

  // ep2 starts and then errors: correctly calculates duration against its item_start time
  state = reduceQueueEvent(state, { version: 1, type: "item_start", id: "ep2.mkv" }, 7000);
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "error",
    error: "ffmpeg process crashed",
  }, 9500);

  assert.equal(state.items.get("ep2.mkv").status, "error");
  assert.equal(state.items.get("ep2.mkv").durationSec, "2.5");
  assert.equal(state.errorCount, 2);
});
