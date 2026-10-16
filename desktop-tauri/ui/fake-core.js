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

/** The published Preset Profile catalog, as `mkv_core::preset::get_preset_profiles` builds it. */
const PRESETS = [
  { id: "balanced", name: "Balanced", bitrate: "192k", complexity: 5 },
  { id: "fast", name: "Fast muxing", bitrate: "128k", complexity: 5 },
  { id: "hifi", name: "Hi-Fi master", bitrate: "320k", complexity: 10 },
  { id: "custom", name: "Custom", bitrate: "192k", complexity: 5 },
];

/** The published Effective Settings defaults the fake core starts with. */
export const DEFAULTS = { workers: 6, io_jobs: 2, ffmpeg_threads: 1, bitrate: "192k", complexity: 5 };

/** A mode's initial Settings Draft, as `SettingsDraft::new` builds it, with `fields` over it. */
export function draft(mode, fields = {}) {
  const remux = mode === "remux"
    ? { assets_dir: "", preset: "balanced", bitrate: "192k", complexity: "5", sub_offset: "0", ffmpeg_threads: "" }
    : {};
  return { input_dir: "", out_dir: "", fonts_dir: "", workers: "", io_jobs: "", dry_run: false, no_cache: false, ...remux, ...fields };
}

/** A published ModeView with `rows` (Rows or IDs), an idle scan and `mode`'s initial draft. */
export function modeView(rows = [], fields = {}, mode = "remux") {
  return {
    directory: null,
    scan: { state: "idle" },
    manifest_rev: 0,
    subtitle_candidates: {},
    rows: rows.map(entry => (typeof entry === "string" ? row(entry) : entry)),
    vanished_selected_ids: [],
    assignments: {},
    draft: draft(mode),
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
 * Applies a draft Intent to `view.draft` as `SettingsDraft` does; false when nothing changed.
 * Throws the core's error for a field or a preset the mode does not have.
 */
function applyDraftIntent(view, intent, presets) {
  const current = view.draft;
  if (intent.type === "set_preset") {
    if (!("preset" in current)) throw "Preset Profiles are for remux only.";
    const profile = presets.find(p => p.id === intent.preset);
    if (!profile) throw `Unknown Preset Profile '${intent.preset}'.`;
    const next = profile.id === "custom"
      ? { ...current, preset: "custom" }
      : { ...current, preset: profile.id, bitrate: profile.bitrate, complexity: String(profile.complexity) };
    view.draft = next;
    return JSON.stringify(next) !== JSON.stringify(current);
  }
  // The core rejects the key while deserializing the Intent, on either mode.
  if ("preset" in intent.changes) throw "unknown field `preset`";
  const missing = Object.keys(intent.changes).find(key => !(key in current));
  if (missing) throw `Extract has no ${missing} setting.`;
  const changed = Object.keys(intent.changes).filter(key => current[key] !== intent.changes[key]);
  view.draft = { ...current, ...intent.changes };
  if (changed.includes("bitrate") || changed.includes("complexity")) view.draft.preset = "custom";
  return changed.length > 0;
}

/**
 * A Desktop Core over a scripted View State. `state` is the next state to publish; change it, then
 * `publish()` bumps `seq` and hands a copy to every `onPublish` listener (the `view-state` event).
 * `invoke(command, args)` answers `get_state` and `dispatch`: a queue Intent is applied to
 * `state` as the core would: refused while a job runs or the mode scans, a Track Assignment is
 * refused on extract, and only the manifest rows take Intents. A Settings Draft Intent follows
 * `applyDraftIntent`, refused while a job runs. A scan is answered by
 * `onScan(intent)`, which the test scripts and which publishes what it wants itself.
 */
export function createFakeCore({ onScan = async () => true } = {}) {
  const listeners = new Set();
  const core = {
    state: {
      seq: 0,
      job: null,
      defaults: structuredClone(DEFAULTS),
      presets: structuredClone(PRESETS),
      remux: modeView(),
      extract: modeView([], {}, "extract"),
    },
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
      core.state[mode] = modeView(rows, { directory, manifest_rev: previous.manifest_rev + 1, draft: previous.draft, ...fields });
      return core.publish();
    },
    async invoke(command, args) {
      if (command === "get_state") return structuredClone(core.state);
      if (command !== "dispatch") return undefined;
      const { intent } = args;
      core.intents.push(intent);
      if (intent.type === "scan_directory") return core.onScan(intent);
      const view = core.state[intent.mode];
      if (intent.type === "edit_draft" || intent.type === "set_preset") {
        if (core.state.job?.active) throw "A batch is running; the settings are locked until it ends.";
        const changed = applyDraftIntent(view, intent, core.state.presets);
        if (changed) core.publish();
        return changed;
      }
      if (core.state.job?.active) throw "A batch is running; the queue is locked until it ends.";
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
