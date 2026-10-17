use super::*;
use crate::inspector::{Choice, LadderRung, Pill};
use crate::presentation::Tone;
use crate::queue::test_support::manifest_of;
use crate::snapshot::test_support::{
    batch_complete, batch_start, effective_settings, item_complete, item_progress, item_start,
};
use crate::snapshot::ItemStatus;
use mkv_core::request::BatchRequest;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::Notify;

/// One thing the actor published, dones as JSON.
#[derive(Debug, Clone)]
pub(super) enum Published {
    State(Box<ViewState>),
    Logs(LogBatch),
    Done(Value),
}

#[derive(Clone, Default)]
pub(super) struct RecordingPublisher {
    published: Arc<Mutex<Vec<Published>>>,
    /// Notified on every state publication.
    pub(super) stated: Arc<Notify>,
}

impl RecordingPublisher {
    pub(super) fn all(&self) -> Vec<Published> {
        self.published.lock().unwrap().clone()
    }

    pub(super) fn states(&self) -> Vec<ViewState> {
        self.all()
            .into_iter()
            .filter_map(|published| match published {
                Published::State(state) => Some(*state),
                _ => None,
            })
            .collect()
    }

    /// The log batches published, in order.
    pub(super) fn logs(&self) -> Vec<LogBatch> {
        self.all()
            .into_iter()
            .filter_map(|published| match published {
                Published::Logs(batch) => Some(batch),
                _ => None,
            })
            .collect()
    }
}

impl Publisher for RecordingPublisher {
    fn state(&self, state: &ViewState) {
        self.published
            .lock()
            .unwrap()
            .push(Published::State(Box::new(state.clone())));
        self.stated.notify_one();
    }

    fn logs(&self, batch: &LogBatch) {
        let batch = Published::Logs(batch.clone());
        self.published.lock().unwrap().push(batch);
    }

    fn done(&self, done: &JobDone) {
        let done = serde_json::to_value(done).unwrap();
        self.published.lock().unwrap().push(Published::Done(done));
    }
}

/// A core whose defaults are known from the start, so they publish nothing of their own.
pub(super) fn ready<P: Publisher, I: Inspector>(publisher: P, inspector: I) -> CoreHandle {
    ready_with(publisher, inspector, FakeLauncher::default())
}

pub(super) fn ready_with<P: Publisher, I: Inspector, L: Launcher>(
    publisher: P,
    inspector: I,
    launcher: L,
) -> CoreHandle {
    ready_with_ports(publisher, inspector, launcher, NoSampler)
}

pub(super) fn ready_with_ports<P: Publisher, I: Inspector, L: Launcher, S: Sampler>(
    publisher: P,
    inspector: I,
    launcher: L,
    sampler: S,
) -> CoreHandle {
    let ports = Ports {
        inspector,
        launcher,
        sampler,
    };
    Core::start(
        Handle::current(),
        publisher,
        ports,
        Some(effective_settings()),
    )
}

/// A `Sampler` whose first sample never comes back, so telemetry publishes nothing and only
/// `actor::timers` tests see it.
pub(super) struct NoSampler;

impl Sampler for NoSampler {
    async fn sample(&self) -> Result<TelemetryData, String> {
        std::future::pending().await
    }
}

fn spawn_core() -> (CoreHandle, RecordingPublisher) {
    let publisher = RecordingPublisher::default();
    let core = ready(publisher.clone(), FakeInspector::default());
    (core, publisher)
}

pub(super) fn started(job_id: u64) -> JobStarted {
    JobStarted {
        job_id,
        mode: "remux",
        input_dir: "/anime".into(),
        effective_settings: effective_settings(),
        start_time_ms: 1_700_000_000_000,
    }
}

pub(super) fn done(job_id: u64, code: i32) -> JobDone {
    JobDone {
        mode: "remux",
        job_id,
        code,
    }
}

#[tokio::test(start_paused = true)]
async fn test_a_burst_of_events_publishes_at_most_once_per_interval_and_ends_on_the_final_state() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    let begun = Instant::now();

    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    sink.event(&item_start("ep1"));
    for step in 0..10_000 {
        sink.event(&item_progress("ep1", f64::from(step) / 100.0));
        // Spread the burst over about a second of paused time.
        if step % 100 == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    sink.event(&item_complete("ep1", 9.0));
    sink.event(&batch_complete(true, false));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;

    let elapsed = begun.elapsed();
    let states = publisher.states();
    let bound = elapsed.as_millis().div_ceil(PUBLISH_INTERVAL.as_millis()) + 1;
    assert!(
        states.len() as u128 <= bound,
        "{} publications in {elapsed:?}",
        states.len()
    );
    assert!(states.len() > 2, "the burst published only at its edges");
    // The last publication holds the final snapshot, and nothing is left unpublished.
    let last = states.last().unwrap().clone();
    let job = last.job.clone().unwrap();
    assert_eq!(job.status, BatchStatus::Completed);
    assert_eq!(core.state().await.unwrap(), last);
    let row = &last.remux.rows[0];
    assert!(row.participant);
    assert_eq!(row.progress, Some(99.99));
    assert_eq!(row.label, "Done in 9.0s");
}

#[tokio::test(start_paused = true)]
async fn test_a_steady_stream_that_never_drains_the_inbox_still_publishes_every_interval() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    let begun = Instant::now();

    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    sink.event(&item_start("ep1"));
    for step in 0..300 {
        // More than the actor's cooperative budget per turn, so the queue only grows and
        // the biased inbox arm is always ready.
        for _ in 0..200 {
            sink.event(&item_progress("ep1", f64::from(step) / 10.0));
        }
        tokio::time::advance(Duration::from_millis(2)).await;
    }

    let elapsed = begun.elapsed();
    let states = publisher.states().len() as u128;
    let interval = PUBLISH_INTERVAL.as_millis();
    let bound = elapsed.as_millis().div_ceil(interval) + 1;
    assert!(states <= bound, "{states} publications in {elapsed:?}");
    assert!(
        states + 2 >= elapsed.as_millis() / interval,
        "a steady stream starved publication: {states} in {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn test_a_sink_dropped_without_done_ends_the_active_job() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    // A supervisor that panicked inside a sink call drops it without `done`.
    drop(sink);
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;

    let state = core.state().await.unwrap();
    assert!(!state.job.unwrap().active);
    assert!(!publisher
        .all()
        .iter()
        .any(|published| matches!(published, Published::Done(_))));
    // No longer active, so a reset of its mode drops the snapshot.
    let reset = Intent::ScanDirectory {
        mode: PipelineMode::Remux,
        path: String::new(),
        force: false,
        no_cache: false,
    };
    assert_eq!(core.dispatch(reset).await, Ok(false));
    assert_eq!(core.state().await.unwrap().job, None);
}

/// What `busy` must read in every published state: the run phase is not idle, or a job is
/// active. `actor::start` tests check it over the starting, running and stopping phases.
pub(super) fn assert_busy_follows_the_phase_and_the_job(states: &[ViewState]) {
    for state in states {
        let active = state.job.as_ref().is_some_and(|job| job.active);
        assert_eq!(
            state.busy,
            state.run.phase != Phase::Idle || active,
            "{state:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_busy_is_published_while_a_job_is_active_even_with_the_phase_idle() {
    let (core, publisher) = spawn_core();
    assert!(!core.state().await.unwrap().busy);
    // A job the actor did not start, as a sink reports it: the phase stays idle.
    let sink = core.job_sink();
    sink.started(&started(1));
    let state = core.state().await.unwrap();
    assert_eq!(state.run.phase, Phase::Idle);
    assert!(state.busy);
    sink.done(&done(1, 0));
    assert!(!core.state().await.unwrap().busy);
    assert_busy_follows_the_phase_and_the_job(&publisher.states());
}

#[tokio::test(start_paused = true)]
async fn test_a_sink_dropped_after_done_publishes_nothing_more() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    sink.started(&started(1));
    sink.done(&done(1, 0));
    core.state().await.unwrap();
    let before = publisher.all().len();

    drop(sink);
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    core.state().await.unwrap();
    assert_eq!(publisher.all().len(), before);
}

#[tokio::test(start_paused = true)]
async fn test_a_single_change_after_idle_publishes_at_once() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    let begun = Instant::now();

    sink.started(&started(1));
    // The state is read after the start in the same inbox, with the clock not moved: the
    // start was published on the leading edge, not after an interval.
    let state = core.state().await.unwrap();
    assert_eq!(begun.elapsed(), Duration::ZERO);
    assert_eq!(state.seq, 1);
    assert_eq!(state.job.as_ref().unwrap().job_id, 1);
    assert!(state.job.unwrap().active);

    tokio::time::sleep(Duration::from_secs(1)).await;
    let idle_until = Instant::now();
    sink.event(&batch_start(&["ep1", "ep2"]));
    let state = core.state().await.unwrap();
    assert_eq!(idle_until.elapsed(), Duration::ZERO);
    assert_eq!(state.seq, 2);
    assert_eq!(ids(&state.remux), ["ep1", "ep2"]);
    assert_eq!(publisher.states().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn test_events_logs_and_done_keep_the_supervisors_order_and_the_last_state_precedes_done() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();

    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    sink.logs(&[LogLine::note("one")]);
    sink.event(&item_start("ep1"));
    sink.logs(&[LogLine::note("two")]);
    sink.event(&batch_complete(false, true));
    sink.done(&done(1, -1));
    core.state().await.unwrap();

    let published = publisher.all();
    let order: Vec<String> = published
        .iter()
        .filter_map(|published| match published {
            Published::State(_) => None,
            Published::Logs(batch) => Some(format!(
                "log {} {:?} at {}",
                batch.lines[0].text, batch.mode, batch.first
            )),
            Published::Done(done) => Some(format!("done {}", done["code"])),
        })
        .collect();
    // Each batch goes into the Run Log of the job's mode, after the lines before it.
    assert_eq!(
        order,
        ["log one Remux at 0", "log two Remux at 1", "done -1"]
    );
    // The trailing changes were not yet due, so `done` flushed them first.
    let Published::State(last) = &published[published.len() - 2] else {
        panic!("done was not preceded by a state publication: {published:?}");
    };
    let job = last.job.as_ref().unwrap();
    assert!(!job.active);
    assert_eq!(job.status, BatchStatus::Cancelled);
    assert!(matches!(published.last(), Some(Published::Done(_))));
}

#[tokio::test(start_paused = true)]
async fn test_a_jobs_lines_go_into_the_run_log_of_its_own_mode_and_get_log_reads_them_back() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();
    sink.started(&JobStarted {
        mode: "extract",
        ..started(1)
    });
    sink.logs(&[LogLine::step("Extracting /anime/ep1.mkv")]);
    // A bridge error line takes the same way.
    sink.logs(&[LogLine::error("ERROR: Pipeline failed: x")]);

    let log = core.log(EXTRACT).await.unwrap();
    assert_eq!(
        serde_json::to_value(&log).unwrap(),
        serde_json::json!({
            "mode": "extract",
            "generation": 0,
            "first": 0,
            "errors": 1,
            "lines": [
                {
                    "kind": "step",
                    "text": "Extracting /anime/ep1.mkv",
                    "display": "Extracting anime › ep1.mkv"
                },
                { "kind": "error", "text": "ERROR: Pipeline failed: x" },
            ]
        })
    );
    assert!(core.log(REMUX).await.unwrap().lines.is_empty());
    let batches = publisher.logs();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        serde_json::to_value(&batches[1]).unwrap(),
        serde_json::json!({
            "mode": "extract",
            "generation": 0,
            "first": 1,
            "keep_from": 0,
            "errors": 1,
            "lines": [{ "kind": "error", "text": "ERROR: Pipeline failed: x" }]
        })
    );
}

#[tokio::test(start_paused = true)]
async fn test_clear_cache_writes_its_line_or_its_error_into_the_modes_run_log() {
    let inspector = FakeInspector::default();
    let publisher = RecordingPublisher::default();
    let core = ready(publisher.clone(), inspector.clone());
    let clear = |mode| Intent::ClearCache { mode };

    assert_eq!(core.dispatch(clear(REMUX)).await, Ok(true));
    *inspector.clear_error.lock().unwrap() = Some("Permission denied".into());
    assert_eq!(
        core.dispatch(clear(REMUX)).await,
        Err("Permission denied".into())
    );
    let log = core.log(REMUX).await.unwrap();
    let lines: Vec<_> = log
        .lines
        .iter()
        .map(|line| (line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        lines,
        [
            (LogKind::Cache, "Persistent inspection cache cleared."),
            (LogKind::Error, "ERROR: Permission denied"),
        ]
    );
    assert!(core.log(EXTRACT).await.unwrap().lines.is_empty());
    // Nothing in the View State changed.
    assert!(publisher.states().is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_a_tick_armed_before_a_done_flush_waits_a_full_interval_after_it() {
    let (core, _publisher) = spawn_core();
    let ms = Duration::from_millis;
    let first = core.job_sink();
    first.started(&started(1)); // t=0: leading edge, seq 1
    tokio::time::sleep(ms(10)).await;
    first.event(&batch_start(&["ep1"])); // t=10: the tick is armed for t=66
    tokio::time::sleep(ms(10)).await;
    first.done(&done(1, 0)); // t=20: the flush, seq 2
    tokio::time::sleep(ms(10)).await;
    let second = core.job_sink(); // lives to the end, so job 2 stays active
    second.started(&started(2)); // t=30: due at t=86, not at the stale t=66

    tokio::time::sleep(ms(40)).await; // t=70
    assert_eq!(core.state().await.unwrap().seq, 2);
    tokio::time::sleep(ms(20)).await; // t=90
    let state = core.state().await.unwrap();
    assert_eq!(state.seq, 3);
    assert_eq!(state.job.unwrap().job_id, 2);
}

#[tokio::test(start_paused = true)]
async fn test_get_state_mid_job_is_the_last_published_state() {
    let (core, publisher) = spawn_core();
    let sink = core.job_sink();

    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    // Not due yet: the batch waits for the trailing edge.
    let state = core.state().await.unwrap();
    assert_eq!(&state, publisher.states().last().unwrap());
    assert!(state.remux.rows.is_empty());

    // Past the trailing edge, not on it, so the tick cannot race the read.
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let state = core.state().await.unwrap();
    assert_eq!(&state, publisher.states().last().unwrap());
    assert_eq!(ids(&state.remux), ["ep1"]);
}

#[tokio::test(start_paused = true)]
async fn test_a_later_job_replaces_the_snapshot_and_stale_events_are_dropped() {
    let (core, _publisher) = spawn_core();
    let first = core.job_sink();
    first.started(&started(1));
    first.event(&batch_start(&["ep1"]));
    first.done(&done(1, 0));

    let second = core.job_sink();
    second.started(&JobStarted {
        job_id: 2,
        mode: "extract",
        input_dir: "/other".into(),
        effective_settings: EffectiveSettings::defaults(),
        start_time_ms: 1_700_000_000_001,
    });
    // A late event of the first job does not touch the second's snapshot.
    first.event(&batch_start(&["ep9"]));
    // Past the trailing edge, not on it, so the tick cannot race the read.
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;

    let state = core.state().await.unwrap();
    let job = state.job.unwrap();
    assert_eq!(job.job_id, 2);
    assert_eq!(job.mode, "extract");
    assert_eq!(job.input_dir, "/other");
    assert_eq!(job.effective_settings, EffectiveSettings::defaults());
    assert!(state.extract.rows.is_empty());
    assert!(state.remux.rows.is_empty());
}

#[test]
fn test_view_state_wire_shape() {
    let mut remux = ModeView {
        directory: Some("/anime".into()),
        scan: ScanView::Scanning {
            directory: "/anime".into(),
        },
        manifest_rev: 2,
        rows: Arc::new(vec![Row {
            name: "ep1.mkv".into(),
            status: ItemStatus::Processing,
            progress: Some(40.0),
            participant: true,
            label: "40% · 3s".into(),
            tone: Tone::Info,
            ..Row::test("ep1")
        }]),
        vanished_selected_ids: vec!["ep9".into()],
        title: "anime".into(),
        crumbs: vec!["anime".into()],
        count_text: "Scanning…".into(),
        clock: "3s elapsed".into(),
        progress: Progress {
            live: 40.0,
            ..Progress::default()
        },
        filters: Filters {
            all: 1,
            todo: 1,
            ..Filters::default()
        },
        selected_count: 1,
        total_count: 1,
        library: vec![LibraryEntry {
            release: None,
            label: "anime".into(),
            tooltip: "anime".into(),
            count: 1,
            group_label: "anime".into(),
        }],
        ..empty(REMUX)
    };
    let row = serde_json::to_value(&remux.rows[0]).unwrap();
    remux
        .subtitle_candidates
        .insert(".".into(), vec!["ep1.ass".into()]);
    remux.assignments.insert(
        "ep1".into(),
        TrackAssignment {
            target_video: "ep1".into(),
            asset_path: "/subs/ep1.ass".into(),
            track_type: Default::default(),
            language: Some("ind".into()),
            title: None,
            offset_ms: Some(-250),
        },
    );
    remux.outcome = Some(Outcome::Failed);
    remux.inspector = Some(InspectorView {
        id: "ep1".into(),
        number: "01".into(),
        title: "ep1.mkv".into(),
        crumbs: Vec::new(),
        error: None,
        pill: Pill {
            tone: Tone::Info,
            label: "40% · 3s".into(),
        },
        video: "HEVC · copied".into(),
        audio_label: "Audio",
        audio: "FLAC 2.0 → Opus 192k".into(),
        out_crumbs: Some(vec!["out".into()]),
        fonts_head: "Fonts · 1".into(),
        fonts: "1 attached font · folder: auto".into(),
        ladder: vec![LadderRung {
            key: "manual",
            label: "Manual assignment",
            detail: "subs › ep1.ass".into(),
            chosen: true,
            found: true,
            tag: "Used",
        }],
        options: vec![Choice {
            value: String::new(),
            label: "Automatic (follow the ladder)".into(),
        }],
        assignment: None,
    });
    remux.controls = controls(
        REMUX,
        &remux,
        &Hold {
            owner: Some(REMUX),
            stopping: false,
            busy: true,
        },
    );
    let mut telemetry = TelemetryView::default();
    telemetry.push(&TelemetryData {
        cpu_pct: 37.0,
        ram_used_mb: 5243,
        ram_total_mb: 16_384,
        ram_pct: 32.0,
        io_mb_s: 0.0,
    });
    let state = ViewState {
        seq: 3,
        run: RunView {
            phase: Phase::Running,
            mode: Some(REMUX),
            settings: Some(effective_settings()),
        },
        busy: true,
        job: Some(JobView {
            job_id: 7,
            mode: "remux".into(),
            status: BatchStatus::Running,
            active: true,
            input_dir: "/anime".into(),
            effective_settings: effective_settings(),
        }),
        defaults: Some(EffectiveSettings::defaults()),
        chips: Chips {
            workers: "3 Workers Active".into(),
            io_jobs: "5 IO Jobs Active".into(),
            workers_hint: Some("auto (6)".into()),
            io_jobs_hint: None,
        },
        presets: get_preset_profiles()[..1].to_vec(),
        remux,
        extract: ModeView {
            scan: ScanView::Failed,
            ..empty(EXTRACT)
        },
        telemetry: Some(telemetry),
    };
    // Too deep for one `json!`.
    let inspector = serde_json::json!({
        "id": "ep1",
        "number": "01",
        "title": "ep1.mkv",
        "crumbs": [],
        "error": null,
        "pill": { "tone": "info", "label": "40% · 3s" },
        "video": "HEVC · copied",
        "audio_label": "Audio",
        "audio": "FLAC 2.0 → Opus 192k",
        "out_crumbs": ["out"],
        "fonts_head": "Fonts · 1",
        "fonts": "1 attached font · folder: auto",
        "ladder": [{
            "key": "manual",
            "label": "Manual assignment",
            "detail": "subs › ep1.ass",
            "chosen": true,
            "found": true,
            "tag": "Used"
        }],
        "options": [{ "value": "", "label": "Automatic (follow the ladder)" }],
        "assignment": null
    });
    let controls_json = serde_json::json!({
        "run_label": "Run remux · 1",
        // The owner's Run is hidden, but its tooltip still says why it would be disabled.
        "run_title": "Scanning the folder",
        "run_disabled": true,
        "owns": true,
        "stop_label": "Stop",
        "stop_disabled": false,
        "selection_locked": true,
        "status": { "class": "status-running", "label": "Running" }
    });
    // Rows have their own pin: `queue::tests::test_row_wire_shape`.
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::json!({
            "seq": 3,
            "job": {
                "job_id": 7,
                "mode": "remux",
                "status": "running",
                "active": true,
                "input_dir": "/anime",
                "effective_settings": {
                    "workers": 3,
                    "io_jobs": 5,
                    "ffmpeg_threads": 2,
                    "bitrate": "256k",
                    "complexity": 8
                }
            },
            "run": {
                "phase": "running",
                "mode": "remux",
                "settings": {
                    "workers": 3,
                    "io_jobs": 5,
                    "ffmpeg_threads": 2,
                    "bitrate": "256k",
                    "complexity": 8
                }
            },
            "busy": true,
            "defaults": serde_json::to_value(EffectiveSettings::defaults()).unwrap(),
            "chips": {
                "workers": "3 Workers Active",
                "io_jobs": "5 IO Jobs Active",
                "workers_hint": "auto (6)",
                "io_jobs_hint": null
            },
            "presets": [
                { "id": "balanced", "name": "Balanced", "bitrate": "192k", "complexity": 5 }
            ],
            "remux": {
                "directory": "/anime",
                "scan": { "state": "scanning", "directory": "/anime" },
                "manifest_rev": 2,
                "subtitle_candidates": { ".": ["ep1.ass"] },
                // The shared rows serialize as the list itself.
                "rows": [row],
                "vanished_selected_ids": ["ep9"],
                "assignments": {
                    "ep1": {
                        "target_video": "ep1",
                        "asset_path": "/subs/ep1.ass",
                        "track_type": "subtitle",
                        "language": "ind",
                        "offset_ms": -250
                    }
                },
                // The draft has its own pin: `draft::tests::test_draft_wire_shape`.
                "draft": serde_json::to_value(SettingsDraft::new(REMUX)).unwrap(),
                "outcome": "failed",
                "title": "anime",
                "crumbs": ["anime"],
                "count_text": "Scanning…",
                "clock": "3s elapsed",
                "progress": { "done": 0.0, "skipped": 0.0, "failed": 0.0, "live": 40.0 },
                "filters": { "all": 1, "todo": 1, "done": 0, "failed": 0 },
                "selected_count": 1,
                "total_count": 1,
                "library": [{
                    "release": null,
                    "label": "anime",
                    "tooltip": "anime",
                    "count": 1,
                    "group_label": "anime"
                }],
                "inspector": inspector,
                "controls": controls_json
            },
            "extract": {
                "directory": null,
                "scan": { "state": "failed" },
                "manifest_rev": 0,
                "subtitle_candidates": {},
                "rows": [],
                "vanished_selected_ids": [],
                "assignments": {},
                "draft": {
                    "input_dir": "", "out_dir": "", "fonts_dir": "", "workers": "",
                    "io_jobs": "", "dry_run": false, "no_cache": false, "invalid": null
                },
                "outcome": null,
                "title": "No folder open",
                "crumbs": [],
                "count_text": "Ready",
                "clock": "Not started",
                "progress": { "done": 0.0, "skipped": 0.0, "failed": 0.0, "live": 0.0 },
                "filters": { "all": 0, "todo": 0, "done": 0, "failed": 0 },
                "selected_count": 0,
                "total_count": 0,
                "library": [],
                "inspector": null,
                "controls": {
                    "run_label": "Run extract",
                    "run_title": "",
                    "run_disabled": false,
                    "owns": false,
                    "stop_label": "Stop",
                    "stop_disabled": true,
                    "selection_locked": false,
                    "status": { "class": "status-idle", "label": "Idle" }
                }
            },
            "telemetry": {
                "cpu": "37%",
                "ram": "5.12 GB",
                "disk": "Idle",
                "samples": { "cpu": [37.0], "ram": [32.0], "disk": [0.0] }
            }
        })
    );
    let idle = serde_json::to_value(ViewState::new(None)).unwrap();
    assert_eq!(idle["job"], Value::Null);
    assert_eq!(idle["defaults"], Value::Null);
    assert_eq!(idle["telemetry"], Value::Null);
    assert_eq!(idle["busy"], false);
    assert_eq!(
        idle["run"],
        serde_json::json!({ "phase": "idle", "mode": null, "settings": null })
    );
    for (outcome, wire) in [
        (Outcome::Done, "done"),
        (Outcome::Cancelled, "cancelled"),
        (Outcome::Failed, "failed"),
    ] {
        assert_eq!(serde_json::to_value(outcome).unwrap(), wire);
    }
    for (phase, wire) in [(Phase::Starting, "starting"), (Phase::Stopping, "stopping")] {
        assert_eq!(serde_json::to_value(phase).unwrap(), wire);
    }
    assert_eq!(
        idle["remux"]["scan"],
        serde_json::json!({ "state": "idle" })
    );
}

#[test]
fn test_intent_wire_shape() {
    let intent = |json: Value| serde_json::from_value::<Intent>(json).unwrap();
    // `no_cache` may be left out.
    assert_eq!(
        intent(serde_json::json!({
            "type": "scan_directory", "mode": "remux", "path": "/anime", "force": true
        })),
        Intent::ScanDirectory {
            mode: PipelineMode::Remux,
            path: "/anime".into(),
            force: true,
            no_cache: false,
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "scan_directory", "mode": "extract", "path": "/anime", "force": false,
            "no_cache": true
        })),
        Intent::ScanDirectory {
            mode: PipelineMode::Extract,
            path: "/anime".into(),
            force: false,
            no_cache: true,
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "set_selected", "mode": "remux", "id": "ep1", "selected": false
        })),
        Intent::SetSelected {
            mode: PipelineMode::Remux,
            id: "ep1".into(),
            selected: false,
        }
    );
    assert_eq!(
        intent(
            serde_json::json!({ "type": "set_all_selected", "mode": "remux", "selected": true })
        ),
        Intent::SetAllSelected {
            mode: PipelineMode::Remux,
            selected: true,
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "reorder", "mode": "extract", "id": "ep1", "direction": "up"
        })),
        Intent::Reorder {
            mode: PipelineMode::Extract,
            id: "ep1".into(),
            direction: Direction::Up,
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "assign_track", "mode": "remux",
            "assignment": {
                "target_video": "ep1", "asset_path": "/subs/ep1.ass", "track_type": "subtitle",
                "language": null, "title": "Signs", "offset_ms": null
            }
        })),
        Intent::AssignTrack {
            mode: PipelineMode::Remux,
            assignment: subtitle("ep1", "/subs/ep1.ass", Some("Signs")),
        }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "clear_track", "mode": "remux", "id": "ep1" })),
        Intent::ClearTrack {
            mode: PipelineMode::Remux,
            id: "ep1".into(),
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "edit_draft", "mode": "remux",
            "changes": { "input_dir": "/anime", "dry_run": true }
        })),
        Intent::EditDraft {
            mode: PipelineMode::Remux,
            changes: DraftChanges {
                input_dir: Some("/anime".into()),
                dry_run: Some(true),
                ..DraftChanges::default()
            },
        }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "set_preset", "mode": "remux", "preset": "fast" })),
        Intent::SetPreset {
            mode: PipelineMode::Remux,
            preset: "fast".into(),
        }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "run", "mode": "extract" })),
        Intent::Run { mode: EXTRACT }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "stop", "mode": "remux" })),
        Intent::Stop { mode: REMUX }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "clear_cache", "mode": "extract" })),
        Intent::ClearCache { mode: EXTRACT }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "view_visible", "visible": false })),
        Intent::ViewVisible { visible: false }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "inspect", "mode": "remux", "id": "ep1" })),
        Intent::Inspect {
            mode: REMUX,
            id: Some("ep1".into()),
        }
    );
    assert_eq!(
        intent(serde_json::json!({ "type": "inspect", "mode": "extract", "id": null })),
        Intent::Inspect {
            mode: EXTRACT,
            id: None,
        }
    );
    assert_eq!(
        intent(serde_json::json!({
            "type": "step_draft", "mode": "remux", "field": "sub_offset", "direction": "down"
        })),
        Intent::StepDraft {
            mode: REMUX,
            field: StepField::SubOffset,
            direction: Direction::Down,
        }
    );
    for (wire, field) in [
        ("workers", StepField::Workers),
        ("io_jobs", StepField::IoJobs),
        ("ffmpeg_threads", StepField::FfmpegThreads),
    ] {
        assert_eq!(
            intent(serde_json::json!({
                "type": "step_draft", "mode": "remux", "field": wire, "direction": "up"
            })),
            Intent::StepDraft {
                mode: REMUX,
                field,
                direction: Direction::Up,
            }
        );
    }
    assert!(serde_json::from_value::<Intent>(serde_json::json!({
        "type": "reorder", "mode": "remux", "id": "ep1", "direction": "left"
    }))
    .is_err());
}

/// A Track Assignment of `path` to `id`, with no language.
pub(super) fn subtitle(id: &str, path: &str, title: Option<&str>) -> TrackAssignment {
    TrackAssignment {
        target_video: id.into(),
        asset_path: path.into(),
        track_type: Default::default(),
        language: None,
        title: title.map(str::to_string),
        offset_ms: None,
    }
}

/// An `Inspector` that answers from a script, after a delay on the paused clock. A path
/// resolves to itself less any trailing `/` or `/.`, after `slow` when it is `/slow`; a
/// directory without a scripted manifest fails to inspect.
#[derive(Clone, Default)]
pub(super) struct FakeInspector {
    pub(super) manifests: Arc<Mutex<HashMap<String, InspectionManifest>>>,
    inspected: Arc<Mutex<Vec<(String, bool)>>>,
    /// What `clear_cache` answers: `Ok` when `None`.
    pub(super) clear_error: Arc<Mutex<Option<String>>>,
}

pub(super) const SLOW: Duration = Duration::from_millis(500);
pub(super) const INSPECTION: Duration = Duration::from_millis(200);

impl FakeInspector {
    pub(super) fn with(self, directory: &str, manifest: InspectionManifest) -> Self {
        self.manifests
            .lock()
            .unwrap()
            .insert(directory.into(), manifest);
        self
    }

    pub(super) fn inspected(&self) -> Vec<(String, bool)> {
        self.inspected.lock().unwrap().clone()
    }
}

impl Inspector for FakeInspector {
    async fn identity(&self, path: String) -> Result<String, String> {
        if path == "/slow" {
            tokio::time::sleep(SLOW).await;
        }
        if path.starts_with("/gone") {
            return Err("Invalid directory path: missing".into());
        }
        let trimmed = path.trim_end_matches("/.").trim_end_matches('/');
        Ok(trimmed.to_string())
    }

    async fn inspect(
        &self,
        identity: String,
        no_cache: bool,
    ) -> Result<InspectionManifest, String> {
        self.inspected
            .lock()
            .unwrap()
            .push((identity.clone(), no_cache));
        tokio::time::sleep(INSPECTION).await;
        assert!(identity != "/panic", "inspector panicked");
        let manifest = self.manifests.lock().unwrap().get(&identity).cloned();
        manifest.ok_or_else(|| format!("cannot inspect {identity}"))
    }

    async fn clear_cache(&self) -> Result<(), String> {
        tokio::time::sleep(INSPECTION).await;
        self.clear_error.lock().unwrap().clone().map_or(Ok(()), Err)
    }
}

pub(super) fn scan(mode: PipelineMode, path: &str, force: bool) -> Intent {
    Intent::ScanDirectory {
        mode,
        path: path.into(),
        force,
        no_cache: false,
    }
}

pub(super) const REMUX: PipelineMode = PipelineMode::Remux;
pub(super) const EXTRACT: PipelineMode = PipelineMode::Extract;

/// A core over `inspector` whose `/anime` holds `ep1` and `ep2`, and `/fast` and `/slow` one
/// episode each.
fn scanning_core() -> (CoreHandle, RecordingPublisher, FakeInspector) {
    let inspector = FakeInspector::default()
        .with("/anime", manifest_of(&["ep1", "ep2"]))
        .with("/fast", manifest_of(&["fast"]))
        .with("/slow", manifest_of(&["slow"]));
    let publisher = RecordingPublisher::default();
    let core = ready(publisher.clone(), inspector.clone());
    (core, publisher, inspector)
}

/// The view of a mode with nothing in its queue and its draft as it starts.
fn empty(mode: PipelineMode) -> ModeView {
    let mut view = ModeState::new(mode).view(None, StdInstant::now());
    view.controls = controls(mode, &view, &Hold::default());
    view
}

pub(super) fn ids(view: &ModeView) -> Vec<&str> {
    view.rows.iter().map(|row| row.id.as_str()).collect()
}

#[tokio::test(start_paused = true)]
async fn test_a_scan_shows_scanning_then_accepts_the_inspection_of_the_identity() {
    let (core, publisher, inspector) = scanning_core();

    let pending = tokio::spawn({
        let core = core.clone();
        async move { core.dispatch(scan(REMUX, " /anime/. ", false)).await }
    });
    tokio::time::sleep(INSPECTION / 2).await;
    let state = core.state().await.unwrap();
    assert_eq!(
        state.remux.scan,
        ScanView::Scanning {
            directory: "/anime".into()
        }
    );
    // The queue is the scan's until it settles.
    assert!(core
        .dispatch(Intent::SetAllSelected {
            mode: REMUX,
            selected: false,
        })
        .await
        .unwrap_err()
        .contains("scanned"));

    assert_eq!(pending.await.unwrap(), Ok(true));
    // The answer follows the publication of the accepted queue.
    let state = core.state().await.unwrap();
    assert_eq!(&state, publisher.states().last().unwrap());
    assert_eq!(state.remux.scan, ScanView::Idle);
    assert_eq!(state.remux.directory.as_deref(), Some("/anime"));
    assert_eq!(ids(&state.remux), ["ep1", "ep2"]);
    assert_eq!(state.remux.manifest_rev, 1);
    assert_eq!(state.extract, empty(EXTRACT));
    assert_eq!(inspector.inspected(), [("/anime".to_string(), false)]);
}

#[tokio::test(start_paused = true)]
async fn test_a_manifest_is_accepted_under_its_canonical_dir_else_the_identity() {
    let mut manifest = manifest_of(&["ep1"]);
    manifest
        .extra
        .insert("canonical_dir".into(), "/real/anime".into());
    let inspector = FakeInspector::default()
        .with("/anime", manifest)
        .with("/plain", manifest_of(&["ep1"]));
    let core = ready(RecordingPublisher::default(), inspector.clone());

    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.directory.as_deref(), Some("/real/anime"));
    assert_eq!(
        core.dispatch(scan(EXTRACT, "/plain/", false)).await,
        Ok(true)
    );
    let state = core.state().await.unwrap();
    assert_eq!(state.extract.directory.as_deref(), Some("/plain"));
}

#[tokio::test(start_paused = true)]
async fn test_a_rescan_reuses_the_held_inspection_unless_forced() {
    let (core, _publisher, inspector) = scanning_core();
    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
    core.dispatch(Intent::SetSelected {
        mode: REMUX,
        id: "ep2".into(),
        selected: false,
    })
    .await
    .unwrap();

    // Another spelling of the same identity reuses it.
    assert_eq!(core.dispatch(scan(REMUX, "/anime/", false)).await, Ok(true));
    assert_eq!(inspector.inspected().len(), 1);
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.manifest_rev, 2);
    assert!(!state.remux.rows[1].selected);

    // A forced scan inspects again, keeping the selection of the same directory.
    assert_eq!(core.dispatch(scan(REMUX, "/anime", true)).await, Ok(true));
    assert_eq!(inspector.inspected().len(), 2);
    let state = core.state().await.unwrap();
    assert!(!state.remux.rows[1].selected);
    // The other mode holds no inspection to reuse.
    assert_eq!(
        core.dispatch(scan(EXTRACT, "/anime", false)).await,
        Ok(true)
    );
    assert_eq!(inspector.inspected().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn test_scans_of_one_key_share_one_inspection_across_views_but_a_forced_one_does_not() {
    let (core, _publisher, inspector) = scanning_core();
    let (remux, extract, other_spelling) = tokio::join!(
        core.dispatch(scan(REMUX, "/anime", false)),
        core.dispatch(scan(EXTRACT, "/anime", false)),
        core.dispatch(scan(EXTRACT, "/anime/.", false)),
    );
    assert_eq!(
        (remux, extract, other_spelling),
        (Ok(true), Ok(false), Ok(true))
    );
    assert_eq!(inspector.inspected(), [("/anime".to_string(), false)]);
    let state = core.state().await.unwrap();
    assert_eq!(ids(&state.remux), ["ep1", "ep2"]);
    assert_eq!(ids(&state.extract), ["ep1", "ep2"]);

    let (browse, forced) = tokio::join!(
        core.dispatch(scan(REMUX, "/fast", false)),
        core.dispatch(scan(EXTRACT, "/fast", true)),
    );
    assert_eq!((browse, forced), (Ok(true), Ok(true)));
    assert_eq!(inspector.inspected().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn test_a_late_resolving_scan_does_not_overtake_a_newer_request() {
    let (core, _publisher, inspector) = scanning_core();
    let (slow, fast) = tokio::join!(core.dispatch(scan(REMUX, "/slow", false)), async {
        tokio::task::yield_now().await;
        core.dispatch(scan(REMUX, "/fast", false)).await
    },);
    assert_eq!((slow, fast), (Ok(false), Ok(true)));
    assert_eq!(inspector.inspected(), [("/fast".to_string(), false)]);
    let state = core.state().await.unwrap();
    assert_eq!(ids(&state.remux), ["fast"]);
}

#[tokio::test(start_paused = true)]
async fn test_an_inspection_that_settles_while_a_newer_request_resolves_is_overtaken() {
    let (core, publisher, _inspector) = scanning_core();
    // `/anime` settles at 200 ms, while the newer `/slow` request resolves until 600 ms.
    let (anime, slow) = tokio::join!(core.dispatch(scan(REMUX, "/anime", false)), async {
        tokio::time::sleep(INSPECTION / 2).await;
        core.dispatch(scan(REMUX, "/slow", false)).await
    });
    assert_eq!((anime, slow), (Ok(false), Ok(true)));
    assert!(publisher
        .states()
        .iter()
        .all(|state| !ids(&state.remux).contains(&"ep1")));
    assert_eq!(ids(&core.state().await.unwrap().remux), ["slow"]);
}

#[tokio::test(start_paused = true)]
async fn test_a_publication_rebuilds_only_the_view_of_a_mode_that_changed() {
    /// The address of each mode's row buffer in every publication: a view rebuilt is a new
    /// buffer, while one republished as last built keeps its own.
    #[derive(Clone, Default)]
    struct RowBuffers(Arc<Mutex<Vec<(usize, usize)>>>);

    impl Publisher for RowBuffers {
        fn state(&self, state: &ViewState) {
            let buffers = (
                state.remux.rows.as_ptr() as usize,
                state.extract.rows.as_ptr() as usize,
            );
            self.0.lock().unwrap().push(buffers);
        }

        fn logs(&self, _batch: &LogBatch) {}

        fn done(&self, _done: &JobDone) {}
    }

    let inspector = FakeInspector::default().with("/anime", manifest_of(&["ep1", "ep2"]));
    let publisher = RowBuffers::default();
    let core = ready(publisher.clone(), inspector);
    core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
    core.dispatch(scan(EXTRACT, "/anime", false)).await.unwrap();
    let before = publisher.0.lock().unwrap().len();
    // A draft edit rebuilds no rows either.
    let out_dir = DraftChanges {
        out_dir: Some("/out".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(EXTRACT, out_dir)).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;

    let sink = core.job_sink();
    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));
    sink.event(&item_start("ep1"));
    for step in 0..5 {
        sink.event(&item_progress("ep1", f64::from(step)));
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    }

    // From the extract scan's last publication on: the remux job rebuilt the remux rows
    // every time it published, and nothing rebuilt the extract ones.
    let buffers = publisher.0.lock().unwrap()[before - 1..].to_vec();
    assert!(buffers.len() > 6, "{} publications", buffers.len());
    assert!(buffers.iter().all(|&(_, extract)| extract == buffers[0].1));
    // The draft edit's publication, then the job's.
    assert_eq!(buffers[1].0, buffers[0].0);
    assert!(buffers[1..].windows(2).all(|pair| pair[0].0 != pair[1].0));
    let state = core.state().await.unwrap();
    assert_eq!(state.extract.draft.out_dir, "/out");
    assert_eq!(state.remux.rows[0].progress, Some(4.0));
    assert!(state.extract.rows.iter().all(|row| !row.participant));
}

#[tokio::test(start_paused = true)]
async fn test_a_scan_of_one_view_does_not_overtake_the_others() {
    let (core, _publisher, _inspector) = scanning_core();
    let (remux, extract) = tokio::join!(
        core.dispatch(scan(REMUX, "/slow", false)),
        core.dispatch(scan(EXTRACT, "/fast", false)),
    );
    assert_eq!((remux, extract), (Ok(true), Ok(true)));
    let state = core.state().await.unwrap();
    assert_eq!(ids(&state.remux), ["slow"]);
    assert_eq!(ids(&state.extract), ["fast"]);
}

#[tokio::test(start_paused = true)]
async fn test_an_empty_path_resets_the_queue_and_overtakes_a_pending_scan() {
    let (core, _publisher, inspector) = scanning_core();
    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));

    // One still resolving, and one still inspecting.
    let (resolving, inspecting, reset) = tokio::join!(
        core.dispatch(scan(REMUX, "/slow", false)),
        core.dispatch(scan(EXTRACT, "/fast", false)),
        async {
            tokio::time::sleep(INSPECTION / 2).await;
            let reset = core.dispatch(scan(REMUX, "   ", false)).await;
            (reset, core.dispatch(scan(EXTRACT, "", true)).await)
        },
    );
    assert_eq!((resolving, inspecting), (Ok(false), Ok(false)));
    assert_eq!(reset, (Ok(false), Ok(false)));
    assert_eq!(
        inspector.inspected(),
        [("/anime".to_string(), false), ("/fast".to_string(), false)]
    );
    let state = core.state().await.unwrap();
    assert_eq!(
        state.remux,
        ModeView {
            manifest_rev: 2,
            ..empty(REMUX)
        }
    );
    assert_eq!(state.extract.scan, ScanView::Idle);
    assert!(state.extract.rows.is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_a_failed_scan_keeps_the_rows_and_inspects_again_next_time() {
    let (core, _publisher, inspector) = scanning_core();
    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
    inspector.manifests.lock().unwrap().remove("/anime");

    assert_eq!(core.dispatch(scan(REMUX, "/anime", true)).await, Ok(false));
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.scan, ScanView::Failed);
    assert_eq!(ids(&state.remux), ["ep1", "ep2"]);
    assert_eq!(state.remux.directory.as_deref(), Some("/anime"));

    // No reuse after a failure.
    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(false));
    assert_eq!(inspector.inspected().len(), 3);

    // A path with no identity is inspected as typed (trimmed), and fails there.
    assert_eq!(
        core.dispatch(scan(EXTRACT, " /gone/ ", false)).await,
        Ok(false)
    );
    assert_eq!(inspector.inspected()[3], ("/gone/".to_string(), false));
    assert_eq!(core.state().await.unwrap().extract.scan, ScanView::Failed);
}

#[tokio::test(start_paused = true)]
async fn test_a_panicking_inspection_fails_the_scan_and_leaves_no_key_in_flight() {
    let (core, _publisher, inspector) = scanning_core();
    assert_eq!(core.dispatch(scan(REMUX, "/panic", false)).await, Ok(false));
    assert_eq!(core.state().await.unwrap().remux.scan, ScanView::Failed);
    // The key is free again: the next scan inspects anew instead of joining the dead one.
    assert_eq!(core.dispatch(scan(REMUX, "/panic", false)).await, Ok(false));
    assert_eq!(inspector.inspected().len(), 2);
    // And the queue is not locked.
    assert_eq!(
        core.dispatch(Intent::SetAllSelected {
            mode: REMUX,
            selected: true,
        })
        .await,
        Ok(false)
    );
}

#[tokio::test(start_paused = true)]
async fn test_a_manifest_of_another_version_fails_the_scan() {
    let mut manifest = manifest_of(&["ep1"]);
    manifest.version = 1;
    let inspector = FakeInspector::default().with("/anime", manifest);
    let core = ready(RecordingPublisher::default(), inspector);

    assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(false));
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.scan, ScanView::Failed);
    assert!(state.remux.rows.is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_queue_intents_apply_and_publish_before_they_answer() {
    let (core, publisher, _inspector) = scanning_core();
    core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
    let select = |id: &str, selected| Intent::SetSelected {
        mode: REMUX,
        id: id.into(),
        selected,
    };

    assert_eq!(core.dispatch(select("ep1", false)).await, Ok(true));
    assert_eq!(core.dispatch(select("missing", false)).await, Ok(false));
    let reorder = |direction| Intent::Reorder {
        mode: REMUX,
        id: "ep2".into(),
        direction,
    };
    assert_eq!(core.dispatch(reorder(Direction::Up)).await, Ok(true));
    assert_eq!(core.dispatch(reorder(Direction::Up)).await, Ok(false));
    let assignment = subtitle("ep2", "/subs/ep2.ass", None);
    let assign = |mode| Intent::AssignTrack {
        mode,
        assignment: assignment.clone(),
    };
    assert_eq!(core.dispatch(assign(REMUX)).await, Ok(true));
    assert!(core.dispatch(assign(EXTRACT)).await.is_err());

    // No sleep: each answer came after its publication.
    let state = core.state().await.unwrap();
    assert_eq!(&state, publisher.states().last().unwrap());
    assert_eq!(ids(&state.remux), ["ep2", "ep1"]);
    assert!(!state.remux.rows[1].selected);
    assert_eq!(state.remux.assignments["ep2"], assignment);

    let clear = Intent::ClearTrack {
        mode: REMUX,
        id: "ep2".into(),
    };
    assert_eq!(core.dispatch(clear.clone()).await, Ok(true));
    assert_eq!(core.dispatch(clear).await, Ok(false));
    let clear_extract = Intent::ClearTrack {
        mode: EXTRACT,
        id: "ep2".into(),
    };
    assert!(core.dispatch(clear_extract).await.is_err());
    assert!(core.state().await.unwrap().remux.assignments.is_empty());
    assert_eq!(
        core.dispatch(Intent::SetAllSelected {
            mode: REMUX,
            selected: true,
        })
        .await,
        Ok(true)
    );
    assert!(core
        .state()
        .await
        .unwrap()
        .remux
        .rows
        .iter()
        .all(|row| row.selected));
}

#[tokio::test(start_paused = true)]
async fn test_a_running_job_locks_both_queues_and_lays_its_items_over_its_own_mode() {
    let (core, _publisher, _inspector) = scanning_core();
    core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
    core.dispatch(scan(EXTRACT, "/anime", false)).await.unwrap();
    let sink = core.job_sink();
    sink.started(&started(1));
    sink.event(&batch_start(&["ep2"]));
    sink.event(&item_start("ep2"));

    for mode in [REMUX, EXTRACT] {
        let error = core
            .dispatch(Intent::SetAllSelected {
                mode,
                selected: false,
            })
            .await
            .unwrap_err();
        assert!(error.contains("running"), "{error}");
    }
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.rows[1].status, ItemStatus::Processing);
    assert!(state.remux.rows[1].participant);
    assert!(!state.remux.rows[0].participant);
    // The extract queue shows the same directory, but the job is not its.
    assert!(state.extract.rows.iter().all(|row| !row.participant));

    sink.done(&done(1, 0));
    assert_eq!(
        core.dispatch(Intent::SetAllSelected {
            mode: REMUX,
            selected: false,
        })
        .await,
        Ok(true)
    );
}

#[tokio::test(start_paused = true)]
async fn test_the_job_snapshot_clears_only_for_its_own_mode_and_never_while_it_runs() {
    let (core, _publisher, _inspector) = scanning_core();
    let sink = core.job_sink();
    sink.started(&started(1));
    sink.event(&batch_start(&["ep1"]));

    // While the job runs, a scan or a reset of its own mode is refused, so its queue stays.
    for path in ["/anime", ""] {
        let error = core.dispatch(scan(REMUX, path, false)).await.unwrap_err();
        assert!(error.contains("running"), "{path:?}: {error}");
    }
    // Another mode's scan still runs, and leaves the snapshot.
    assert_eq!(
        core.dispatch(scan(EXTRACT, "/anime", false)).await,
        Ok(true)
    );
    assert!(core.state().await.unwrap().job.is_some());

    sink.done(&done(1, 0));
    // Another mode's scan and reset leave it.
    assert_eq!(
        core.dispatch(scan(EXTRACT, "/anime", false)).await,
        Ok(true)
    );
    assert_eq!(core.dispatch(scan(EXTRACT, "", false)).await, Ok(false));
    let state = core.state().await.unwrap();
    assert!(state.job.is_some());
    // A failed scan of its own mode leaves it too.
    assert_eq!(core.dispatch(scan(REMUX, "/gone", false)).await, Ok(false));
    assert!(core.state().await.unwrap().job.is_some());

    // Each of an accepted scan, a reuse and a reset of its own mode clears it.
    for (path, force) in [("/anime", true), ("/anime", false), ("", false)] {
        core.job_sink().started(&started(2));
        core.job_sink().done(&done(2, 0));
        assert!(core.state().await.unwrap().job.is_some());
        core.dispatch(scan(REMUX, path, force)).await.unwrap();
        assert_eq!(core.state().await.unwrap().job, None, "{path:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn test_a_scan_begun_before_a_job_of_its_mode_is_refused_once_the_job_runs() {
    let (core, _publisher, _inspector) = scanning_core();
    // One scan still resolving, then one still inspecting, when the job starts.
    for (path, wait) in [("/slow", SLOW / 2), ("/anime", INSPECTION / 2)] {
        let pending = tokio::spawn({
            let core = core.clone();
            async move { core.dispatch(scan(REMUX, path, false)).await }
        });
        tokio::time::sleep(wait).await;
        let sink = core.job_sink();
        sink.started(&started(1));
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("running"), "{path}: {error}");
        let state = core.state().await.unwrap();
        assert_eq!(state.remux.scan, ScanView::Idle, "{path}");
        assert!(state.remux.rows.is_empty(), "{path}");
        sink.done(&done(1, 0));
    }
}

pub(super) fn edit(mode: PipelineMode, changes: DraftChanges) -> Intent {
    Intent::EditDraft { mode, changes }
}

pub(super) fn set_preset(mode: PipelineMode, preset: &str) -> Intent {
    Intent::SetPreset {
        mode,
        preset: preset.into(),
    }
}

#[tokio::test(start_paused = true)]
async fn test_draft_intents_apply_and_publish_with_the_next_publication() {
    let (core, publisher) = spawn_core();
    // Before any publication, `get_state` already holds each mode's draft and the catalog.
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.draft, SettingsDraft::new(REMUX));
    assert_eq!(state.extract.draft, SettingsDraft::new(EXTRACT));
    assert_eq!(state.presets, get_preset_profiles());

    let typed = DraftChanges {
        input_dir: Some("/anime".into()),
        bitrate: Some("256k".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(REMUX, typed.clone())).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let state = core.state().await.unwrap();
    assert_eq!(&state, publisher.states().last().unwrap());
    assert_eq!(state.remux.draft.input_dir, "/anime");
    assert_eq!(state.remux.draft.preset.as_deref(), Some("custom"));
    // The other mode keeps its own draft.
    assert_eq!(state.extract.draft, SettingsDraft::new(EXTRACT));
    let published = publisher.states().len();
    assert_eq!(core.dispatch(edit(REMUX, typed)).await, Ok(false));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert_eq!(
        publisher.states().len(),
        published,
        "nothing changed, nothing published"
    );

    assert_eq!(core.dispatch(set_preset(REMUX, "hifi")).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let draft = core.state().await.unwrap().remux.draft;
    assert_eq!(draft.bitrate.as_deref(), Some("320k"));
    assert!(core.dispatch(set_preset(REMUX, "studio")).await.is_err());
    assert!(core.dispatch(set_preset(EXTRACT, "fast")).await.is_err());
    let assets = DraftChanges {
        assets_dir: Some("/subs".into()),
        ..DraftChanges::default()
    };
    assert!(core.dispatch(edit(EXTRACT, assets)).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn test_a_burst_of_draft_intents_answers_each_and_is_coalesced_into_the_publications() {
    let (core, publisher) = spawn_core();
    let begun = Instant::now();
    let typed = |step: u32| DraftChanges {
        input_dir: Some(format!("/anime{step}")),
        ..DraftChanges::default()
    };
    for step in 0..1_000 {
        assert_eq!(core.dispatch(edit(REMUX, typed(step))).await, Ok(true));
        // Spread the burst over about a second of paused time.
        if step % 10 == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;

    let elapsed = begun.elapsed();
    let states = publisher.states();
    let bound = elapsed.as_millis().div_ceil(PUBLISH_INTERVAL.as_millis()) + 1;
    assert!(
        states.len() as u128 <= bound,
        "{} publications in {elapsed:?}",
        states.len()
    );
    let last = states.last().unwrap();
    assert_eq!(last.remux.draft.input_dir, "/anime999");
    assert_eq!(&core.state().await.unwrap(), last);

    // An Intent of another kind still publishes the pending draft before it answers. The first
    // edit goes out on the leading edge, so the second lands inside the interval and waits.
    assert_eq!(core.dispatch(edit(REMUX, typed(1_000))).await, Ok(true));
    assert_eq!(core.dispatch(edit(REMUX, typed(0))).await, Ok(true));
    let published = publisher.states().len();
    let last = publisher
        .states()
        .last()
        .unwrap()
        .remux
        .draft
        .input_dir
        .clone();
    assert_eq!(last, "/anime1000");
    let select = Intent::SetAllSelected {
        mode: REMUX,
        selected: true,
    };
    assert_eq!(core.dispatch(select).await, Ok(false));
    let states = publisher.states();
    assert_eq!(states.len(), published + 1);
    assert_eq!(states.last().unwrap().remux.draft.input_dir, "/anime0");
}

#[tokio::test(start_paused = true)]
async fn test_a_running_job_refuses_draft_intents() {
    let (core, _publisher) = spawn_core();
    let sink = core.job_sink();
    sink.started(&started(1));
    let workers = || DraftChanges {
        workers: Some("4".into()),
        ..DraftChanges::default()
    };
    for mode in [REMUX, EXTRACT] {
        let error = core.dispatch(edit(mode, workers())).await.unwrap_err();
        assert!(error.contains("running"), "{error}");
    }
    let error = core.dispatch(set_preset(REMUX, "fast")).await.unwrap_err();
    assert!(error.contains("running"), "{error}");
    assert_eq!(
        core.state().await.unwrap().remux.draft,
        SettingsDraft::new(REMUX)
    );

    sink.done(&done(1, 0));
    assert_eq!(core.dispatch(edit(REMUX, workers())).await, Ok(true));
}

#[tokio::test]
async fn test_the_defaults_are_published_once_the_actor_has_read_them() {
    let publisher = RecordingPublisher::default();
    let ports = Ports {
        inspector: FakeInspector::default(),
        launcher: FakeLauncher::default(),
        sampler: NoSampler,
    };
    let core = Core::spawn(Handle::current(), publisher.clone(), ports);
    // Read on the blocking pool after the spawn, then published at once: each publication
    // wakes the wait, and the timeout only bounds a hang.
    let published = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let defaults = publisher
                .states()
                .into_iter()
                .find_map(|state| state.defaults);
            if defaults.is_some() {
                return defaults;
            }
            publisher.stated.notified().await;
        }
    })
    .await
    .expect("the defaults were never published");
    assert_eq!(published, Some(EffectiveSettings::defaults()));
    assert_eq!(
        core.state().await.unwrap().defaults,
        Some(EffectiveSettings::defaults())
    );
}

/// What a `FakeLauncher` was told, and how it answers.
#[derive(Default)]
pub(super) struct LauncherScript {
    /// Each call, in order: `resolve`, `launch`, `stop None` or `stop Some(id)`.
    calls: Vec<String>,
    launched: Vec<BatchRequest>,
    pub(super) resolve_error: Option<String>,
    pub(super) launch_error: Option<String>,
    pub(super) stop_error: Option<String>,
    /// Ends each job with this `job-done` code before its launch returns.
    pub(super) end_at_launch: Option<i32>,
    /// The jobs still running: ID, mode and sink.
    pub(super) jobs: Vec<(u64, &'static str, ActorSink)>,
    next_id: u64,
}

/// A `Launcher` that answers from a script, after a delay on the paused clock. A launched job
/// runs until the test ends it or a Stop does, which ends it cancelled before the Stop
/// settles, as the supervisor's `done` precedes the slot release.
#[derive(Clone, Default)]
pub(super) struct FakeLauncher(Arc<Mutex<LauncherScript>>);

pub(super) const RESOLVE: Duration = Duration::from_millis(100);
pub(super) const LAUNCH: Duration = Duration::from_millis(100);
pub(super) const STOP: Duration = Duration::from_millis(50);

impl FakeLauncher {
    pub(super) fn script(&self) -> std::sync::MutexGuard<'_, LauncherScript> {
        self.0.lock().unwrap()
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.script().calls.clone()
    }

    pub(super) fn launched(&self) -> Vec<BatchRequest> {
        self.script().launched.clone()
    }

    /// Ends the running job with `code`, as its supervisor would.
    pub(super) fn end_job(&self, code: i32) {
        let (job_id, mode, sink) = self.script().jobs.remove(0);
        sink.done(&JobDone { mode, job_id, code });
    }
}

impl Launcher for FakeLauncher {
    async fn resolve(&self, _request: BatchRequest) -> Result<EffectiveSettings, String> {
        self.script().calls.push("resolve".into());
        tokio::time::sleep(RESOLVE).await;
        match self.script().resolve_error.clone() {
            Some(error) => Err(error),
            None => Ok(effective_settings()),
        }
    }

    async fn launch(&self, request: BatchRequest, sink: ActorSink) -> Result<u64, String> {
        self.script().calls.push("launch".into());
        tokio::time::sleep(LAUNCH).await;
        let mut script = self.script();
        if let Some(error) = script.launch_error.clone() {
            return Err(error);
        }
        script.next_id += 1;
        let job_id = script.next_id;
        let mode = mode_name(request.mode);
        sink.started(&JobStarted {
            job_id,
            mode,
            input_dir: request.input_dir.to_string_lossy().into_owned(),
            effective_settings: effective_settings(),
            start_time_ms: 1_700_000_000_000,
        });
        script.launched.push(request);
        match script.end_at_launch {
            Some(code) => sink.done(&JobDone { mode, job_id, code }),
            None => script.jobs.push((job_id, mode, sink)),
        }
        Ok(job_id)
    }

    async fn stop(&self, job_id: Option<u64>) -> Result<(), String> {
        self.script().calls.push(format!("stop {job_id:?}"));
        tokio::time::sleep(STOP).await;
        let mut script = self.script();
        if let Some(error) = script.stop_error.clone() {
            return Err(error);
        }
        if let Some(at) = script.jobs.iter().position(|job| Some(job.0) == job_id) {
            let (job_id, mode, sink) = script.jobs.remove(at);
            sink.done(&JobDone {
                mode,
                job_id,
                code: -1,
            });
        }
        Ok(())
    }
}

/// A manifest of a FLAC episode at the root and two in `Season 2`, under `/media/Show`.
fn show_manifest() -> InspectionManifest {
    let mut manifest = manifest_of(&["ep1", "Season 2/ep2", "Season 2/ep3"]);
    for (item, release) in manifest.items.iter_mut().zip([".", "Season 2", "Season 2"]) {
        item.release = release.into();
        item.metadata.audio_summary = Some("FLAC 2.0".into());
        item.metadata.has_flac = Some(true);
    }
    manifest
}

#[tokio::test(start_paused = true)]
async fn test_a_scanned_mode_publishes_its_header_counts_progress_and_library() {
    let inspector = FakeInspector::default().with("/media/Show", show_manifest());
    let core = ready(RecordingPublisher::default(), inspector);
    assert_eq!(
        core.dispatch(scan(REMUX, "/media/Show", false)).await,
        Ok(true)
    );
    let selected = Intent::SetSelected {
        mode: REMUX,
        id: "Season 2/ep3".into(),
        selected: false,
    };
    assert_eq!(core.dispatch(selected).await, Ok(true));
    let view = core.state().await.unwrap().remux;
    assert_eq!(
        (view.title.as_str(), view.crumbs.as_slice()),
        ("Show", &["media".to_string(), "Show".into()][..])
    );
    assert_eq!(view.count_text, "2 / 3 Selected");
    assert_eq!(view.clock, "Not started");
    assert_eq!((view.selected_count, view.total_count), (2, 3));
    assert_eq!(
        view.filters,
        Filters {
            all: 3,
            todo: 2,
            done: 0,
            failed: 0
        }
    );
    assert_eq!(view.progress, Progress::default());
    let library: Vec<_> = view
        .library
        .iter()
        .map(|entry| (entry.release.as_deref(), entry.label.as_str(), entry.count))
        .collect();
    assert_eq!(
        library,
        [(None, "Show", 3), (Some("Season 2"), "Season 2", 2)]
    );
    assert_eq!(view.rows[2].bucket, None);
    assert_eq!(
        view.rows[0].meta,
        "FLAC 2.0 → Opus 192k · No subs · 0 fonts"
    );
    // The other mode has no folder open.
    let extract = core.state().await.unwrap().extract;
    assert_eq!(
        (extract.title.as_str(), extract.count_text.as_str()),
        ("No folder open", "Ready")
    );
}

#[tokio::test(start_paused = true)]
async fn test_a_bitrate_or_assets_dir_edit_redraws_the_remux_meta_lines_and_another_edit_no_rows() {
    let inspector = FakeInspector::default().with("/media/Show", show_manifest());
    let publisher = RecordingPublisher::default();
    let core = ready(publisher.clone(), inspector);
    core.dispatch(scan(REMUX, "/media/Show", false))
        .await
        .unwrap();
    let rows = || async { core.state().await.unwrap().remux.rows };
    let before = rows().await;

    // Not the bitrate or the assets dir: the rows are the ones last built.
    let out_dir = DraftChanges {
        out_dir: Some("/out".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(REMUX, out_dir)).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert!(Arc::ptr_eq(&before, &rows().await));
    // Spaces around the bitrate change nothing a row shows, so no rows either.
    let padded = DraftChanges {
        bitrate: Some(" 192k ".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(REMUX, padded)).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert!(Arc::ptr_eq(&before, &rows().await));

    let bitrate = DraftChanges {
        bitrate: Some("256k".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(REMUX, bitrate)).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let after = rows().await;
    assert_eq!(after[0].meta, "FLAC 2.0 → Opus 256k · No subs · 0 fonts");
    let assets = DraftChanges {
        assets_dir: Some("/assets".into()),
        ..DraftChanges::default()
    };
    assert_eq!(core.dispatch(edit(REMUX, assets)).await, Ok(true));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert_eq!(
        rows().await[0].meta,
        "FLAC 2.0 → Opus 256k · Assets dir fallback · 0 fonts"
    );
    // The extract queue saw none of it.
    assert!(publisher
        .states()
        .windows(2)
        .all(|pair| Arc::ptr_eq(&pair[0].extract.rows, &pair[1].extract.rows)));
}

// The inspector shows the first row until a pick, then the pick.
#[tokio::test(start_paused = true)]
async fn test_an_inspect_intent_picks_the_inspected_row_and_publishes_before_it_answers() {
    let (core, publisher, inspector) = scanning_core();
    assert_eq!(core.state().await.unwrap().remux.inspector, None);
    core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
    let shown = || async { core.state().await.unwrap().remux.inspector.unwrap() };
    assert_eq!(
        (shown().await.id, shown().await.number),
        ("ep1".into(), "01".into())
    );

    let inspect = |id: Option<&str>| Intent::Inspect {
        mode: REMUX,
        id: id.map(str::to_string),
    };
    assert_eq!(core.dispatch(inspect(Some("ep2"))).await, Ok(true));
    // Published before the answer.
    let last = publisher.states().last().unwrap().clone();
    assert_eq!(last.remux.inspector.unwrap().id, "ep2");
    assert_eq!(shown().await.number, "02");
    assert_eq!(core.dispatch(inspect(Some("ep2"))).await, Ok(false));
    assert_eq!(core.dispatch(inspect(Some("ep9"))).await, Ok(false));
    assert_eq!(shown().await.id, "ep2");
    // The other mode's pick is its own.
    assert_eq!(core.state().await.unwrap().extract.inspector, None);

    // A draft edit the inspector shows redraws it, and no row.
    let rows = core.state().await.unwrap().remux.rows;
    let out_dir = DraftChanges {
        out_dir: Some("/out".into()),
        ..DraftChanges::default()
    };
    core.dispatch(edit(REMUX, out_dir)).await.unwrap();
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert_eq!(shown().await.out_crumbs, Some(vec!["out".to_string()]));
    assert!(Arc::ptr_eq(&rows, &core.state().await.unwrap().remux.rows));

    // A rescan that loses the pick shows the first row again.
    inspector
        .manifests
        .lock()
        .unwrap()
        .insert("/anime".into(), manifest_of(&["ep1", "ep3"]));
    core.dispatch(scan(REMUX, "/anime", true)).await.unwrap();
    assert_eq!(shown().await.id, "ep1");
    assert_eq!(core.dispatch(inspect(Some("ep3"))).await, Ok(true));
    assert_eq!(core.dispatch(inspect(None)).await, Ok(true));
    assert_eq!(shown().await.id, "ep1");
}

#[tokio::test(start_paused = true)]
async fn test_a_step_draft_intent_steps_from_the_published_defaults_and_is_coalesced() {
    let (core, publisher) = spawn_core();
    let step = |mode, field, direction| Intent::StepDraft {
        mode,
        field,
        direction,
    };
    let published = publisher.states().len();
    // `effective_settings()` are the defaults: 3 workers. A burst of steps answers each without
    // a flush: only the leading edge publishes before the interval.
    for _ in 0..5 {
        assert_eq!(
            core.dispatch(step(REMUX, StepField::Workers, Direction::Up))
                .await,
            Ok(true)
        );
    }
    assert!(publisher.states().len() <= published + 1);
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    assert_eq!(core.state().await.unwrap().remux.draft.workers, "8");
    assert!(core
        .dispatch(step(EXTRACT, StepField::SubOffset, Direction::Up))
        .await
        .is_err());

    let sink = core.job_sink();
    sink.started(&started(1));
    let error = core
        .dispatch(step(REMUX, StepField::IoJobs, Direction::Down))
        .await
        .unwrap_err();
    assert!(error.contains("running"), "{error}");
}

// The controls and the settings chips, as the actor lays them over its views.
#[tokio::test(start_paused = true)]
async fn test_the_controls_and_chips_follow_the_batch_holder_and_the_queue() {
    let (core, _publisher, _inspector) = scanning_core();
    let state = core.state().await.unwrap();
    assert_eq!(state.remux.controls.run_label, "Run remux");
    assert_eq!(state.chips.workers, "Standby (3 Workers)");
    assert_eq!(state.chips.io_jobs_hint.as_deref(), Some("auto (5)"));
    core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
    assert_eq!(
        core.state().await.unwrap().remux.controls.run_label,
        "Run remux · 2"
    );

    let sink = core.job_sink();
    sink.started(&started(1));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let state = core.state().await.unwrap();
    assert!(state.remux.controls.owns && state.remux.controls.selection_locked);
    assert_eq!(state.remux.controls.status.label, "Running");
    assert_eq!(state.extract.controls.run_title, "Remux batch is running");
    assert!(state.extract.controls.run_disabled);
    // No run settings: the job's own Effective Settings.
    assert_eq!(state.chips.workers, "3 Workers Active");

    sink.done(&done(1, 0));
    tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
    let state = core.state().await.unwrap();
    assert!(!state.remux.controls.owns);
    assert_eq!(state.remux.controls.status.label, "Done");
    assert_eq!(state.chips.workers, "Standby (3 Workers)");
}
