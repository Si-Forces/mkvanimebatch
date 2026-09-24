/**
 * queue-state.js
 * Queue state factory, event reducer, and Engine Bridge event stream handler
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
 * Format a duration outcome for display in cards and tags.
 *
 * @param {number|string|null|undefined} seconds
 * @returns {string|null}
 */
export function formatOutcomeDuration(seconds) {
  if (seconds == null) return null;
  if (typeof seconds === "string") {
    if (seconds.endsWith("s")) return seconds;
    if (seconds.startsWith("<")) return `${seconds}s`;
  }

  const num = typeof seconds === "number" ? seconds : parseFloat(seconds);
  if (Number.isNaN(num)) return null;

  const rawSec = Math.max(0, num);
  if (rawSec < 0.1) return "<0.1s";

  const rounded = Math.round(rawSec * 10) / 10;
  if (rounded < 60) {
    return `${rounded.toFixed(1)}s`;
  }

  const mins = Math.floor(rounded / 60);
  const secs = (rounded % 60).toFixed(1);
  return `${mins}m ${secs}s`;
}

/**
 * Format an elapsed seconds counter as an in-flight duration string (e.g. 5s, 1m 15s).
 *
 * @param {number} seconds
 * @returns {string}
 */
export function formatElapsedDuration(seconds) {
  let sec = Number(seconds);
  if (Number.isNaN(sec) || sec < 0) {
    sec = 0;
  }
  if (sec < 60) {
    return `${Math.floor(sec)}s`;
  }
  return `${Math.floor(sec / 60)}m ${Math.floor(sec % 60)}s`;
}
/**
 * Normalizes a directory path string for canonical identity comparison.
 * Trims whitespace, standardizes separators to '/', resolves '.' and '..' segments,
 * normalizes Windows drive letters, collapses redundant slashes, and strips trailing slashes (except root).
 * On Windows systems or paths with drive letters, normalizes casing.
 *
 * @param {string|null|undefined} path
 * @returns {string}
 */
export function normalizeDirectoryPath(path) {
  if (typeof path !== "string") return "";
  let clean = path.trim().replace(/\\/g, "/");
  if (!clean) return "";

  // Strip Windows extended-length prefix (\\?\ or //?/) if present
  clean = clean.replace(/^(\/\/|\?\/?)[?.]\//, "");

  // Normalize Windows drive letter (e.g. C: -> c:)
  const hasDrive = /^[a-zA-Z]:/.test(clean);
  let drivePrefix = "";
  if (hasDrive) {
    drivePrefix = clean.slice(0, 2).toLowerCase();
    clean = clean.slice(2);
  }

  const isAbsolute = clean.startsWith("/");
  const segments = clean.split("/").filter(s => s.length > 0 && s !== ".");
  const resolved = [];

  for (const seg of segments) {
    if (seg === "..") {
      if (resolved.length > 0 && resolved[resolved.length - 1] !== "..") {
        resolved.pop();
      } else if (!isAbsolute && !hasDrive) {
        resolved.push("..");
      }
    } else {
      resolved.push(seg);
    }
  }

  let result = (hasDrive ? drivePrefix : "") + (isAbsolute ? "/" : "") + resolved.join("/");
  if (!result && isAbsolute) result = "/";
  if (hasDrive && !resolved.length) result = drivePrefix + "/";

  // Case normalization for Windows: on Windows, filesystems are case-insensitive
  if (
    hasDrive ||
    (typeof navigator !== "undefined" &&
      (navigator.platform?.includes("Win") || navigator.userAgent?.includes("Win"))) ||
    (typeof process !== "undefined" && process.platform === "win32")
  ) {
    result = result.toLowerCase();
  }

  return result || "/";
}

/**
 * Returns true if both directory paths represent the same canonical directory.
 * An empty, null, or missing directory identity always returns false.
 *
 * @param {string|null|undefined} dirA
 * @param {string|null|undefined} dirB
 * @returns {boolean}
 */
export function isSameDirectory(dirA, dirB) {
  if (!dirA || !dirB) return false;
  const normA = normalizeDirectoryPath(dirA);
  const normB = normalizeDirectoryPath(dirB);
  return Boolean(normA && normB && normA === normB);
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
    inputDir: options.inputDir ? normalizeDirectoryPath(options.inputDir) : null,
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
      selected: true,
    };

    state.items.set(id, item);
    state.order.push(id);
    state.totalCount++;
  }

  return state;
}
/** Preserve surviving selection and order; append newly inspected IDs unselected. */
export function reconcileQueueSelection(state, previous) {
  const order = [];
  for (const id of previous.order) {
    const item = state.items.get(id);
    if (!item) continue;
    item.selected = previous.items.get(id)?.selected !== false;
    order.push(id);
  }
  for (const id of state.order) {
    if (previous.items.has(id)) continue;
    state.items.get(id).selected = false;
    order.push(id);
  }
  state.order = order;
  return state;
}

/**
 * Creates and initializes a queue state projection from an in-memory JobSnapshot.
 *
 * @param {Object} snapshot
 * @param {{mode?: string}} [options]
 */
export function createQueueStateFromSnapshot(snapshot, options = {}) {
  const mode = snapshot?.mode || options.mode || "remux";
  const state = {
    version: 1,
    mode,
    inputDir: snapshot?.input_dir || null,
    items: new Map(),
    order: [],
    batchStartTime: snapshot?.start_time_ms ?? null,
    batchCompleted: snapshot?.status === "completed" || snapshot?.status === "cancelled" || snapshot?.status === "error",
    batchSuccess: snapshot?.status === "completed",
    batchCancelled: snapshot?.status === "cancelled",
    completedCount: 0,
    totalCount: 0,
    skippedCount: 0,
    cancelledCount: 0,
    errorCount: 0,
  };

  if (!snapshot || !snapshot.items || !Array.isArray(snapshot.ordered_item_ids)) {
    return state;
  }

  for (const id of snapshot.ordered_item_ids) {
    const snapItem = snapshot.items[id];
    if (!snapItem || state.items.has(id)) continue;
    const name = String(snapItem.file || id);
    const status = snapItem.status || "queued";

    let resolvedStatus = status;
    if (state.batchCompleted && (status === "processing" || status === "queued")) {
      resolvedStatus = state.batchCancelled ? "cancelled" : "error";
    }

    const item = {
      id,
      name,
      release: "",
      metadata: null,
      inspectionError: null,
      status: resolvedStatus,
      startTime: resolvedStatus === "processing" ? (snapItem.start_time_ms ?? options.now ?? Date.now()) : null,
      durationSec: snapItem.duration_sec ?? null,
      progress: resolvedStatus === "processing" && typeof snapItem.progress === "number"
        ? Math.min(100, Math.max(0, snapItem.progress)) : null,
      reason: snapItem.reason ?? null,
      error: snapItem.error ?? null,
      selected: true,
    };

    state.items.set(id, item);
    state.order.push(id);
    state.totalCount++;

    if (resolvedStatus === "completed" || resolvedStatus === "planned") state.completedCount++;
    else if (resolvedStatus === "skipped") state.skippedCount++;
    else if (resolvedStatus === "cancelled") state.cancelledCount++;
    else if (resolvedStatus === "error") state.errorCount++;
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

export function buildTrackAssignmentsForManifest(assignments, manifest) {
  const carried = carryOverAssignments(assignments, manifest);
  if (carried.size === 0) return null;
  const list = [];
  for (const [targetVideo, val] of carried.entries()) {
    if (typeof val === "object" && val !== null) {
      list.push({
        target_video: targetVideo,
        asset_path: val.asset_path || val.path,
        track_type: val.track_type || "subtitle",
        language: val.language || null,
        title: val.title || null,
        offset_ms: typeof val.offset_ms === "number" ? val.offset_ms : null,
      });
    } else {
      list.push({
        target_video: targetVideo,
        asset_path: val,
        track_type: "subtitle",
      });
    }
  }
  return list;
}

/**
 * Advance queue state for one engine JobEvent. Mutates `state` in place and
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
      if (Array.isArray(event.items)) {
        for (const it of event.items) {
          if (!it || !it.id || state.items.has(it.id)) continue;
          state.items.set(it.id, {
            id: it.id,
            name: it.file || it.id,
            release: ".",
            metadata: null,
            inspectionError: null,
            status: "queued",
            startTime: null,
            durationSec: null,
            durationFormatted: null,
            progress: null,
            speed: null,
            eta: null,
            reason: null,
            error: null,
            outcome: null,
          });
          if (!state.order.includes(it.id)) {
            state.order.push(it.id);
          }
        }
        state.totalCount = state.items.size;
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
        item.progress = Math.min(100, Math.max(0, event.progress));
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
      let dur = null;
      if (event.duration_sec != null) {
        const d = Number(event.duration_sec);
        dur = Number.isNaN(d) ? null : Math.max(0, d);
      } else if (
        item.startTime !== null &&
        (outcome.status === "completed" ||
         outcome.status === "error" ||
         outcome.status === "cancelled")
      ) {
        dur = Math.max(0, (now - item.startTime) / 1000);
      }

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

/**
 * Toggles or sets item selection in queue state.
 */
export function toggleItemSelection(state, id, selected) {
  if (!state || !state.items) return;
  const item = state.items.get(id);
  if (!item) return;
  item.selected = typeof selected === "boolean" ? selected : !item.selected;
}

/**
 * Selects all items in queue state.
 */
export function selectAll(state) {
  if (!state || !state.items) return;
  for (const item of state.items.values()) {
    item.selected = true;
  }
}

/**
 * Deselects all items in queue state.
 */
export function deselectAll(state) {
  if (!state || !state.items) return;
  for (const item of state.items.values()) {
    item.selected = false;
  }
}

/**
 * Reorders an item within state.order earlier or later.
 */
export function reorderItem(state, id, direction) {
  if (!state || !Array.isArray(state.order)) return;
  const idx = state.order.indexOf(id);
  if (idx === -1) return;
  if (direction === "up" && idx > 0) {
    [state.order[idx - 1], state.order[idx]] = [state.order[idx], state.order[idx - 1]];
  } else if (direction === "down" && idx < state.order.length - 1) {
    [state.order[idx + 1], state.order[idx]] = [state.order[idx], state.order[idx + 1]];
  }
}

/**
 * Returns the count of selected items in state.
 */
export function getSelectedCount(state) {
  if (!state || !state.items) return 0;
  let count = 0;
  for (const item of state.items.values()) {
    if (item.selected !== false) count++;
  }
  return count;
}
