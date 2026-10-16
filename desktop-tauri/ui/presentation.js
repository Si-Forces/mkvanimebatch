/**
 * DOM-free presentation rules of the Library + Inspector layout: Finder-style paths, row status
 * text and color, the list filters, the bottom bar's progress, the subtitle precedence ladder
 * and the telemetry sparklines. Nothing here decides what the engine does.
 */

/**
 * Returns true if both Directory Identities name the same directory. The Engine Bridge
 * produces every identity in display-canonical form, so the UI compares them byte for byte
 * and never normalizes a path itself. An empty, null, or missing identity is never the same.
 *
 * @param {string|null|undefined} dirA
 * @param {string|null|undefined} dirB
 * @returns {boolean}
 */
export function isSameDirectory(dirA, dirB) {
  return Boolean(dirA) && dirA === dirB;
}

/**
 * Whether a start or a job holds the batch in the published View State `state`, which locks both
 * queues and both Settings Drafts: the Desktop Core's own rule (`busy` in
 * `crates/mkv-desktop-core/src/actor.rs`; `busy` in `fake-core.js` mirrors it too). False before
 * the first View State.
 *
 * @param {object|null|undefined} state
 * @returns {boolean}
 */
export function isBusy(state) {
  return (state?.run?.phase ?? "idle") !== "idle" || Boolean(state?.job?.active);
}

/**
 * A path's segments as the Finder path bar shows them: split on `/` or `\`, with empty, `.`
 * and `..` segments left out. Display only: never compare or send the result.
 *
 * @param {string|null|undefined} path
 * @returns {string[]}
 */
export function pathSegments(path) {
  return String(path ?? "").split(/[\\/]+/).filter(segment => segment && segment !== "." && segment !== "..");
}

/**
 * The folder crumbs of an Inspector episode: its Item ID minus the file name, which the
 * Inspector title already shows. None for an episode at the top of the input folder.
 *
 * @param {string} id
 * @returns {string[]}
 */
export function inspectorCrumbs(id) {
  return pathSegments(id).slice(0, -1);
}

/** A path as one line of Finder-style text: `media › anime › Show`. */
export function prettyPath(path) {
  return pathSegments(path).join(" › ");
}

/**
 * A log line as the drawer shows it: a path's leading separator is dropped and every other one
 * becomes ` › `. A run of separators counts as one, and a run between two digits stays as written,
 * so `3/12` and dates read as they were. Copy uses the raw line.
 *
 * Known limit: this is a regex over the whole line, not a path parser, so a folder named `2`
 * followed by `01.mkv` keeps its slash, and a codec ID such as `S_TEXT/ASS` gets a chevron. Parse
 * paths out of structured log fields if that ever matters.
 *
 * @param {string} line
 * @returns {string}
 */
export function logDisplayText(line) {
  // No lookbehind: WebKitGTK before 2.40 cannot parse one, and that SyntaxError would stop the
  // whole module graph from loading, leaving an empty window.
  const digit = ch => ch !== undefined && ch >= "0" && ch <= "9";
  return String(line)
    .replace(/(^|\s)[\\/]+(?=[^\s\\/])/g, "$1")
    .replace(/[\\/]+/g, (sep, at, text) => (digit(text[at - 1]) && digit(text[at + sep.length]) ? sep : " › "));
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

/** The color token of an item status: running is blue, done green, failed red, the rest muted. */
const STATUS_TONES = { processing: "info", completed: "ok", error: "err" };

/** @returns {"info"|"ok"|"err"|"mute"} */
export function statusTone(status) {
  return STATUS_TONES[status] ?? "mute";
}

/** The status column text of an episode row and the inspector's status pill. */
export function statusLabel(item, now = Date.now()) {
  switch (item.status) {
    case "processing": {
      const elapsed = formatElapsedDuration((now - (item.startTime ?? now)) / 1000);
      return typeof item.progress === "number" ? `${Math.round(item.progress)}% · ${elapsed}` : `Processing · ${elapsed}`;
    }
    case "completed":
      return item.durationSec != null ? `Done in ${formatOutcomeDuration(item.durationSec)}` : "Done";
    case "skipped":
      return "Skipped";
    case "cancelled":
      return "Cancelled";
    case "planned":
      return "Planned";
    case "error":
      return "Failed";
    default:
      return "Queued";
  }
}

/** The episode list filters, by chip key. An unselected item matches only "all". */
const FILTERS = {
  all: () => true,
  todo: item => item.selected !== false && ["queued", "processing", "cancelled"].includes(item.status),
  done: item => item.selected !== false && ["completed", "skipped", "planned"].includes(item.status),
  failed: item => item.selected !== false && item.status === "error",
};

const FILTER_KEYS = Object.keys(FILTERS);

export function matchesFilter(item, filter) {
  return (FILTERS[filter] ?? FILTERS.all)(item);
}

/** How many items of `state` each filter chip matches, keyed by filter. */
export function filterCounts(state) {
  const counts = Object.fromEntries(FILTER_KEYS.map(key => [key, 0]));
  for (const item of state.items.values()) {
    for (const key of FILTER_KEYS) if (FILTERS[key](item)) counts[key]++;
  }
  return counts;
}

/**
 * The stacked progress bar's four widths, in percent of the batch: the Active Batch once it
 * started, else the Queue Selection. A processing item adds its own progress to `live`, or a
 * sliver while it has none.
 */
export function progressSegments(state) {
  const ids = state.batchItemIds ?? state.order.filter(id => state.items.get(id)?.selected !== false);
  const parts = { done: 0, skipped: 0, failed: 0, live: 0 };
  for (const id of ids) {
    const item = state.items.get(id);
    if (!item) continue;
    if (item.status === "completed" || item.status === "planned") parts.done++;
    else if (item.status === "skipped" || item.status === "cancelled") parts.skipped++;
    else if (item.status === "error") parts.failed++;
    else if (item.status === "processing") parts.live += typeof item.progress === "number" ? item.progress / 100 : 0.05;
  }
  const total = Math.max(ids.length, 1);
  for (const key of Object.keys(parts)) parts[key] = Math.min(100, (parts[key] / total) * 100);
  return parts;
}

/** The bottom bar's clock: how long the batch has run, or how it ended. */
export function batchClock(state, now = Date.now()) {
  if (state.batchStartTime === null) return "Not started";
  if (state.batchCompleted) {
    if (state.batchCancelled) return "Stopped";
    return state.batchSuccess ? "Finished" : "Finished with errors";
  }
  return `${formatElapsedDuration((now - state.batchStartTime) / 1000)} elapsed`;
}

/** The audio a Remux run writes for an item's inspected tracks. */
export function remuxAudioText(meta, bitrate) {
  if (!meta?.audio_summary) return "No audio";
  if (!meta.has_flac) return `${meta.audio_summary} · passthrough`;
  return `${meta.audio_summary} → ${bitrate ? `Opus ${bitrate}` : "Opus"}`;
}

const RUNGS = [
  ["manual", "Manual assignment", "Manual subs"],
  ["same", "Same folder", "Same folder subs"],
  ["embedded", "Embedded track", "Embedded subs"],
  ["ancestor", "Parent folder", "Parent folder subs"],
  ["assets", "Assets directory", "Assets dir fallback"],
];

function filesText(paths) {
  if (paths.length === 0) return "";
  return paths.length === 1 ? paths[0] : `${paths.length} subtitle files`;
}

/**
 * The ladder rung of a manifest subtitle candidate for an item of `release`: "same" when it sits
 * in the release directory, else "ancestor". Both are root-relative POSIX paths from the manifest.
 */
export function candidateTier(path, release) {
  const dir = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : ".";
  return dir === (release || ".") ? "same" : "ancestor";
}

/**
 * The subtitle precedence ladder of one item (ADR-0004): manual > same folder > embedded >
 * parent folder > assets dir. Each rung says what the inspection found for it. A manual
 * assignment is "Used". Without one, the highest rung that found anything is "Auto": the engine
 * tries it first, but matches files to episodes itself, so the UI does not claim which file wins.
 *
 * `candidates` are the release's root-relative subtitle paths from the Inspection Manifest.
 */
export function subtitleLadder({ release, candidates = [], embedded = null, assignment = null, assetsDir = "" }) {
  const details = {
    manual: assignment?.asset_path ?? "",
    same: filesText(candidates.filter(path => candidateTier(path, release) === "same")),
    embedded: embedded ?? "",
    ancestor: filesText(candidates.filter(path => candidateTier(path, release) === "ancestor")),
    assets: assetsDir ?? "",
  };
  // The assets dir is a setting, not a file the inspection found, so it is only ever a fallback.
  const winner = RUNGS.find(([key]) => key !== "assets" && details[key])?.[0] ?? null;
  return RUNGS.map(([key, label, short]) => ({
    key,
    label,
    short,
    detail: details[key],
    chosen: key === winner,
    tag: key === winner
      ? (key === "manual" ? "Used" : "Auto")
      : !details[key] ? "None" : key === "assets" ? "Fallback" : "Found",
  }));
}

/** The meta line's subtitle part: the ladder's chosen rung, the assets fallback, or "No subs". */
export function subtitleSourceText(ladder) {
  const rung = ladder.find(r => r.chosen) ?? ladder.find(r => r.key === "assets" && r.detail);
  return rung?.short ?? "No subs";
}

/**
 * A sparkline's polyline points for `samples` scaled to `max`, newest at the right edge of a
 * `width`×`height` box. Returns "" with fewer than two samples.
 */
export function sparkPoints(samples, max, width = 64, height = 20) {
  if (samples.length < 2) return "";
  const step = width / (samples.length - 1);
  return samples
    .map((value, i) => `${(i * step).toFixed(1)},${(height - 2 - Math.min(Math.max(value / max, 0), 1) * (height - 4)).toFixed(1)}`)
    .join(" ");
}
