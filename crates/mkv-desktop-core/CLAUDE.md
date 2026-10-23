# crates/mkv-desktop-core/CLAUDE.md

The Desktop Core side of the Engine Bridge. The Tauri host's commands, its publisher and the webview's patch format are in `desktop-tauri/CLAUDE.md`.

## Engine ↔ desktop bridge

### Modules

- `job.rs`: the active job (`JobState`: the Job Slot, the supervisor, the two-phase stop).
- `snapshot.rs`: the `JobSnapshot` and its reducer.
- `actor.rs`: the Desktop Core actor (the Intents, the View State).
- `coordinator.rs`: the Job Coordinator and the Run preflight.
- `actor/start.rs`: the start of a Run and its Stop.
- `actor/timers.rs`: the telemetry cadence and the Presentation Heartbeat (the actor's tests in `actor/tests.rs`).
- `presentation.rs`: every displayed value (row labels, tones, meta lines, filter buckets, the clock, counts, progress, Library, the job controls' and chips' texts).
- `inspector.rs`: the Episode Inspector.
- `log.rs`: each mode's Run Log.
- `draft.rs`: each mode's Settings Draft (its stepper and the `BatchRequest` a Run builds from it).
- `queue.rs`: each mode's queue (Inspection Manifest, Queue Selection, Track Assignments, rows over a cached row base).
- `inspection.rs`: the Inspection Coordinator.
- `control.rs`: the run, stop and shutdown logic (with `EngineLauncher`, the actor's `Launcher` port adapter, which resolves, launches and stops on the blocking pool).
- `telemetry.rs`: the System Telemetry Provider (the `Sampler` port, `EngineSampler`, and the `TelemetryView` the View State shows).
- `text.rs`: text predicates that read input the way a JavaScript view would.

The Desktop Core's tasks run on a tokio `Handle` the adapter passes to `JobState::new` and `Core::spawn`.

### Job events

The engine streams `JobEvent`s and log lines over two tokio mpsc channels. A supervisor task forwards them through a `JobSink`, which in the app is the actor's `ActorSink` and in tests records them with no Tauri app. The supervisor reduces the events into its own private `JobSnapshot`, the settlement ledger it settles the job and computes `job-done` from, so teardown never depends on the actor. The actor reduces the same events with `JobSnapshot::apply_at` (the same reducer, on tokio's clock so paused-time tests move it) into the published snapshot, which carries the job's Effective Settings.

Only one job runs at a time. Stopping is two-phase: a cooperative cancel with a 2 s wait, then an abort of the pipeline task.

### The actor and the View State

The actor owns each mode's queue, scan, Settings Draft (the form's values as typed), Run Log and outcome, and the Job Coordinator, applies the Intents `dispatch` sends (a scan, a selection, a reorder, an `inspect` pick, a Track Assignment, a draft edit or `step_draft`, a Preset Profile, a Run, a Stop, a cache clear, the view's visibility), and publishes the whole View State (`{seq, job, run, busy, defaults, chips, presets, remux, extract, telemetry}`, `busy` being the phase not idle or a job active; each ModeView carries its rows' texts, the list header, `count_text`, `clock`, `progress`, `filters`, `library`, `inspector` and `controls`) at most every 66 ms, at once before a `dispatch` reply except to a draft Intent (`edit_draft`, `step_draft`, `set_preset`) or `view_visible` (coalesced by that cap), and before `job-done`. Each publication rebuilds only the ModeView of a mode that changed (a remux bitrate or assets dir edit changes remux, since the rows' meta lines name them), laying the status over each row's cached base. The core publishes no diff (a native view reads the state in-process).

A Run answers once its start settles (resolve, forced scan, preflight, launch) and a Stop once its teardown is over; while a start or a job holds the batch, both queues and both drafts are locked and the user's scans are refused.

### Run Log

Each mode's Run Log keeps its newest 2000 lines: a job's batches go to the job's mode, and the core writes the Run refusal, Stop, teardown and cache lines itself; a Run clears its mode's log once its request resolves. Each append or clear goes out at once as a `job-log` `LogBatch` (`{mode, generation, first, keep_from, errors, lines}`), and `get_log` reads a mode's whole log back.

### Telemetry and the Presentation Heartbeat

The core samples telemetry through its `Sampler` port every 1 s while busy and every 30 s otherwise, and not at all while `view_visible` says the window is hidden. The Presentation Heartbeat beats on each whole second of the job's clock while the batch clock runs (after a `BatchStart` naming items, until the batch ends) and the window is visible; a beat only marks the job's mode changed, and every publication presents the elapsed times and the clock as of then.

### Directory Identities

Directory Identities that cross the bridge must use the display form of the canonical path, with the `\\?\` prefix stripped (`display_canonical_path`). That covers the identity a `ScanDirectory` Intent resolves the typed path to (`inspection::canonicalize_directory`), as well as `canonical_dir`, a ModeView's `directory`, the Job Snapshot's `input_dir`, and `BatchStart.input_dir`. The Desktop Core's queue and its Run preflight compare directories byte for byte and never normalize a path, so a mismatch silently breaks selection and ordering.
