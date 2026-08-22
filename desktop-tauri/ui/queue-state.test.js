import test from "node:test";
import assert from "node:assert/strict";
import {
  createQueueState,
  reduceQueueEvent,
  finalizeQueueState,
  carryOverAssignments,
} from "./queue-state.js";

test("createQueueState initializes state from manifest v1", () => {
  const manifest = {
    version: 1,
    items: [
      {
        id: "Season 01/Show - 01.mkv",
        name: "Show - 01.mkv",
        metadata: { resolution: "1080p", video_codec: "HEVC" },
      },
      {
        id: "Season 01/Show - 02.mkv",
        name: "Show - 02.mkv",
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
  assert.deepEqual(item1.metadata, { resolution: "1080p", video_codec: "HEVC" });
});

test("createQueueState correctly distinguishes duplicate basenames in different directories", () => {
  const manifest = {
    version: 1,
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
    version: 1,
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
    version: 1,
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
    version: 1,
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
    version: 1,
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
    version: 1,
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
    version: 1,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

  const finalState = finalizeQueueState(state, 0);

  assert.equal(finalState.items.get("ep1.mkv").status, "error");
  assert.equal(finalState.batchSuccess, false);
});

test("all items completed without batch_complete event fails batch completion", () => {
  let state = createQueueState({
    version: 1,
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

test("reduceQueueEvent is a pure reducer and does not mutate input state", () => {
  const initialState = createQueueState({
    version: 1,
    items: [{ id: "ep1.mkv", name: "ep1.mkv" }],
  });

  const nextState = reduceQueueEvent(initialState, {
    version: 1,
    type: "item_start",
    id: "ep1.mkv",
  }, 1000);

  assert.notEqual(initialState, nextState);
  assert.notEqual(initialState.items, nextState.items);
  assert.equal(initialState.items.get("ep1.mkv").status, "queued");
  assert.equal(nextState.items.get("ep1.mkv").status, "processing");

  const completedState = reduceQueueEvent(nextState, {
    version: 1,
    type: "item_complete",
    id: "ep1.mkv",
    status: "completed",
  }, 2000);

  assert.notEqual(nextState, completedState);
  assert.equal(nextState.completedCount, 0);
  assert.equal(completedState.completedCount, 1);
  assert.equal(completedState.items.get("ep1.mkv").durationSec, "1.0");
});

test("carryOverAssignments keeps selections for surviving items and drops stale ones", () => {
  const assignments = new Map([
    ["Season 01/ep1.mkv", "Season 01/better.ass"],
    ["Season 01/gone.mkv", "Season 01/deleted.ass"],
  ]);
  const manifest = {
    version: 1,
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
  assert.equal(carryOverAssignments(assignments, { version: 1, items: [] }).size, 0);
});


