/**
 * queue-state.js
 * Queue state factories, the engine JobEvent reducer, and the selection and
 * order helpers behind the Queue Tracker.
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
 * @property {string=} input_dir
 * @property {{id: string, file: string}[]=} items
 * @property {number=} progress
 * @property {ItemOutcome=} status
 * @property {string=} reason
 * @property {string=} error
 * @property {number=} duration_sec
 * @property {boolean=} success
 * @property {boolean=} cancelled
 */

/** Terminal item status -> the state counter it increments. */
const OUTCOME_COUNTERS = new Map([
  ["completed", "completedCount"],
  ["planned", "completedCount"],
  ["skipped", "skippedCount"],
  ["cancelled", "cancelledCount"],
  ["error", "errorCount"],
]);

function isTerminal(status) {
  return OUTCOME_COUNTERS.has(status);
}

/**
 * The Engine Bridge's Item Settlement wording, for the guard below. A golden fixture pins both
 * constants to what the bridge sends: `ui/fixtures/settlement-events.json`, written by the Rust
 * test `test_settlement_wording_matches_the_golden_fixture` and asserted in `queue-state.test.js`.
 */
const CANCELLED_REASON = "Operation cancelled";
const BATCH_FAILED = "Batch failed";

function clampPercent(value) {
  return Math.min(100, Math.max(0, value));
}

function swap(list, i, j) {
  [list[i], list[j]] = [list[j], list[i]];
}

/**
 * Format a measured item duration for cards and tags (e.g. <0.1s, 4.2s, 1m 15.4s).
 *
 * @param {number} seconds
 * @returns {string}
 */
export function formatOutcomeDuration(seconds) {
  if (seconds < 0.1) return "<0.1s";
  const rounded = Math.round(seconds * 10) / 10;
  if (rounded < 60) return `${rounded.toFixed(1)}s`;
  return `${Math.floor(rounded / 60)}m ${(rounded % 60).toFixed(1)}s`;
}

/**
 * Format an in-flight elapsed time in whole seconds (e.g. 5s, 1m 15s).
 *
 * @param {number} seconds
 * @returns {string}
 */
export function formatElapsedDuration(seconds) {
  const sec = Math.max(0, seconds);
  if (sec < 60) return `${Math.floor(sec)}s`;
  return `${Math.floor(sec / 60)}m ${Math.floor(sec % 60)}s`;
}

/**
 * Normalizes a directory path string for canonical identity comparison.
 * Trims surrounding whitespace, resolves dot segments and redundant separators,
 * and strips trailing slashes (except root). Windows paths also normalize case
 * and trailing dots/spaces; verbatim `\\?\` paths keep them significant.
 *
 * @param {string|null|undefined} path
 * @returns {string}
 */
export function normalizeDirectoryPath(path) {
  if (typeof path !== "string") return "";
  const leadingTrimmed = path.trimStart();
  const hadExtendedPrefix =
    leadingTrimmed.startsWith("\\\\?\\") ||
    leadingTrimmed.startsWith("\\\\.\\") ||
    leadingTrimmed.startsWith("//?/") ||
    leadingTrimmed.startsWith("//./");
  let clean = hadExtendedPrefix ? leadingTrimmed : path.trim();
  if (!clean) return "";

  // A backslash is a legal POSIX filename byte, so translating it unconditionally
  // collapses distinct directories. Reuse the OS class from os-detect.js; only
  // normalize separators on Windows or Windows-shaped paths.
  const windowsHost =
    (typeof document !== "undefined" &&
      document.documentElement?.classList?.contains("is-windows")) ||
    (typeof process !== "undefined" && process.platform === "win32");
  const windowsPath =
    /^\\\\/.test(clean) || /^[a-zA-Z]:/.test(clean) || /^\/\/[?.][\\/]/.test(clean);
  if (windowsHost || windowsPath) {
    clean = clean.replace(/\\/g, "/");
  }

  // Strip the Windows verbatim/device prefix (`//?/` or `//./`) if present
  clean = clean.replace(/^\/\/[?.]\//, "");
  if (hadExtendedPrefix) clean = clean.replace(/^UNC\//i, "//");

  // Normalize Windows drive letter (e.g. C: -> c:)
  const hasDrive = /^[a-zA-Z]:/.test(clean);
  let drivePrefix = "";
  if (hasDrive) {
    drivePrefix = clean.slice(0, 2).toLowerCase();
    clean = clean.slice(2);
  }

  const isUnc = !hasDrive && /^\/\/[^/]/.test(clean);
  const isAbsolute = clean.startsWith("/");
  const trimWindowsSegments = (windowsHost || windowsPath || isUnc || hasDrive) && !hadExtendedPrefix;
  const segments = clean.split("/").filter(s => s.length > 0);
  const resolved = [];

  for (const rawSegment of segments) {
    const seg = trimWindowsSegments && rawSegment !== ".."
      ? rawSegment.replace(/[ .]+$/g, "")
      : rawSegment;
    if (!seg || seg === ".") continue;
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

  let result = drivePrefix + (isUnc ? "//" : isAbsolute ? "/" : "") + resolved.join("/");
  if (!result && isAbsolute) result = "/";
  if (hasDrive && !resolved.length) result = drivePrefix + "/";

  // Windows filesystems are case-insensitive.
  if (windowsHost || isUnc || hasDrive) {
    result = result.toLowerCase();
  }

  return result || ".";
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
  return normA !== "" && normA === normalizeDirectoryPath(dirB);
}

function emptyQueueState() {
  return {
    items: new Map(),
    order: [],
    knownOrder: [],
    batchStartTime: null,
    batchItemIds: null,
    vanishedSelectedIds: [],
    batchCompleted: false,
    batchSuccess: null,
    batchCancelled: false,
    completedCount: 0,
    totalCount: 0,
    skippedCount: 0,
    cancelledCount: 0,
    errorCount: 0,
  };
}

function createItem(id, name, fields) {
  return {
    id,
    name,
    release: ".",
    metadata: null,
    inspectionError: null,
    status: "queued", // "queued" | "processing" | a terminal ItemOutcome
    startTime: null,
    durationSec: null,
    progress: null,
    reason: null,
    error: null,
    selected: true,
    ...fields,
  };
}

/** @param {InspectionManifest} manifest */
export function createQueueState(manifest) {
  const state = emptyQueueState();
  for (const entry of manifest.items) {
    if (!entry || !entry.id) continue;
    const id = String(entry.id);
    state.items.set(id, createItem(id, String(entry.name || id), {
      release: typeof entry.release === "string" ? entry.release : ".",
      metadata: entry.metadata || null,
      inspectionError: entry.inspection_error || null,
    }));
    state.order.push(id);
    state.totalCount++;
  }
  state.knownOrder = [...state.order];
  return state;
}

/** Preserve surviving selection and order, remember vanished selected IDs, and append newly inspected IDs unselected. */
export function reconcileQueueSelection(state, previous) {
  const priorVanished = new Set(previous.vanishedSelectedIds);
  const vanished = new Set([...priorVanished].filter(id => !state.items.has(id)));
  const baseSet = new Set(previous.knownOrder);
  const order = [];
  const knownOrder = [];

  for (const id of previous.knownOrder) {
    knownOrder.push(id);
    const prevItem = previous.items.get(id);
    const wasSelected = prevItem ? prevItem.selected !== false : priorVanished.has(id);
    const item = state.items.get(id);
    if (!item) {
      if (wasSelected) vanished.add(id);
      continue;
    }
    item.selected = wasSelected;
    order.push(id);
  }

  for (const id of state.order) {
    if (baseSet.has(id)) continue;
    state.items.get(id).selected = priorVanished.has(id);
    order.push(id);
    knownOrder.push(id);
  }

  state.order = order;
  state.knownOrder = knownOrder;
  state.vanishedSelectedIds = [...vanished];
  return state;
}

/**
 * Project an Engine Bridge JobSnapshot into queue state. The bridge settles every item of a
 * finished batch, so the Item Settlement guard only acts on a snapshot that broke that protocol.
 *
 * @param {Object} snapshot
 * @param {number} [now] start time for processing items the snapshot has none for
 */
export function createQueueStateFromSnapshot(snapshot, now = Date.now()) {
  const state = emptyQueueState();
  state.batchStartTime = snapshot.start_time_ms ?? null;
  state.batchCompleted = snapshot.status === "completed" || snapshot.status === "cancelled" || snapshot.status === "error";
  state.batchSuccess = snapshot.status === "completed";
  state.batchCancelled = snapshot.status === "cancelled";

  for (const id of snapshot.ordered_item_ids) {
    const snapItem = snapshot.items[id];
    if (!snapItem || state.items.has(id)) continue;

    const status = snapItem.status || "queued";
    const processing = status === "processing";

    state.items.set(id, createItem(id, String(snapItem.file || id), {
      release: "",
      status,
      startTime: processing ? (snapItem.start_time_ms ?? now) : null,
      durationSec: snapItem.duration_sec ?? null,
      progress: processing && typeof snapItem.progress === "number" ? clampPercent(snapItem.progress) : null,
      reason: snapItem.reason ?? null,
      error: snapItem.error ?? null,
    }));
    state.order.push(id);
    state.totalCount++;

    const counter = OUTCOME_COUNTERS.get(status);
    if (counter) state[counter]++;
  }

  state.knownOrder = [...state.order];
  // A snapshot holds exactly the participants of its Active Batch.
  state.batchItemIds = [...state.order];
  if (state.batchCompleted) guardUnsettledItems(state, state.batchCancelled);
  return state;
}

/**
 * Keep only the manual assignments whose item id still exists in the given
 * manifest, so a rescan does not silently drop selections for surviving items.
 */
export function carryOverAssignments(previousAssignments, manifest) {
  const carried = new Map();
  for (const entry of manifest?.items ?? []) {
    if (previousAssignments.has(entry.id)) {
      carried.set(entry.id, previousAssignments.get(entry.id));
    }
  }
  return carried;
}

/**
 * Build the batch request's Track Assignment list from the manual assignments
 * that survive in `manifest`. Returns null when nothing survives.
 */
export function buildTrackAssignmentsForManifest(assignments, manifest) {
  const carried = carryOverAssignments(assignments, manifest);
  if (carried.size === 0) return null;
  return [...carried].map(([targetVideo, val]) => ({
    target_video: targetVideo,
    asset_path: val.asset_path,
    track_type: val.track_type || "subtitle",
    language: val.language || null,
    title: val.title || null,
    offset_ms: typeof val.offset_ms === "number" ? val.offset_ms : null,
  }));
}

/**
 * Advance queue state for one engine JobEvent. Mutates `state` in place and
 * returns it (see ownership contract in the module header).
 *
 * @param {JobEvent} event
 */
export function reduceQueueEvent(state, event, now = Date.now()) {
  if (event?.version !== 1) return state;

  switch (event.type) {
    case "batch_start": {
      state.batchStartTime ||= now;
      if (Array.isArray(event.items)) {
        state.batchItemIds = [];
        // Append participants missing from either list. knownOrder also keeps
        // vanished IDs that order omits, so each list gets its own membership set.
        const inOrder = new Set(state.order);
        const inKnown = new Set(state.knownOrder);
        for (const it of event.items) {
          if (!it?.id) continue;
          state.batchItemIds.push(it.id);
          if (!state.items.has(it.id)) {
            state.items.set(it.id, createItem(it.id, it.file || it.id));
          }
          if (!inOrder.has(it.id)) {
            state.order.push(it.id);
            inOrder.add(it.id);
          }
          if (!inKnown.has(it.id)) {
            state.knownOrder.push(it.id);
            inKnown.add(it.id);
          }
        }
        state.totalCount = state.items.size;
      }
      return state;
    }

    case "item_start": {
      const item = state.items.get(event.id);
      if (!item || isTerminal(item.status)) return state;
      item.status = "processing";
      item.startTime ||= now;
      return state;
    }

    case "item_progress": {
      const item = state.items.get(event.id);
      if (!item || isTerminal(item.status)) return state;
      if (typeof event.progress === "number") {
        item.progress = clampPercent(event.progress);
      }
      return state;
    }

    case "item_complete": {
      const item = state.items.get(event.id);
      if (!item || isTerminal(item.status)) return state;

      const known = OUTCOME_COUNTERS.has(event.status);
      const status = known ? event.status : "error";

      // Only measured work gets a duration: skips, dry-run plans, and items
      // that end without an item_start would otherwise report batch-elapsed
      // time. Items that had started report how long they ran.
      let durationSec = null;
      if (event.duration_sec != null) {
        const d = Number(event.duration_sec);
        durationSec = Number.isNaN(d) ? null : Math.max(0, d);
      } else if (
        item.startTime !== null &&
        (status === "completed" || status === "error" || status === "cancelled")
      ) {
        durationSec = Math.max(0, (now - item.startTime) / 1000);
      }

      item.status = status;
      item.durationSec = durationSec;
      item.reason = null;
      item.error = null;
      if (status === "skipped") {
        item.reason = event.reason || "Skipped";
      } else if (status === "cancelled") {
        item.reason = event.reason || "Cancelled";
      } else if (status === "error") {
        item.error = known ? event.error || "Failed" : `Unknown status: ${event.status}`;
      }
      state[OUTCOME_COUNTERS.get(status)]++;
      return state;
    }

    case "batch_complete": {
      state.batchCompleted = true;
      // The bridge settles items before this event, so an error count is one the engine's own
      // `success` does not cover (a success with open items): the snapshot status is `error` then.
      state.batchSuccess = event.success === true && state.errorCount === 0;
      state.batchCancelled = event.cancelled === true;
      return state;
    }

    default:
      return state;
  }
}

/**
 * The Item Settlement guard, for a finished job (live on `job-done`, or a restored snapshot).
 * The Engine Bridge gives every item of the Active Batch a terminal outcome before it ends the
 * job, so a participant still open here means the run broke that protocol. It gets the bridge's
 * wording (cancelled when `cancelled`, else an error), the batch is no success, and one warning
 * names the items. Only participants of a started batch (`batchItemIds`) are touched: a job
 * that failed before `batch_start` leaves the queue as it was. Mutates `state` in place and
 * returns it (see the ownership contract in the module header).
 *
 * @param {boolean} cancelled whether the job was stopped
 */
export function guardUnsettledItems(state, cancelled) {
  const open = (state.batchItemIds ?? []).filter(id => {
    const item = state.items.get(id);
    return item && !isTerminal(item.status);
  });
  if (open.length === 0) return state;

  console.warn(
    `The run broke Item Settlement: ${open.length} item(s) were left open (${open.join(", ")}); settling them as ${cancelled ? "cancelled" : "error"}.`
  );
  for (const id of open) {
    const item = state.items.get(id);
    item.status = cancelled ? "cancelled" : "error";
    item.reason = cancelled ? CANCELLED_REASON : null;
    item.error = cancelled ? null : BATCH_FAILED;
    state[OUTCOME_COUNTERS.get(item.status)]++;
  }
  state.batchSuccess = false;
  return state;
}

/** Set one item's selection; an explicit choice forgets remembered vanished selections. */
export function setItemSelected(state, id, selected) {
  const item = state.items.get(id);
  if (!item) return;
  item.selected = selected;
  state.vanishedSelectedIds = [];
}

/** Set every item's selection; forgets remembered vanished selections. */
export function setAllSelected(state, selected) {
  for (const item of state.items.values()) {
    item.selected = selected;
  }
  state.vanishedSelectedIds = [];
}

/** Swap an item with its neighbour in `order` ("up" = earlier) and mirror the swap in `knownOrder`. */
export function reorderItem(state, id, direction) {
  const idx = state.order.indexOf(id);
  if (idx === -1) return;
  const targetIdx = direction === "up" ? idx - 1 : idx + 1;
  if (targetIdx < 0 || targetIdx >= state.order.length) return;
  const otherId = state.order[targetIdx];
  swap(state.order, idx, targetIdx);
  const kIdx = state.knownOrder.indexOf(id);
  const kTarget = state.knownOrder.indexOf(otherId);
  if (kIdx !== -1 && kTarget !== -1) {
    swap(state.knownOrder, kIdx, kTarget);
  }
}

export function getSelectedCount(state) {
  let count = 0;
  for (const item of state.items.values()) {
    if (item.selected !== false) count++;
  }
  return count;
}
