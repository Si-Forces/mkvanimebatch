import test from "node:test";
import assert from "node:assert/strict";
import {
  createQueueState,
  reconcileQueueSelection,
  reduceQueueEvent,
  finalizeQueueState,
  carryOverAssignments,
  serializeAssignmentsForManifest,
  buildTrackAssignmentsForManifest,
  createQueueStateFromSnapshot,
  formatOutcomeDuration,
  formatElapsedDuration,
  toggleItemSelection,
  selectAll,
  deselectAll,
  reorderItem,
  getSelectedCount,
  normalizeDirectoryPath,
  isSameDirectory,
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
  assert.equal(item.durationSec, 1);
  assert.equal(formatOutcomeDuration(item.durationSec), "1.0s");
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

  assert.equal(formatOutcomeDuration(state.items.get("fast.mkv").durationSec), "<0.1s");
  assert.equal(state.items.get("fast.mkv").durationSec, 0.049);
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
  assert.equal(state.items.get("ep1.mkv").durationSec, 2.5);
  assert.equal(formatOutcomeDuration(state.items.get("ep1.mkv").durationSec), "2.5s");
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
  assert.equal(state.items.get("ep2.mkv").durationSec, 2.5);
  assert.equal(formatOutcomeDuration(state.items.get("ep2.mkv").durationSec), "2.5s");
  assert.equal(state.errorCount, 2);
});

test("buildTrackAssignmentsForManifest builds track assignments from carried over manual assignments", () => {
  const manifest = {
    version: 2,
    items: [
      { id: "Season 1/ep1.mkv", name: "ep1.mkv" },
      { id: "Season 1/ep2.mkv", name: "ep2.mkv" },
    ],
  };
  const assignments = new Map([
    ["Season 1/ep1.mkv", "/subs/ep1.ass"],
    ["Season 1/ep2.mkv", "/subs/ep2.ass"],
  ]);

  const result = buildTrackAssignmentsForManifest(assignments, manifest);
  assert.deepEqual(result, [
    {
      target_video: "Season 1/ep1.mkv",
      asset_path: "/subs/ep1.ass",
      track_type: "subtitle",
    },
    {
      target_video: "Season 1/ep2.mkv",
      asset_path: "/subs/ep2.ass",
      track_type: "subtitle",
    },
  ]);
});

test("buildTrackAssignmentsForManifest returns null when assignments map is empty", () => {
  const manifest = {
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  };
  const assignments = new Map();

  const result = buildTrackAssignmentsForManifest(assignments, manifest);
  assert.equal(result, null);
});

test("buildTrackAssignmentsForManifest ignores assignments for items not in the manifest", () => {
  const manifest = {
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  };
  const assignments = new Map([
    ["ep1.mkv", "/subs/ep1.ass"],
    ["deleted_ep2.mkv", "/subs/ep2.ass"],
  ]);

  const result = buildTrackAssignmentsForManifest(assignments, manifest);
  assert.deepEqual(result, [
    {
      target_video: "ep1.mkv",
      asset_path: "/subs/ep1.ass",
      track_type: "subtitle",
    },
  ]);

  const orphanedAssignments = new Map([
    ["orphaned.mkv", "/subs/orphaned.ass"],
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
  assert.equal(state.version, 1);
  assert.equal(state.mode, "extract");
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

test("createQueueStateFromSnapshot handles empty or null snapshots", () => {
  const stateNull = createQueueStateFromSnapshot(null);
  assert.equal(stateNull.totalCount, 0);
  assert.equal(stateNull.items.size, 0);

  const stateEmpty = createQueueStateFromSnapshot({});
  assert.equal(stateEmpty.totalCount, 0);
});

test("createQueueStateFromSnapshot resolves in-flight processing and queued items to cancelled for cancelled snapshot", () => {
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
});

test("createQueueStateFromSnapshot resolves in-flight processing and queued items to error for error snapshot", () => {
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
  const state = createQueueStateFromSnapshot(snapshot, { now: fixedNow });
  assert.equal(state.batchCompleted, false);
  assert.equal(state.items.get("ep1.mkv").status, "processing");
  assert.equal(state.items.get("ep1.mkv").startTime, fixedNow);
  assert.equal(state.items.get("ep2.mkv").status, "queued");
  assert.equal(state.items.get("ep2.mkv").startTime, null);
});

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
  const state = createQueueStateFromSnapshot(snapshot, { now: 30000 });
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

test("formatOutcomeDuration formats null, existing formatted strings, and seconds thresholds", () => {
  assert.equal(formatOutcomeDuration(null), null);
  assert.equal(formatOutcomeDuration(undefined), null);

  // Return strings ending with 's' or containing 'm' as-is
  assert.equal(formatOutcomeDuration("4.2s"), "4.2s");
  assert.equal(formatOutcomeDuration("1m 15.4s"), "1m 15.4s");
  assert.equal(formatOutcomeDuration("2m 05s"), "2m 05s");
  assert.equal(formatOutcomeDuration("<0.1s"), "<0.1s");

  // Thresholds: < 0.1s, 4.2s, 75.4s -> 1m 15.4s
  assert.equal(formatOutcomeDuration(0), "<0.1s");
  assert.equal(formatOutcomeDuration(0.0), "<0.1s");
  assert.equal(formatOutcomeDuration(0.04), "<0.1s");
  assert.equal(formatOutcomeDuration("<0.1"), "<0.1s");
  assert.equal(formatOutcomeDuration(4.2), "4.2s");
  assert.equal(formatOutcomeDuration("4.2"), "4.2s");
  assert.equal(formatOutcomeDuration(59.94), "59.9s");
  assert.equal(formatOutcomeDuration(59.96), "1m 0.0s");
  assert.equal(formatOutcomeDuration(60.0), "1m 0.0s");
  assert.equal(formatOutcomeDuration(75.4), "1m 15.4s");
  assert.equal(formatOutcomeDuration(119.96), "2m 0.0s");
  assert.equal(formatOutcomeDuration("invalid"), null);
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
  const manifest = {
    version: 2,
    items: [
      { id: "ep1.mkv", name: "ep1.mkv" },
      { id: "ep2.mkv", name: "ep2.mkv" },
      { id: "ep3.mkv", name: "ep3.mkv" },
    ],
  };
  let state = createQueueState(manifest);

  // <0.1s
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
    duration_sec: 0.08,
  });
  assert.equal(state.items.get("ep1.mkv").durationSec, 0.08);
  assert.equal(formatOutcomeDuration(state.items.get("ep1.mkv").durationSec), "<0.1s");

  // 4.2s
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep2.mkv",
    status: "completed",
    duration_sec: 4.2,
  });
  assert.equal(state.items.get("ep2.mkv").durationSec, 4.2);
  assert.equal(formatOutcomeDuration(state.items.get("ep2.mkv").durationSec), "4.2s");

  // 75.4s -> 1m 15.4s
  state = reduceQueueEvent(state, {
    version: 1,
    type: "item_complete",
    id: "ep3.mkv",
    status: "completed",
    duration_sec: 75.4,
  });
  assert.equal(state.items.get("ep3.mkv").durationSec, 75.4);
  assert.equal(formatOutcomeDuration(state.items.get("ep3.mkv").durationSec), "1m 15.4s");
});

test("reduceQueueEvent clamps item_progress and preserves float precision", () => {
  const manifest = {
    version: 2,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  };
  let state = createQueueState(manifest);
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
  let state = createQueueState({ version: 2, items: [] });
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

test("item selection and reordering helpers", () => {
  const manifest = {
    version: 2,
    items: [
      { id: "e1", name: "Episode 1.mkv" },
      { id: "e2", name: "Episode 2.mkv" },
      { id: "e3", name: "Episode 3.mkv" },
    ],
  };

  const state = createQueueState(manifest);
  assert.equal(getSelectedCount(state), 3);
  assert.equal(state.items.get("e1").selected, true);

  // toggle selection
  toggleItemSelection(state, "e2");
  assert.equal(state.items.get("e2").selected, false);
  assert.equal(getSelectedCount(state), 2);

  toggleItemSelection(state, "e2", true);
  assert.equal(state.items.get("e2").selected, true);
  assert.equal(getSelectedCount(state), 3);

  // deselect all
  deselectAll(state);
  assert.equal(getSelectedCount(state), 0);
  assert.equal(state.items.get("e1").selected, false);
  assert.equal(state.items.get("e2").selected, false);
  assert.equal(state.items.get("e3").selected, false);

  // select all
  selectAll(state);
  assert.equal(getSelectedCount(state), 3);
  assert.equal(state.items.get("e1").selected, true);

  // reorder item
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
  const manifest = ids => ({ version: 2, items: ids.map(id => ({ id, name: id })) });
  const previous = createQueueState(manifest(["ep1", "ep2", "gone"]));
  toggleItemSelection(previous, "ep1", false);
  reorderItem(previous, "ep2", "up");

  const replacement = reconcileQueueSelection(createQueueState(manifest(["ep1", "new", "ep2"])), previous);
  assert.deepEqual(replacement.order, ["ep2", "ep1", "new"]);
  assert.deepEqual(replacement.order.map(id => replacement.items.get(id).selected), [true, false, false]);
  assert.equal(replacement.items.has("gone"), false);
  assert.deepEqual(previous.order, ["ep2", "ep1", "gone"]);
});
test("normalizeDirectoryPath and isSameDirectory handle canonical, relative, and Windows paths", () => {
  // Relative dot segments
  assert.equal(normalizeDirectoryPath("/anime/x/../show"), "/anime/show");
  assert.equal(normalizeDirectoryPath("/anime/./show/."), "/anime/show");
  assert.equal(isSameDirectory("/anime/show", "/anime/x/../show"), true);
  assert.equal(isSameDirectory("/anime/show/", "/anime/x/../show"), true);

  // Trailing slash differences
  assert.equal(isSameDirectory("/anime/show", "/anime/show/"), true);
  assert.equal(isSameDirectory("/anime/show///", "/anime/show"), true);

  // Windows drive letters and separators
  assert.equal(normalizeDirectoryPath("C:\\Anime\\Show"), "c:/anime/show");
  assert.equal(normalizeDirectoryPath("c:/anime/show/"), "c:/anime/show");
  assert.equal(isSameDirectory("C:\\Anime\\Show", "c:/anime/show/"), true);
  assert.equal(isSameDirectory("C:\\Anime\\x\\..\\Show", "c:/anime/show"), true);

  // Missing or empty identities
  assert.equal(isSameDirectory(null, "/anime/show"), false);
  assert.equal(isSameDirectory("/anime/show", null), false);
  assert.equal(isSameDirectory(undefined, undefined), false);
  assert.equal(isSameDirectory("", "/anime/show"), false);

  // Different directories
  assert.equal(isSameDirectory("/anime/showA", "/anime/showB"), false);
});

