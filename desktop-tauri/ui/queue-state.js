/**
 * queue-state.js
 * Pure state reducer and sidecar event contract handler for MKV Anime Batch UI.
 */

function isTerminal(status) {
  return status === "completed" || status === "skipped" || status === "planned" || status === "error" || status === "cancelled";
}

export function createQueueState(manifest, options = {}) {
  const mode = options.mode || "remux";

  const state = {
    version: 1,
    mode,
    items: new Map(),
    order: [],
    batchStartTime: null,
    batchCompleted: false,
    batchSuccess: null,
    batchCancelled: false,
    completedCount: 0,
    totalCount: 0,
    skippedCount: 0,
    cancelledCount: 0,
    errorCount: 0,
  };

  if (!manifest || manifest.version !== 1 || !Array.isArray(manifest.items)) {
    return state;
  }

  for (const entry of manifest.items) {
    if (!entry || !entry.id) continue;
    const id = String(entry.id);
    const name = String(entry.name || id);
    const metadata = entry.metadata || null;

    const item = {
      id,
      name,
      metadata,
      status: "queued", // 'queued' | 'processing' | 'completed' | 'skipped' | 'planned' | 'error'
      startTime: null,
      durationSec: null,
      reason: null,
      error: null,
    };

    state.items.set(id, item);
    state.order.push(id);
    state.totalCount++;
  }

  return state;
}

/**
 * Keep only the manual assignments whose item id still exists in the given
 * manifest, so a rescan does not silently drop selections for surviving items.
 */
export function carryOverAssignments(previousAssignments, manifest) {
  const carried = new Map();
  for (const entry of manifest?.items ?? []) {
    if (entry && previousAssignments.has(entry.id)) {
      carried.set(entry.id, previousAssignments.get(entry.id));
    }
  }
  return carried;
}

export function serializeAssignmentsForManifest(assignments, manifest) {
  const carried = carryOverAssignments(assignments, manifest);
  return carried.size > 0
    ? JSON.stringify({ version: 1, assignments: Object.fromEntries(carried) })
    : null;
}

export function reduceQueueEvent(state, event, now = Date.now()) {
  if (!state || !event || typeof event !== "object" || event.version !== 1) {
    return state;
  }

  switch (event.type) {
    case "batch_start": {
      return {
        ...state,
        batchStartTime: state.batchStartTime || now,
        expectedTotal: typeof event.total === "number" ? event.total : state.expectedTotal,
      };
    }

    case "item_start": {
      if (!event.id || !state.items.has(event.id)) return state;
      const prevItem = state.items.get(event.id);
      if (isTerminal(prevItem.status)) return state;

      const nextItems = new Map(state.items);
      nextItems.set(event.id, {
        ...prevItem,
        status: "processing",
        startTime: prevItem.startTime || now,
      });

      return {
        ...state,
        items: nextItems,
      };
    }

    case "item_complete": {
      if (!event.id || !state.items.has(event.id)) return state;
      const prevItem = state.items.get(event.id);
      if (isTerminal(prevItem.status)) return state;

      // Completion status -> item patch plus which counters it increments.
      const outcomes = {
        completed: { status: "completed", completed: 1 },
        planned: { status: "planned", completed: 1 },
        skipped: { status: "skipped", reason: event.reason || "Skipped", skipped: 1 },
        cancelled: { status: "cancelled", reason: event.reason || "Cancelled", cancelledCount: 1 },
        error: { status: "error", error: event.error || "Failed", failed: 1 },
      };
      const outcome = outcomes[event.status] ?? {
        status: "error",
        error: `Unknown status: ${event.status}`,
        failed: 1,
      };
      const { completed = 0, skipped = 0, cancelledCount = 0, failed = 0, ...itemPatch } = outcome;

      // Only measured work gets a duration: skips and dry-run plans are
      // decided during planning without an item_start, so falling back to
      // batch-elapsed time would report a meaningless number. Cancelled
      // items that had started still report how long they ran. Sub-second
      // jobs render as "<0.1" so fast extractions don't display as "0.0s".
      const timed =
        outcome.status === "completed" ||
        outcome.status === "error" ||
        (outcome.status === "cancelled" && prevItem.startTime !== null);
      const startTime = prevItem.startTime || state.batchStartTime;
      const elapsedMs = timed && startTime ? now - startTime : null;
      const dur =
        elapsedMs === null ? null
        : elapsedMs < 100 ? "<0.1"
        : (elapsedMs / 1000).toFixed(1);

      const nextItems = new Map(state.items);
      nextItems.set(event.id, {
        ...prevItem,
        durationSec: dur,
        reason: null,
        error: null,
        ...itemPatch,
      });

      return {
        ...state,
        items: nextItems,
        completedCount: state.completedCount + completed,
        skippedCount: state.skippedCount + skipped,
        cancelledCount: (state.cancelledCount || 0) + cancelledCount,
        errorCount: state.errorCount + failed,
      };
    }

    case "batch_complete": {
      return {
        ...state,
        batchCompleted: true,
        batchSuccess: event.success === true,
        batchCancelled: event.cancelled === true,
      };
    }

    default:
      return state;
  }
}

export function finalizeQueueState(state, exitCode, cancelled = false) {
  if (!state) return state;

  let nextItems = state.items;
  let addedErrors = 0;
  let addedCancelled = 0;

  for (const [id, item] of state.items.entries()) {
    if (!isTerminal(item.status)) {
      if (nextItems === state.items) {
        nextItems = new Map(state.items);
      }
      nextItems.set(id, {
        ...item,
        status: cancelled ? "cancelled" : "error",
        reason: cancelled ? "Cancelled" : null,
        error: cancelled
          ? null
          : exitCode === 0
            ? "Protocol error: missing completion event"
            : `Aborted (exit ${exitCode})`,
      });
      if (cancelled) addedCancelled++;
      else addedErrors++;
    }
  }

  const finalErrorCount = state.errorCount + addedErrors;
  const receivedBatchComplete = state.batchSuccess === true;
  const finalBatchSuccess = receivedBatchComplete && exitCode === 0 && finalErrorCount === 0;

  return {
    ...state,
    items: nextItems,
    errorCount: finalErrorCount,
    cancelledCount: state.cancelledCount + addedCancelled,
    batchCompleted: true,
    batchSuccess: finalBatchSuccess,
  };
}
