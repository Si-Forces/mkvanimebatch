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
  return {
    input_dir: "", out_dir: "", fonts_dir: "", workers: "", io_jobs: "", dry_run: false, no_cache: false, ...remux, invalid: null, ...fields,
  };
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
    outcome: null,
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

/** The published Job Coordinator while nothing holds the batch. */
export const IDLE_RUN = { phase: "idle", mode: null, settings: null };

/** Drops the nulls the core leaves out of a published Track Assignment. */
function published(assignment) {
  return Object.fromEntries(Object.entries(assignment).filter(([, value]) => value !== null && value !== undefined));
}

/** The keys an `edit_draft` Intent may set: the fields of `DraftChanges`. */
const DRAFT_KEYS = new Set([
  "input_dir", "out_dir", "fonts_dir", "assets_dir", "bitrate", "complexity", "sub_offset",
  "workers", "io_jobs", "ffmpeg_threads", "dry_run", "no_cache",
]);

/**
 * Applies a draft Intent to `view.draft` as `SettingsDraft` does; false when nothing changed.
 * Throws the core's error for a field or a preset the mode does not have. A mirror for tests:
 * `crates/mkv-desktop-core/src/draft.rs` holds the rules, so change them there first. Presets are
 * looked up by exact id, without `lookup_preset`'s aliases.
 */
function applyDraftIntent(view, intent, presets) {
  const current = view.draft;
  if (intent.type === "set_preset") {
    if (!("preset" in current)) throw "Preset Profiles are for remux only.";
    const profile = presets.find(p => p.id === intent.preset);
    if (!profile) throw `Unknown preset profile: '${intent.preset}'`;
    const next = profile.id === "custom"
      ? { ...current, preset: "custom" }
      : {
        ...current,
        preset: profile.id,
        bitrate: profile.bitrate,
        complexity: String(profile.complexity),
        // The profile replaced a flagged complexity.
        ...(current.invalid === "complexity" ? { invalid: null } : {}),
      };
    view.draft = next;
    return JSON.stringify(next) !== JSON.stringify(current);
  }
  // The core rejects a key `DraftChanges` lacks (the preset too) while deserializing the Intent, on
  // either mode.
  const unknown = Object.keys(intent.changes).find(key => !DRAFT_KEYS.has(key));
  if (unknown) throw `unknown field \`${unknown}\``;
  const missing = Object.keys(intent.changes).find(key => !(key in current));
  if (missing) throw `Extract has no ${missing} setting.`;
  const changed = Object.keys(intent.changes).filter(key => current[key] !== intent.changes[key]);
  // An edit of the flagged field clears the flag.
  const unflagged = current.invalid !== null && current.invalid in intent.changes;
  view.draft = { ...current, ...intent.changes, ...(unflagged ? { invalid: null } : {}) };
  if (changed.includes("bitrate") || changed.includes("complexity")) view.draft.preset = "custom";
  return changed.length > 0 || unflagged;
}

/**
 * The answers the core gives a Run it refuses, or whose start fails (`actor/start.rs`), and a scan
 * it refuses while a start is before its launch (`actor.rs`).
 */
const RUN_BUSY = "A job is already running or stopping. Wait for it to finish.";
const RUN_SCANNING = "The folder is being scanned; a batch can start once the scan ends.";
const SCAN_STARTING = "A batch is starting; no folder can be scanned until it has started.";

/** The lines the core writes into a Run Log (`actor/start.rs`, `actor.rs`). */
const STOP_REQUESTED = "Stop requested; waiting for shutdown.";
const CACHE_CLEARED = "Persistent inspection cache cleared.";

/**
 * A mode's Run Log as `crates/mkv-desktop-core/src/log.rs` keeps it, uncapped and with no display
 * text: what `get_log` answers. `push` and `clear` return the `job-log` batch.
 */
function runLog(mode) {
  const log = { mode, generation: 0, first: 0, lines: [] };
  const errors = () => log.lines.filter(line => line.kind === "error").length;
  const batch = (first, lines) => ({ mode, generation: log.generation, first, keep_from: log.first, errors: errors(), lines });
  return {
    view: () => ({ mode, generation: log.generation, first: log.first, errors: errors(), lines: structuredClone(log.lines) }),
    push(lines) {
      const first = log.first + log.lines.length;
      log.lines.push(...lines);
      return batch(first, structuredClone(lines));
    },
    clear() {
      Object.assign(log, { generation: log.generation + 1, first: 0, lines: [] });
      return batch(0, []);
    },
  };
}

/**
 * A Desktop Core over a scripted View State. `state` is the next state to publish; change it, then
 * `publish()` bumps `seq` and hands a copy to every `onPublish` listener (the `view-state` event).
 * `invoke(command, args)` answers `get_state` and `dispatch`: a queue Intent is applied to
 * `state` as the core would: refused while a job runs or the mode scans, a Track Assignment is
 * refused on extract, and only the manifest rows take Intents. A Settings Draft Intent follows
 * `applyDraftIntent`, refused while busy. A scan is answered by
 * `onScan(intent)`, which the test scripts and which publishes what it wants itself; every scan
 * is refused while a start is before its launch.
 *
 * Run and Stop mirror the Job Coordinator (`crates/mkv-desktop-core/src/actor/start.rs` and
 * `coordinator.rs` hold the rules; change them there first). A Run is refused while busy or while
 * its mode scans; a draft with no input folder flags `input_dir` (the only build check mirrored;
 * `draft.rs` holds the rest); otherwise the start begins
 * (`starting`, its outcome reset) and `onStart(intent)` scripts the rest: by default
 * `launch(mode)`, which runs a job over the mode's selected rows. Its answer is the Run's. A Stop
 * of the owning mode ends the job, or the start, as cancelled (`end`) and answers true; of any
 * other, false. Busy is `run.phase` not idle, or an active job, as `isBusy` reads it.
 *
 * Each mode has a Run Log (`runLog`): `log(mode, lines)` appends to it and hands the batch to every
 * `onLog` listener (the `job-log` event), and `get_log` answers it. Like the core, a Run answered
 * with an error logs `ERROR: …`, a Run that passes the input folder check clears its log, a Stop of
 * a running batch logs the wait, and `clear_cache` logs its line. `view_visible` is recorded in
 * `visible`, not in `intents`.
 */
export function createFakeCore({ onScan = async () => true, onStart = null } = {}) {
  const listeners = new Set();
  const logListeners = new Set();
  const logs = { remux: runLog("remux"), extract: runLog("extract") };
  const emitLog = batch => {
    for (const listener of logListeners) listener(structuredClone(batch));
  };
  const core = {
    state: {
      seq: 0,
      job: null,
      run: structuredClone(IDLE_RUN),
      defaults: structuredClone(DEFAULTS),
      presets: structuredClone(PRESETS),
      remux: modeView(),
      extract: modeView([], {}, "extract"),
      telemetry: null,
    },
    visible: null,
    onScan,
    onStart: onStart ?? (({ mode }) => core.launch(mode)),
    intents: [],
    // The core's `busy()` (`crates/mkv-desktop-core/src/actor.rs`), as `isBusy` in `presentation.js` reads it.
    busy: () => core.state.run.phase !== "idle" || Boolean(core.state.job?.active),
    /** Launches job `job_id` of the starting `mode` over its selected rows, and publishes it running. */
    launch(mode, job_id = (core.state.job?.job_id ?? 0) + 1) {
      const participants = core.state[mode].rows.filter(entry => entry.in_manifest && entry.selected).map(entry => entry.id);
      core.state.run = { phase: "running", mode, settings: structuredClone(DEFAULTS) };
      core.state.job = jobView(participants, {
        job_id, mode, input_dir: core.state[mode].directory, effective_settings: structuredClone(DEFAULTS),
      });
      core.publish();
      return true;
    },
    /** Ends the batch `run.mode` owns with `outcome`, its job (if any) in `status`, and publishes it. */
    end(outcome, status = outcome === "done" ? "completed" : outcome) {
      const mode = core.state.run.mode ?? core.state.job?.mode;
      if (core.state.job?.active) Object.assign(core.state.job, { status, active: false });
      core.state.run = structuredClone(IDLE_RUN);
      core.state[mode].outcome = outcome;
      return core.publish();
    },
    onPublish: listener => listeners.add(listener),
    onLog: listener => logListeners.add(listener),
    /** Appends `lines` (`{kind, text, display?}`) to `mode`'s Run Log and emits their batch. */
    log(mode, lines) {
      emitLog(logs[mode].push(lines));
    },
    publish() {
      core.state.seq++;
      const copy = structuredClone(core.state);
      for (const listener of listeners) listener(copy);
      return copy;
    },
    /** Accepts `rows` as the inspection of `directory` for `mode`, as a new manifest. */
    accept(mode, directory, rows, fields = {}) {
      const previous = core.state[mode];
      core.state[mode] = modeView(rows, { directory, manifest_rev: previous.manifest_rev + 1, draft: previous.draft, outcome: previous.outcome, ...fields }, mode);
      return core.publish();
    },
    async invoke(command, args) {
      if (command === "get_state") return structuredClone(core.state);
      if (command === "get_log") return logs[args.mode].view();
      if (command !== "dispatch") return undefined;
      const { intent } = args;
      if (intent.type === "view_visible") {
        const changed = core.visible !== intent.visible;
        core.visible = intent.visible;
        return changed;
      }
      core.intents.push(intent);
      if (intent.type === "scan_directory") {
        if (core.state.run.phase === "starting") throw SCAN_STARTING;
        return core.onScan(intent);
      }
      const view = core.state[intent.mode];
      if (intent.type === "run") {
        try {
          return await run(intent, view);
        } catch (err) {
          core.log(intent.mode, [{ kind: "error", text: `ERROR: ${err}` }]);
          throw err;
        }
      }
      if (intent.type === "stop") {
        if (core.state.run.mode !== intent.mode) return false;
        if (core.state.run.phase === "running") core.log(intent.mode, [{ kind: "note", text: STOP_REQUESTED }]);
        core.end("cancelled");
        return true;
      }
      if (intent.type === "clear_cache") {
        core.log(intent.mode, [{ kind: "cache", text: CACHE_CLEARED }]);
        return true;
      }
      if (intent.type === "edit_draft" || intent.type === "set_preset") {
        if (core.busy()) throw "A batch is running; the settings are locked until it ends.";
        const changed = applyDraftIntent(view, intent, core.state.presets);
        if (changed) core.publish();
        return changed;
      }
      if (core.busy()) throw "A batch is running; the queue is locked until it ends.";
      if (view.scan.state === "scanning") throw "The folder is being scanned; the queue is locked until it ends.";
      if (intent.mode === "extract" && intent.type.endsWith("_track")) throw "Track Assignments are for remux only.";
      const changed = applyIntent(view, intent);
      if (changed) core.publish();
      return changed;
    },
  };
  /** A Run as the core starts it; the caller logs the error it answers with. */
  async function run(intent, view) {
    if (core.busy()) throw RUN_BUSY;
    if (view.scan.state === "scanning") throw RUN_SCANNING;
    if (!view.draft.input_dir.trim()) {
      view.draft.invalid = "input_dir";
      core.publish();
      throw "Input directory is required.";
    }
    view.draft.invalid = null;
    view.outcome = null;
    // The request resolved: its mode's log clears.
    emitLog(logs[intent.mode].clear());
    core.state.run = { phase: "starting", mode: intent.mode, settings: null };
    core.publish();
    return core.onStart(intent);
  }
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
