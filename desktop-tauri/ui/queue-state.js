/**
 * queue-state.js
 * Queue state factory, event reducer, and sidecar event contract handler
 * for MKV Anime Batch UI.
 *
 * Ownership contract: the state object returned by createQueueState is
 * exclusively owned by a single Queue Tracker. reduceQueueEvent advances
 * it by mutating the owned item records and counters in place (O(1) per
 * event) and returns the same state object. It is NOT persistent
 * immutable data — callers must not retain pre-event snapshots.
 */

/**
 * @typedef {Object} InspectionMetadata
 * @property {string|null} resolution
 * @property {string|null} video_codec
 * @property {string|null} video_tag
 * @property {string|null} audio_summary
 * @property {boolean|null} has_flac
 * @property {string|null} sub_summary
 * @property {number|null} fonts_count
 */

/**
 * @typedef {Object} ManifestItem
 * @property {string} id
 * @property {string} name
 * @property {string} release
 * @property {InspectionMetadata} metadata
 * @property {string=} inspection_error
 */

/** @typedef {{version: number, subtitle_candidates?: Record<string, string[]>, items: ManifestItem[]}} InspectionManifest */
/** @typedef {"completed"|"skipped"|"planned"|"error"|"cancelled"} ItemOutcome */
/**
 * @typedef {Object} JobEvent
 * @property {number} version
 * @property {string} type
 * @property {string=} id
 * @property {string=} file
 * @property {ItemOutcome=} status
 * @property {string=} reason
 * @property {string=} error
 * @property {number=} duration_sec
 * @property {boolean=} success
 * @property {boolean=} cancelled
 */

function isTerminal(status) {
  return status === "completed" || status === "skipped" || status === "planned" || status === "error" || status === "cancelled";
}

/**
 * @param {InspectionManifest} manifest
 * @param {{mode?: string}} [options]
 */
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

  if (!manifest || manifest.version !== 2 || !Array.isArray(manifest.items)) {
    return state;
  }

  for (const entry of manifest.items) {
    if (!entry || !entry.id) continue;
    const id = String(entry.id);
    const name = String(entry.name || id);
    const release = typeof entry.release === "string" ? entry.release : ".";
    const metadata = entry.metadata || null;

    const item = {
      id,
      name,
      release,
      metadata,
      inspectionError: entry.inspection_error || null,
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

/**
 * Advance queue state for one sidecar event. Mutates `state` in place and
 * returns it (see ownership contract in the module header).
 *
 * @param {JobEvent} event
 */
export function reduceQueueEvent(state, event, now = Date.now()) {
  if (!state || !event || typeof event !== "object" || event.version !== 1) {
    return state;
  }

  switch (event.type) {
    case "batch_start": {
      state.batchStartTime = state.batchStartTime || now;
      if (typeof event.total === "number") {
        state.expectedTotal = event.total;
      }
      return state;
    }

    case "item_start": {
      if (!event.id || !state.items.has(event.id)) return state;
      const item = state.items.get(event.id);
      if (isTerminal(item.status)) return state;

      item.status = "processing";
      item.startTime = item.startTime || now;
      return state;
    }

    case "item_progress": {
      if (!event.id || !state.items.has(event.id)) return state;
      const item = state.items.get(event.id);
      if (isTerminal(item.status)) return state;

      if (typeof event.progress === "number") {
        item.progress = event.progress;
      }
      if (typeof event.speed === "string") {
        item.speed = event.speed;
      }
      if (typeof event.eta === "string") {
        item.eta = event.eta;
      }
      return state;
    }

    case "item_complete": {
      if (!event.id || !state.items.has(event.id)) return state;
      const item = state.items.get(event.id);
      if (isTerminal(item.status)) return state;

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

      // Only measured work gets a duration: skips, dry-run plans, and
      // items that error before starting have no item_start, so falling
      // back to batch-elapsed time would report a meaningless number.
      // Items that had started still report how long they ran. Sub-second
      // jobs render as "<0.1" so fast extractions don't display as "0.0s".
      const timed =
        item.startTime !== null &&
        (outcome.status === "completed" ||
         outcome.status === "error" ||
         outcome.status === "cancelled");
      const elapsedMs = timed ? now - item.startTime : null;
      const dur =
        elapsedMs === null ? null
        : elapsedMs < 100 ? "<0.1"
        : (elapsedMs / 1000).toFixed(1);

      item.durationSec = dur;
      item.reason = null;
      item.error = null;
      Object.assign(item, itemPatch);

      state.completedCount += completed;
      state.skippedCount += skipped;
      state.cancelledCount = (state.cancelledCount || 0) + cancelledCount;
      state.errorCount += failed;
      return state;
    }

    case "batch_complete": {
      state.batchCompleted = true;
      state.batchSuccess = event.success === true;
      state.batchCancelled = event.cancelled === true;
      return state;
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
    batchCancelled: state.batchCancelled || cancelled,
    batchSuccess: finalBatchSuccess,
  };
}
