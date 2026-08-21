/**
 * queue-state.js
 * Pure state reducer and sidecar event contract handler for MKV Anime Batch UI.
 */

function isTerminal(status) {
  return status === "completed" || status === "skipped" || status === "planned" || status === "error";
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
    completedCount: 0,
    totalCount: 0,
    skippedCount: 0,
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

export function reduceQueueEvent(state, event) {
  if (!state || !event || typeof event !== "object") {
    return state;
  }

  if (event.version !== 1) {
    return state;
  }

  switch (event.type) {
    case "batch_start": {
      if (!state.batchStartTime) {
        state.batchStartTime = Date.now();
      }
      if (typeof event.total === "number") {
        state.expectedTotal = event.total;
      }
      return state;
    }

    case "item_start": {
      if (!event.id) return state;
      const item = state.items.get(event.id);
      if (!item) return state;

      if (isTerminal(item.status)) {
        return state;
      }

      item.status = "processing";
      if (!item.startTime) {
        item.startTime = Date.now();
      }
      return state;
    }

    case "item_complete": {
      if (!event.id) return state;
      const item = state.items.get(event.id);
      if (!item) return state;

      if (isTerminal(item.status)) {
        return state;
      }

      let dur = null;
      if (item.startTime) {
        dur = ((Date.now() - item.startTime) / 1000).toFixed(1);
      } else if (state.batchStartTime) {
        dur = ((Date.now() - state.batchStartTime) / 1000).toFixed(1);
      }
      item.durationSec = dur;

      const status = event.status;
      if (status === "completed") {
        item.status = "completed";
        state.completedCount++;
      } else if (status === "skipped") {
        item.status = "skipped";
        item.reason = event.reason || "Skipped";
        state.skippedCount++;
      } else if (status === "planned") {
        item.status = "planned";
        state.completedCount++;
      } else if (status === "error") {
        item.status = "error";
        item.error = event.error || "Failed";
        state.errorCount++;
      } else {
        item.status = "error";
        item.error = `Unknown status: ${status}`;
        state.errorCount++;
      }
      return state;
    }

    case "batch_complete": {
      state.batchCompleted = true;
      state.batchSuccess = event.success === true;
      return state;
    }

    default:
      return state;
  }
}

export function finalizeQueueState(state, exitCode) {
  if (!state) return state;

  state.items.forEach((item) => {
    if (!isTerminal(item.status)) {
      item.status = "error";
      item.error =
        exitCode === 0
          ? "Protocol error: missing completion event"
          : `Aborted (exit ${exitCode})`;
      state.errorCount++;
    }
  });

  state.batchCompleted = true;
  state.batchSuccess = state.batchSuccess !== false && exitCode === 0 && state.errorCount === 0;

  return state;
}
