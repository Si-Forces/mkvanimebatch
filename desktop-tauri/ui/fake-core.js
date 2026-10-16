// Test support, not part of the app: published View State shapes, and a Desktop Core that serves
// scripted ones. The queue rules themselves are the core's and are tested in Rust; this fake only
// applies the Intents a test needs, so the webview can be driven end to end.

/** The inspection metadata a manifest row carries; a row the manifest does not list has none. */
const METADATA = {
  resolution: null,
  video_codec: null,
  video_tag: null,
  audio_summary: null,
  has_flac: null,
  sub_summary: null,
  fonts_count: null,
};

/** A published Row: a queued, selected manifest row named after its ID unless `fields` say otherwise. */
export function row(id, fields = {}) {
  return {
    id,
    name: id,
    release: ".",
    metadata: { ...METADATA },
    inspection_error: null,
    in_manifest: true,
    selected: true,
    status: "queued",
    progress: null,
    start_time_ms: null,
    duration_sec: null,
    reason: null,
    error: null,
    participant: false,
    ...fields,
  };
}

/** A published ModeView with `rows` (Rows or IDs) and an idle scan. */
export function modeView(rows = [], fields = {}) {
  return {
    directory: null,
    scan: { state: "idle" },
    manifest_rev: 0,
    subtitle_candidates: {},
    rows: rows.map(entry => (typeof entry === "string" ? row(entry) : entry)),
    vanished_selected_ids: [],
    assignments: {},
    ...fields,
  };
}

/** A published Job View of a running remux job over `participants`. */
export function jobView(participants = [], fields = {}) {
  return {
    job_id: 1,
    mode: "remux",
    status: "running",
    active: true,
    start_time_ms: Date.now(),
    input_dir: "/anime/show",
    effective_settings: { workers: 3, io_jobs: 5, ffmpeg_threads: 2, bitrate: "256k", complexity: 8 },
    participants,
    ...fields,
  };
}

/** Drops the nulls the core leaves out of a published Track Assignment. */
function published(assignment) {
  return Object.fromEntries(Object.entries(assignment).filter(([, value]) => value !== null && value !== undefined));
}

/**
 * A Desktop Core over a scripted View State. `state` is the next state to publish; change it, then
 * `publish()` bumps `seq` and hands a copy to every `onPublish` listener (the `view-state` event).
 * `invoke(command, args)` answers `get_state` and `dispatch`: a queue Intent is applied to
 * `state` as the core would: refused while a job runs or the mode scans, a Track Assignment is
 * refused on extract, and only the manifest rows take Intents. A scan is answered by
 * `onScan(intent)`, which the test scripts and which publishes what it wants itself.
 */
export function createFakeCore({ onScan = async () => true } = {}) {
  const listeners = new Set();
  const core = {
    state: { seq: 0, job: null, remux: modeView(), extract: modeView() },
    onScan,
    intents: [],
    onPublish: listener => listeners.add(listener),
    publish() {
      core.state.seq++;
      const copy = structuredClone(core.state);
      for (const listener of listeners) listener(copy);
      return copy;
    },
    /** Accepts `rows` as the inspection of `directory` for `mode`, as a new manifest. */
    accept(mode, directory, rows, fields = {}) {
      const previous = core.state[mode];
      core.state[mode] = modeView(rows, { directory, manifest_rev: previous.manifest_rev + 1, ...fields });
      return core.publish();
    },
    async invoke(command, args) {
      if (command === "get_state") return structuredClone(core.state);
      if (command !== "dispatch") return undefined;
      const { intent } = args;
      core.intents.push(intent);
      if (intent.type === "scan_directory") return core.onScan(intent);
      if (core.state.job?.active) throw "A batch is running; the queue is locked until it ends.";
      const view = core.state[intent.mode];
      if (view.scan.state === "scanning") throw "The folder is being scanned; the queue is locked until it ends.";
      if (intent.mode === "extract" && intent.type.endsWith("_track")) throw "Track Assignments are for remux only.";
      const changed = applyIntent(view, intent);
      if (changed) core.publish();
      return changed;
    },
  };
  return core;
}

/** Applies a queue Intent to the manifest rows of `view`, as `Queue` does; false when nothing changed. */
function applyIntent(view, intent) {
  // Only the manifest rows are the queue's: the rows a job adds after them take no Intent.
  const manifest = view.rows.filter(entry => entry.in_manifest);
  const jobOnly = view.rows.filter(entry => !entry.in_manifest);
  const at = manifest.findIndex(candidate => candidate.id === (intent.id ?? intent.assignment?.target_video));
  switch (intent.type) {
    case "set_selected": {
      if (at === -1) return false;
      const changed = manifest[at].selected !== intent.selected || view.vanished_selected_ids.length > 0;
      manifest[at].selected = intent.selected;
      view.vanished_selected_ids = [];
      return changed;
    }
    case "set_all_selected": {
      const changed = manifest.some(entry => entry.selected !== intent.selected)
        || view.vanished_selected_ids.length > 0;
      for (const entry of manifest) entry.selected = intent.selected;
      view.vanished_selected_ids = [];
      return changed;
    }
    case "reorder": {
      const target = intent.direction === "up" ? at - 1 : at + 1;
      if (at === -1 || target < 0 || target >= manifest.length) return false;
      [manifest[at], manifest[target]] = [manifest[target], manifest[at]];
      view.rows = [...manifest, ...jobOnly];
      return true;
    }
    case "assign_track":
      if (at === -1) return false;
      view.assignments[intent.assignment.target_video] = published(intent.assignment);
      return true;
    case "clear_track":
      if (!(intent.id in view.assignments)) return false;
      delete view.assignments[intent.id];
      return true;
    default:
      throw new Error(`unexpected intent: ${intent.type}`);
  }
}
