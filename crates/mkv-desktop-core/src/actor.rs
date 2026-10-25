//! The Desktop Core actor (ADR-0016): one task that owns the queue and the Settings Draft of each
//! mode, the Job Coordinator, the published Job Snapshot and the scans in flight, and publishes
//! the whole View State through a `Publisher`, at most about 15 times a second. Everything
//! reaches it through one inbox, so a job's start, events, log batches and completion arrive in
//! the order the supervisor sent them, and an Intent sees every change sent before it. A UI
//! adapter spawns it once with `Core::spawn` and talks to it through the `CoreHandle`. The start
//! of a Run and the Stop that ends it are in `start`, the telemetry cadence and the Presentation
//! Heartbeat in `timers`, the presentation values and controls in `crate::presentation`, the
//! Episode Inspector in `crate::inspector`. Each mode's Run Log is published batch by batch,
//! outside the View State.

use crate::control::{mode_name, EngineLauncher, InspectCacheState, Launcher, ScanState};
use crate::coordinator::{JobCoordinator, Phase};
use crate::draft::{DraftChanges, SettingsDraft, StepField};
use crate::inspection::{EngineInspector, InFlight, Inspector};
use crate::inspector::{
    inspected_row, inspector_view, subtitle_assignment, InspectorView, SubtitleFields,
};
use crate::job::{JobDone, JobSink, JobStarted, JobState};
use crate::log::{error_line, LogBatch, LogView, RunLog};
use crate::presentation::{
    batch, batch_clock, chips, controls, count_text, empty_list, filter_counts, library,
    list_crumbs, list_title, progress_segments, Chips, Controls, EmptyList, Filters, Hold,
    LibraryEntry, Progress,
};
use crate::queue::{Direction, Queue, Row};
use crate::snapshot::{BatchStatus, JobSnapshot};
use crate::telemetry::{EngineSampler, Sampler, TelemetryData, TelemetryView};
use mkv_core::log_line::{LogKind, LogLine};
use mkv_core::manifest::InspectionManifest;
use mkv_core::pipeline::{JobEvent, PipelineMode};
use mkv_core::preset::{get_preset_profiles, PresetProfile};
use mkv_core::request::{EffectiveSettings, TrackAssignment};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant as StdInstant};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

mod start;
mod timers;
// A view tells these refusals, which come before the draft is read, from a draft refusal.
use start::{Start, Step};
pub use start::{RUN_BUSY, RUN_SCANNING};
use timers::Telemetry;

/// The shortest gap between two View State publications: at most about 15 a second.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(66);

/// Where the actor publishes: a UI adapter's seam (the Tauri one emits to the webview).
pub trait Publisher: Send + 'static {
    /// The whole View State, at most once per `PUBLISH_INTERVAL` except for the flush before a
    /// `done` or the reply to an Intent that is not a draft Intent.
    fn state(&self, state: &ViewState);
    /// What a mode's Run Log appended or a clear of it, as soon as it happens.
    fn logs(&self, batch: &LogBatch);
    /// A job's end, after the publication of the state it ended in.
    fn done(&self, done: &JobDone);
}

/// What a view asks the core to do. Each names the mode it acts on, except `ViewVisible`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Intent {
    /// Scan `path` into the queue: resolve it to its Directory Identity, reuse the manifest
    /// already held for it unless `force`, else inspect it. Answers once the scan settles:
    /// `true` when the queue took the inspection, `false` when a newer scan of the mode overtook
    /// it, it failed, or an empty `path` reset the queue; an error when a job is running in the
    /// same mode, or a start of either mode is before its launch.
    ScanDirectory {
        mode: PipelineMode,
        path: String,
        force: bool,
        #[serde(default)]
        no_cache: bool,
    },
    SetSelected {
        mode: PipelineMode,
        id: String,
        selected: bool,
    },
    SetAllSelected {
        mode: PipelineMode,
        selected: bool,
    },
    Reorder {
        mode: PipelineMode,
        id: String,
        direction: Direction,
    },
    /// Assigns the subtitle `path` to item `id` (remux only), with the Episode Inspector's
    /// fields as typed: the language select's value, the custom tag, the title and the offset in
    /// milliseconds. The core builds the Track Assignment; see `subtitle_assignment`.
    AssignSubtitle {
        mode: PipelineMode,
        id: String,
        path: String,
        language: String,
        custom_language: String,
        title: String,
        offset: String,
    },
    ClearTrack {
        mode: PipelineMode,
        id: String,
    },
    /// Sets the Settings Draft fields `changes` names; see `SettingsDraft::edit`.
    EditDraft {
        mode: PipelineMode,
        changes: DraftChanges,
    },
    /// Steps a Settings Draft field up or down, as an `EditDraft` of it; see `SettingsDraft::step`.
    StepDraft {
        mode: PipelineMode,
        field: StepField,
        direction: Direction,
    },
    /// Puts the Settings Draft on a Preset Profile (remux only); see `SettingsDraft::set_preset`.
    SetPreset {
        mode: PipelineMode,
        preset: String,
    },
    /// Runs the mode's batch: builds the request from its Settings Draft, resolves it, scans the
    /// input directory again (forced), checks the Queue Selection against that scan, and launches
    /// the job. Answers once the start settles: `true` when the job launched, `false` when a Stop
    /// ended the start before that, and the reason when the draft, the request, the scan, the
    /// preflight or the launch failed. Refused while another start or job holds the batch or the
    /// mode scans.
    Run {
        mode: PipelineMode,
    },
    /// Stops the mode's start or job. Answers once the teardown is over: `true` when it stopped
    /// or the start was cancelled, `false` when the mode owns no batch, and the teardown's error
    /// when it failed, which leaves the job running.
    Stop {
        mode: PipelineMode,
    },
    /// Empties the session inspect cache and deletes its file, and says so, or why it failed, in
    /// the mode's Run Log. Answers `true` once it is cleared, else the error.
    ClearCache {
        mode: PipelineMode,
    },
    /// Shows row `id` in the mode's Episode Inspector, or with `None` the first row again.
    /// Answers whether that changed what it picked: `false` for a row the queue does not hold.
    Inspect {
        mode: PipelineMode,
        id: Option<String>,
    },
    /// The view was shown or hidden: telemetry is not sampled while it is hidden. Answers whether
    /// that changed, at once and without a publication of its own, like a draft Intent.
    ViewVisible {
        visible: bool,
    },
}

/// What the core publishes. `seq` grows by one with every publication, so a view can drop a
/// stale one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewState {
    pub seq: u64,
    pub job: Option<JobView>,
    pub run: RunView,
    /// Whether a start or a job holds the batch, which locks both queues and both drafts: the
    /// run phase is not idle, or a job is active.
    pub busy: bool,
    /// The Effective Settings of a request that sets nothing, for the `auto (N)` hints: `None`
    /// until the actor has read the CPU topology once it started.
    pub defaults: Option<EffectiveSettings>,
    /// The worker and I/O job chips and the `auto (N)` hints.
    pub chips: Chips,
    /// The built-in Preset Profiles.
    pub presets: Vec<PresetProfile>,
    pub remux: ModeView,
    pub extract: ModeView,
    /// The System Telemetry Provider's last sample and history: `None` until the first.
    pub telemetry: Option<TelemetryView>,
}

/// The Job Coordinator as a view renders it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunView {
    pub phase: Phase,
    /// The mode that owns the batch while it is not idle.
    pub mode: Option<PipelineMode>,
    /// The Effective Settings of the owning mode's request, from its resolve until the start or
    /// the job ends.
    pub settings: Option<EffectiveSettings>,
}

/// How a mode's last start or job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Cancelled,
    Failed,
}

impl Outcome {
    /// The outcome of a `job-done` code: `0` completed, `-1` cancelled, else failed.
    fn of_code(code: i32) -> Self {
        match code {
            0 => Self::Done,
            -1 => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// The published Job Snapshot, without its items: those are in the rows of its mode.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobView {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    /// Whether the job still runs: from its start until its `job-done`, or until
    /// its sink is dropped without one.
    pub active: bool,
    pub input_dir: String,
    pub effective_settings: EffectiveSettings,
}

/// The queue of one mode as a view renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModeView {
    /// The Directory Identity the queue shows, if any.
    pub directory: Option<String>,
    pub scan: ScanView,
    /// Changes whenever the manifest behind `rows` is replaced.
    pub manifest_rev: u64,
    /// External subtitles a human can assign, by Release Directory.
    pub subtitle_candidates: BTreeMap<String, Vec<String>>,
    /// Shared, so a copy of an unchanged mode, such as the one a webview adapter keeps to diff
    /// against, is a pointer copy. Serializes as the list itself.
    pub rows: Arc<Vec<Row>>,
    pub vanished_selected_ids: Vec<String>,
    /// Track Assignments by item ID (remux only).
    pub assignments: BTreeMap<String, TrackAssignment>,
    pub draft: SettingsDraft,
    /// How the mode's last start or job ended: `None` before the first and while one runs.
    pub outcome: Option<Outcome>,
    /// The list header: the folder's name, or `No folder open`.
    pub title: String,
    /// The list header's path crumbs: the folder's path segments.
    pub crumbs: Vec<String>,
    /// The queue count beside the header: the scan, the Queue Selection or the batch's tally.
    pub count_text: String,
    /// The bottom bar's batch clock; it advances with the Presentation Heartbeat.
    pub clock: String,
    /// The stacked progress bar's widths, in percent.
    pub progress: Progress,
    /// How many rows each filter chip shows.
    pub filters: Filters,
    pub selected_count: usize,
    pub total_count: usize,
    /// The Library: the folder, then each other Release Directory; empty with no folder open.
    pub library: Vec<LibraryEntry>,
    /// The empty list's card: set only while the queue has no rows and its scan is idle.
    pub empty: Option<EmptyList>,
    /// The Episode Inspector: `None` while the queue has no rows.
    pub inspector: Option<InspectorView>,
    /// The Run and Stop buttons, the status pill and the selection lock.
    pub controls: Controls,
}

impl ViewState {
    /// What a core that holds nothing yet publishes: empty queues, each mode's draft as it
    /// starts, the catalog and `defaults`.
    pub fn new(defaults: Option<EffectiveSettings>) -> Self {
        let mut state = Self {
            seq: 0,
            job: None,
            run: RunView::default(),
            busy: false,
            chips: Chips::default(),
            defaults,
            presets: get_preset_profiles(),
            remux: ModeState::new(PipelineMode::Remux).view(None, StdInstant::now()),
            extract: ModeState::new(PipelineMode::Extract).view(None, StdInstant::now()),
            telemetry: None,
        };
        state.lay_controls(&Hold::default(), None);
        state
    }

    /// Lays each mode's controls over its view and the chips, while `hold` holds the batch,
    /// whose Effective Settings are `batch` once known.
    fn lay_controls(&mut self, hold: &Hold, batch: Option<&EffectiveSettings>) {
        self.remux.controls = controls(PipelineMode::Remux, &self.remux, hold);
        self.extract.controls = controls(PipelineMode::Extract, &self.extract, hold);
        self.chips = chips(hold.busy, batch, self.defaults.as_ref());
    }
}

/// Where a mode's scan stands.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScanView {
    #[default]
    Idle,
    Scanning {
        directory: String,
    },
    /// The last scan failed; the rows are those from before it.
    Failed,
}

type Reply = oneshot::Sender<Result<bool, String>>;

/// Who a scan answers: the Intent that asked for it, or the preflight of start `attempt`, whose
/// forced scan no start or job refuses.
enum ScanReply {
    Intent(Reply),
    Preflight(u64),
}

enum Msg {
    GetState(oneshot::Sender<ViewState>),
    GetLog(PipelineMode, oneshot::Sender<LogView>),
    Dispatch(Intent, Reply),
    /// A scan's path resolved to `directory` (as typed when it has no identity).
    Resolved {
        mode: PipelineMode,
        request: u64,
        directory: String,
        force: bool,
        no_cache: bool,
        reply: ScanReply,
    },
    /// In-flight inspection `id` ended.
    Inspected {
        id: u64,
        result: Result<InspectionManifest, String>,
    },
    JobStarted(JobStarted),
    JobEvent {
        job_id: u64,
        event: JobEvent,
    },
    /// A log batch of a job of `mode`.
    Logs {
        mode: PipelineMode,
        lines: Vec<LogLine>,
    },
    Done(JobDone),
    /// A job's sink was dropped: after its `Done`, or without one if its supervisor died.
    SinkClosed {
        job_id: u64,
    },
    /// The Effective Settings defaults, read off the actor.
    Defaults(EffectiveSettings),
    /// Start `attempt`'s request resolved, or was refused.
    RequestResolved {
        attempt: u64,
        result: Result<EffectiveSettings, String>,
    },
    /// Start `attempt`'s launch returned its job ID, or why it started none.
    Launched {
        attempt: u64,
        result: Result<u64, String>,
    },
    /// The teardown a Stop of start `attempt`, of `mode`, issued is over.
    StopSettled {
        attempt: u64,
        mode: PipelineMode,
        result: Result<(), String>,
    },
    /// A `ClearCache` of `mode` is over.
    CacheCleared {
        mode: PipelineMode,
        result: Result<(), String>,
        reply: Reply,
    },
    /// A telemetry sample came back.
    Sampled(Result<TelemetryData, String>),
}

/// The way into the actor. Clones share its one inbox.
#[derive(Clone)]
pub struct CoreHandle(mpsc::UnboundedSender<Msg>);

impl CoreHandle {
    fn send(&self, msg: Msg) {
        // Only a stopped runtime drops the actor, and nothing is left to show then.
        let _ = self.0.send(msg);
    }

    /// Sends `msg` at call time and returns its answer, so messages reach the actor in call
    /// order however their answers are awaited, or if they are dropped.
    fn ask<T: Send + 'static>(
        &self,
        msg: impl FnOnce(oneshot::Sender<T>) -> Msg,
    ) -> impl Future<Output = Result<T, String>> + Send + 'static {
        let (reply, answer) = oneshot::channel();
        self.send(msg(reply));
        async move {
            answer
                .await
                .map_err(|_| "The Desktop Core is not running.".to_string())
        }
    }

    /// A `JobSink` for one job that feeds this actor.
    pub(crate) fn job_sink(&self) -> ActorSink {
        ActorSink {
            core: self.clone(),
            job: OnceLock::new(),
        }
    }

    /// The last published View State. Asked at call time, as `dispatch` is.
    pub fn state(&self) -> impl Future<Output = Result<ViewState, String>> + Send + 'static {
        self.ask(Msg::GetState)
    }

    /// The whole of `mode`'s Run Log, as every batch published so far left it. Asked at call
    /// time, as `dispatch` is.
    pub fn log(
        &self,
        mode: PipelineMode,
    ) -> impl Future<Output = Result<LogView, String>> + Send + 'static {
        self.ask(move |reply| Msg::GetLog(mode, reply))
    }

    /// Applies `intent`. Its effect is published before the answer, except that a draft Intent
    /// (`EditDraft`, `StepDraft`, `SetPreset`) and `ViewVisible` answer at once and go out with
    /// the next coalesced publication. The answer is `Ok(true)` when it applied, `Ok(false)` when
    /// it changed nothing (see `Intent::ScanDirectory`, `Intent::Run` and `Intent::Stop` for those), and an error
    /// when the core refuses it.
    ///
    /// The Intent is sent at call time, so Intents reach the actor in call order; dropping the
    /// returned future drops only the answer, never the Intent.
    pub fn dispatch(
        &self,
        intent: Intent,
    ) -> impl Future<Output = Result<bool, String>> + Send + 'static {
        let answer = self.ask(|reply| Msg::Dispatch(intent, reply));
        async move { answer.await? }
    }
}

/// The `JobSink` of one job: turns what the supervisor reports into inbox messages, in its order.
pub struct ActorSink {
    core: CoreHandle,
    /// The job's ID and mode, from its start on.
    job: OnceLock<(u64, Option<PipelineMode>)>,
}

impl JobSink for ActorSink {
    fn started(&self, job: &JobStarted) {
        let _ = self.job.set((job.job_id, mode_of(job.mode)));
        self.core.send(Msg::JobStarted(job.clone()));
    }

    fn event(&self, event: &JobEvent) {
        // `JobState::start` reports the start before the supervisor sends any event or log.
        if let Some(&(job_id, _)) = self.job.get() {
            self.core.send(Msg::JobEvent {
                job_id,
                event: event.clone(),
            });
        }
    }

    fn logs(&self, lines: &[LogLine]) {
        // Into the Run Log of the job's mode, whichever mode the view shows.
        if let Some(&(_, Some(mode))) = self.job.get() {
            self.core.send(Msg::Logs {
                mode,
                lines: lines.to_vec(),
            });
        }
    }

    fn done(&self, done: &JobDone) {
        self.core.send(Msg::Done(done.clone()));
    }
}

impl Drop for ActorSink {
    fn drop(&mut self) {
        // The supervisor owns the sink, so this reaches the actor even when it dies before `done`.
        if let Some(&(job_id, _)) = self.job.get() {
            self.core.send(Msg::SinkClosed { job_id });
        }
    }
}

/// One mode's queue, scan, Settings Draft, outcome and Run Log.
struct ModeState {
    mode: PipelineMode,
    queue: Queue,
    draft: SettingsDraft,
    log: RunLog,
    scan: ScanView,
    outcome: Option<Outcome>,
    /// The row an `Inspect` Intent picked for the Episode Inspector, if any.
    inspected: Option<String>,
    /// The newest scan request: a request answered after a newer one began is overtaken.
    request: u64,
    /// The scan the queue waits for: an inspection that settles after a newer scan began, a reuse
    /// or a reset is overtaken.
    token: u64,
    /// Whether the queue, the scan, the Job Snapshot laid over the rows, the draft values the rows
    /// show or the time they show (a heartbeat beat) changed since the last publication. Only a
    /// changed mode's view is rebuilt; the other is published as last built.
    changed: bool,
}

impl ModeState {
    fn new(mode: PipelineMode) -> Self {
        Self {
            mode,
            queue: Queue::default(),
            draft: SettingsDraft::new(mode),
            log: RunLog::new(mode),
            scan: ScanView::Idle,
            outcome: None,
            inspected: None,
            request: 0,
            token: 0,
            changed: false,
        }
    }

    /// The view of this mode, with `job`, the Job Snapshot of this mode if any, and when it
    /// started, over its rows, presented at `now`. With no job, the time does not show.
    fn view(&mut self, job: Option<(&JobSnapshot, StdInstant)>, now: StdInstant) -> ModeView {
        let inputs = self.draft.meta_inputs(self.mode);
        let rows = self.queue.rows(job.map(|(job, _)| job), &inputs, now);
        let batch = job.and_then(|(job, started_at)| batch(job, started_at, now));
        let queue = &self.queue;
        let directory = queue.directory();
        ModeView {
            directory: directory.map(str::to_string),
            scan: self.scan.clone(),
            manifest_rev: queue.manifest_rev(),
            subtitle_candidates: queue.subtitle_candidates(),
            vanished_selected_ids: queue.vanished_selected_ids().to_vec(),
            assignments: queue.assignments().clone(),
            draft: self.draft.clone(),
            outcome: self.outcome,
            title: list_title(directory),
            crumbs: list_crumbs(directory),
            count_text: count_text(&self.scan, &rows, batch.as_ref()),
            clock: batch_clock(batch.as_ref()),
            progress: progress_segments(&rows, batch.as_ref()),
            filters: filter_counts(&rows),
            selected_count: rows.iter().filter(|row| row.selected).count(),
            total_count: rows.len(),
            library: library(directory, &rows),
            empty: empty_list(directory, &self.scan, &rows),
            rows: Arc::new(rows),
            // Laid over the view on each publication: they follow the draft and the batch's holder.
            inspector: None,
            controls: Controls::default(),
        }
    }
}

/// A scan waiting on an in-flight inspection.
struct Waiter {
    mode: PipelineMode,
    token: u64,
    directory: String,
    reply: ScanReply,
}

/// The ports a UI adapter spawns the actor with, besides its `Publisher`.
pub struct Ports<I, L, S> {
    /// Scans directories and owns the session inspect cache.
    pub inspector: I,
    /// Runs and stops jobs.
    pub launcher: L,
    /// Samples the System Telemetry Provider.
    pub sampler: S,
}

impl Ports<EngineInspector, EngineLauncher, EngineSampler> {
    /// The engine ports of a desktop host. Scans and Runs share `scan` and `cache`, which the
    /// host's Close also tears down; jobs run through `job`, the Job Slot.
    pub fn engine(scan: Arc<ScanState>, cache: InspectCacheState, job: JobState) -> Self {
        Self {
            inspector: EngineInspector {
                scan: Arc::clone(&scan),
                cache: cache.clone(),
            },
            launcher: EngineLauncher { job, scan, cache },
            sampler: EngineSampler::default(),
        }
    }
}

/// The actor's state.
pub struct Core<P, I, L, S> {
    publisher: P,
    inspector: Arc<I>,
    launcher: Arc<L>,
    sampler: Arc<S>,
    runtime: Handle,
    /// Where the scans the actor spawns send their results; weak, so the loop still ends once
    /// every `CoreHandle` is gone.
    inbox: mpsc::WeakUnboundedSender<Msg>,
    remux: ModeState,
    extract: ModeState,
    in_flight: InFlight<Waiter>,
    snapshot: Option<JobSnapshot>,
    /// When the actor heard the snapshot's job start: the origin of its batch clock and of the
    /// Presentation Heartbeat's beats.
    job_started_at: Option<Instant>,
    /// When the Presentation Heartbeat beats next: `None` while it does not run.
    beat_at: Option<Instant>,
    /// Whether a job runs: from its `JobStarted` until its `Done`, or until its sink is dropped
    /// when a supervisor that died before `finish` sent no `Done`.
    active: bool,
    coordinator: JobCoordinator,
    start: Option<Start>,
    /// The number of the last start: a message of an older one is stale.
    attempt: u64,
    /// The published `RunView::settings`.
    run_settings: Option<EffectiveSettings>,
    /// The Stops waiting on the teardown of their start.
    stop_waiters: Vec<(u64, Reply)>,
    defaults: Option<EffectiveSettings>,
    telemetry: Telemetry,
    /// Whether the state changed since the last publication.
    dirty: bool,
    last_published: ViewState,
    last_published_at: Option<Instant>,
}

/// The answer to a scan of the running job's own mode.
const SCAN_REFUSED: &str = "A batch is running; its folder cannot be scanned until it ends.";
/// The answer to a scan of either mode while a start runs its forced scan.
const SCAN_STARTING: &str = "A batch is starting; no folder can be scanned until it has started.";
/// The Run Log line of a cleared cache.
const CACHE_CLEARED: &str = "Persistent inspection cache cleared.";
/// The answer to a Settings Draft edit while a job runs.
const DRAFT_REFUSED: &str = "A batch is running; the settings are locked until it ends.";

impl<P: Publisher, I: Inspector, L: Launcher, S: Sampler> Core<P, I, L, S> {
    /// Spawns the actor on `runtime`, publishing through `publisher`, scanning, running jobs
    /// and sampling telemetry through `ports`.
    pub fn spawn(runtime: Handle, publisher: P, ports: Ports<I, L, S>) -> CoreHandle {
        Self::start(runtime, publisher, ports, None)
    }

    /// `spawn`, with the defaults already known when `defaults` holds them, else read on the
    /// blocking pool (`EffectiveSettings::defaults` reads the CPU topology) and published then.
    fn start(
        runtime: Handle,
        publisher: P,
        ports: Ports<I, L, S>,
        defaults: Option<EffectiveSettings>,
    ) -> CoreHandle {
        // Unbounded on purpose (ADR-0016, 2026-10-16 amendment). The engine already drops
        // `ItemProgress` it cannot send (`try_send` in `crates/mkv-core/src/pipeline.rs`), and
        // the actor only applies events in memory and publishes at most about 15 times a second,
        // so the queue drains faster than a job fills it. A bounded channel would need async `JobSink` methods and let a slow actor
        // stall the supervisor's teardown, which must never depend on the actor.
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        let core = Core {
            publisher,
            inspector: Arc::new(ports.inspector),
            launcher: Arc::new(ports.launcher),
            sampler: Arc::new(ports.sampler),
            runtime: runtime.clone(),
            inbox: inbox_tx.downgrade(),
            remux: ModeState::new(PipelineMode::Remux),
            extract: ModeState::new(PipelineMode::Extract),
            in_flight: InFlight::default(),
            snapshot: None,
            job_started_at: None,
            beat_at: None,
            active: false,
            coordinator: JobCoordinator::default(),
            start: None,
            attempt: 0,
            run_settings: None,
            stop_waiters: Vec::new(),
            // What `get_state` answers before the first publication: the drafts and the catalog
            // too.
            last_published: ViewState::new(defaults.clone()),
            defaults,
            telemetry: Telemetry::default(),
            dirty: false,
            last_published_at: None,
        };
        if core.defaults.is_none() {
            let inbox = core.inbox.clone();
            let defaults = runtime.spawn_blocking(EffectiveSettings::defaults);
            runtime.spawn(async move {
                if let (Ok(defaults), Some(inbox)) = (defaults.await, inbox.upgrade()) {
                    let _ = inbox.send(Msg::Defaults(defaults));
                }
            });
        }
        runtime.spawn(core.run(inbox));
        CoreHandle(inbox_tx)
    }

    /// The loop: handle each message, then publish at once if the last publication is at least
    /// `PUBLISH_INTERVAL` old (the leading edge), else once it is (the trailing edge, which
    /// coalesces every change made meanwhile into one publication).
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Msg>) {
        let tick = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(tick);
        let mut armed = false;
        let poll = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(poll);
        let beat = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(beat);
        self.restart_telemetry();
        loop {
            // The telemetry poll's and the heartbeat's deadlines, if one is due at all.
            let poll_at = self.telemetry.poll_at;
            if let Some(at) = poll_at.filter(|&at| poll.deadline() != at) {
                poll.as_mut().reset(at);
            }
            let beat_at = self.beat_at;
            if let Some(at) = beat_at.filter(|&at| beat.deadline() != at) {
                beat.as_mut().reset(at);
            }
            // Biased: messages already queued go before the timers. A steady stream cannot
            // starve publication, the poll or the beat, since the due checks below run after
            // every step, whichever arm ran: the timer arms only wake an idle actor.
            tokio::select! {
                biased;
                msg = inbox.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                () = &mut tick, if armed => armed = false,
                () = &mut poll, if poll_at.is_some() => {}
                () = &mut beat, if beat_at.is_some() => {}
            }
            self.follow_busy();
            self.poll_if_due();
            self.beat_if_due();
            if self.dirty {
                let due = self
                    .last_published_at
                    .map_or_else(Instant::now, |at| at + PUBLISH_INTERVAL);
                if due <= Instant::now() {
                    self.publish();
                    armed = false;
                } else if !armed || tick.deadline() != due {
                    // Re-armed when a flush since arming moved the due time.
                    tick.as_mut().reset(due);
                    armed = true;
                }
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::GetState(reply) => {
                let _ = reply.send(self.last_published.clone());
            }
            Msg::GetLog(mode, reply) => {
                let _ = reply.send(self.state(mode).log.view());
            }
            Msg::Dispatch(intent, reply) => self.dispatch(intent, reply),
            Msg::Resolved {
                mode,
                request,
                directory,
                force,
                no_cache,
                reply,
            } => self.resolved(mode, request, directory, force, no_cache, reply),
            Msg::Inspected { id, result } => self.inspected(id, result),
            Msg::JobStarted(job) => {
                if let Some(start) = self
                    .start
                    .as_mut()
                    .filter(|start| start.step == Step::Launching)
                {
                    start.job_id = Some(job.job_id);
                }
                // The rows of the replaced snapshot's mode lose its overlay.
                self.touch_job();
                self.snapshot = Some(job.snapshot());
                self.job_started_at = Some(Instant::now());
                self.active = true;
                self.touch_job();
            }
            Msg::JobEvent { job_id, event } => {
                // A stale sink's event is not this snapshot's.
                if let Some(snapshot) = self
                    .snapshot
                    .as_mut()
                    .filter(|snapshot| snapshot.job_id == job_id)
                {
                    snapshot.apply_at(&event, Instant::now().into_std());
                    self.touch_job();
                }
            }
            Msg::Logs { mode, lines } => self.log(mode, &lines),
            Msg::Done(done) => {
                let outcome = Outcome::of_code(done.code);
                // A stale sink's `done` ends no later job.
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.job_id == done.job_id)
                {
                    self.active = false;
                    self.dirty = true;
                    if let Some(mode) = mode_of(done.mode) {
                        self.set_outcome(mode, Some(outcome));
                    }
                }
                // A job that ends before its launch returned ends its start too, which answers
                // the Run: the launch did start it.
                if self.start_job_is(done.job_id) {
                    self.finish_start(outcome, Ok(true));
                }
                // The view sees the state the job ended in before it hears the job ended.
                self.flush();
                self.publisher.done(&done);
            }
            Msg::SinkClosed { job_id } => {
                // A no-op after a normal `Done`, or once a later job holds the snapshot.
                let Some(snapshot) = self
                    .snapshot
                    .as_ref()
                    .filter(|snapshot| self.active && snapshot.job_id == job_id)
                else {
                    return;
                };
                let mode = mode_of(&snapshot.mode);
                self.active = false;
                self.dirty = true;
                if let Some(mode) = mode {
                    self.set_outcome(mode, Some(Outcome::Failed));
                }
                if self.start_job_is(job_id) {
                    self.finish_start(Outcome::Failed, Ok(true));
                }
            }
            Msg::Defaults(defaults) => {
                self.defaults = Some(defaults);
                self.dirty = true;
            }
            Msg::RequestResolved { attempt, result } => self.request_resolved(attempt, result),
            Msg::Launched { attempt, result } => self.launched(attempt, result),
            Msg::StopSettled {
                attempt,
                mode,
                result,
            } => self.stop_settled(attempt, mode, result),
            Msg::CacheCleared {
                mode,
                result,
                reply,
            } => {
                let line = match &result {
                    Ok(()) => LogLine::new(LogKind::Cache, CACHE_CLEARED),
                    Err(error) => error_line(error),
                };
                self.log(mode, &[line]);
                self.answer(reply, result.map(|()| true));
            }
            Msg::Sampled(result) => self.sampled(result),
        }
    }

    fn mode(&mut self, mode: PipelineMode) -> &mut ModeState {
        match mode {
            PipelineMode::Remux => &mut self.remux,
            PipelineMode::Extract => &mut self.extract,
        }
    }

    fn state(&self, mode: PipelineMode) -> &ModeState {
        match mode {
            PipelineMode::Remux => &self.remux,
            PipelineMode::Extract => &self.extract,
        }
    }

    /// Appends `lines` to `mode`'s Run Log and publishes them at once, in inbox order.
    fn log(&mut self, mode: PipelineMode, lines: &[LogLine]) {
        let batch = self.mode(mode).log.push(lines);
        self.publisher.logs(&batch);
    }

    /// Marks `mode`'s view as changed, for the next publication.
    fn touch(&mut self, mode: PipelineMode) {
        self.mode(mode).changed = true;
        self.dirty = true;
    }

    /// Marks the Job View, and the view of the Job Snapshot's mode, whose rows show it, as changed.
    fn touch_job(&mut self) {
        self.dirty = true;
        for mode in [PipelineMode::Remux, PipelineMode::Extract] {
            if self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mode == mode_name(mode))
            {
                self.touch(mode);
            }
        }
    }

    /// Publishes a pending change now, so a view that reads the state after an answer sees it.
    fn flush(&mut self) {
        if self.dirty {
            self.publish();
        }
    }

    /// Answers `reply` after the publication of what led to it.
    fn answer(&mut self, reply: Reply, answer: Result<bool, String>) {
        self.flush();
        let _ = reply.send(answer);
    }

    fn dispatch(&mut self, intent: Intent, reply: Reply) {
        let (mode, change) = match intent {
            Intent::ScanDirectory {
                mode,
                path,
                force,
                no_cache,
            } => return self.scan(mode, path, force, no_cache, ScanReply::Intent(reply)),
            Intent::Run { mode } => return self.run_batch(mode, reply),
            Intent::Stop { mode } => return self.stop_batch(mode, reply),
            Intent::ClearCache { mode } => {
                // Like the button, not refused while a batch runs: the cache is the session's.
                let inspector = Arc::clone(&self.inspector);
                return self.off_actor(
                    async move { inspector.clear_cache().await },
                    move |result| Msg::CacheCleared {
                        mode,
                        result,
                        reply,
                    },
                );
            }
            Intent::Inspect { mode, id } => {
                let answer = Ok(self.inspect(mode, id));
                return self.answer(reply, answer);
            }
            Intent::ViewVisible { visible } => {
                let _ = reply.send(Ok(self.view_visible(visible)));
                return;
            }
            Intent::SetSelected { mode, id, selected } => (mode, QueueChange::Select(id, selected)),
            Intent::SetAllSelected { mode, selected } => (mode, QueueChange::SelectAll(selected)),
            Intent::Reorder {
                mode,
                id,
                direction,
            } => (mode, QueueChange::Reorder(id, direction)),
            Intent::AssignSubtitle {
                mode,
                id,
                path,
                language,
                custom_language,
                title,
                offset,
            } => {
                let fields = SubtitleFields {
                    language: &language,
                    custom_language: &custom_language,
                    title: &title,
                    offset: &offset,
                };
                let assignment = subtitle_assignment(&id, &path, fields);
                (mode, QueueChange::Assign(assignment))
            }
            Intent::ClearTrack { mode, id } => (mode, QueueChange::Clear(id)),
            // A draft Intent answers without a flush (ADR-0016, S3): a burst of keystrokes is
            // coalesced into the run loop's publications instead of forcing one each.
            Intent::EditDraft { mode, changes } => {
                let _ = reply.send(self.change_draft(mode, |draft| draft.edit(changes)));
                return;
            }
            Intent::StepDraft {
                mode,
                field,
                direction,
            } => {
                let defaults = self.defaults.clone();
                let step =
                    |draft: &mut SettingsDraft| draft.step(field, direction, defaults.as_ref());
                let _ = reply.send(self.change_draft(mode, step));
                return;
            }
            Intent::SetPreset { mode, preset } => {
                let _ = reply.send(self.change_draft(mode, |draft| draft.set_preset(&preset)));
                return;
            }
        };
        let answer = self.change_queue(mode, change);
        self.answer(reply, answer);
    }

    /// Whether a start or a job holds the batch, which locks both queues and both drafts.
    fn busy(&self) -> bool {
        self.coordinator.phase() != Phase::Idle || self.active
    }

    /// Applies a Queue Selection or Track Assignment change, which a start, a running job or the
    /// mode's own scan refuses: the start captured the selection, the job runs it, and the scan
    /// replaces it.
    fn change_queue(&mut self, mode: PipelineMode, change: QueueChange) -> Result<bool, String> {
        if self.busy() {
            return Err("A batch is running; the queue is locked until it ends.".into());
        }
        let state = self.mode(mode);
        if matches!(state.scan, ScanView::Scanning { .. }) {
            return Err("The folder is being scanned; the queue is locked until it ends.".into());
        }
        let queue = &mut state.queue;
        let changed = match change {
            QueueChange::Select(id, selected) => queue.set_selected(&id, selected),
            QueueChange::SelectAll(selected) => queue.set_all_selected(selected),
            QueueChange::Reorder(id, direction) => queue.reorder(&id, direction),
            QueueChange::Assign(_) | QueueChange::Clear(_) if mode == PipelineMode::Extract => {
                return Err("Track Assignments are for remux only.".into());
            }
            QueueChange::Assign(assignment) => queue.assign(assignment),
            QueueChange::Clear(id) => queue.clear_assignment(&id),
        };
        if changed {
            self.touch(mode);
        }
        Ok(changed)
    }

    /// Applies a Settings Draft change, which a start or a running job refuses: the form is
    /// locked while either holds the batch.
    fn change_draft(
        &mut self,
        mode: PipelineMode,
        change: impl FnOnce(&mut SettingsDraft) -> Result<bool, String>,
    ) -> Result<bool, String> {
        if self.busy() {
            return Err(DRAFT_REFUSED.into());
        }
        let draft = &mut self.mode(mode).draft;
        let inputs = draft.meta_inputs(mode);
        let changed = change(draft)?;
        // Only the draft changed, which `publish` copies in, so a keystroke rebuilds no rows,
        // unless it changed the bitrate or the assets dir a remux row's meta line names.
        if self.mode(mode).draft.meta_inputs(mode) != inputs {
            self.touch(mode);
        }
        self.dirty |= changed;
        Ok(changed)
    }

    /// Picks row `id` of `mode` for its Episode Inspector, or with `None` none, so it shows the
    /// first row: true when that changed the pick. Not refused while a batch runs: it changes
    /// nothing a batch reads.
    fn inspect(&mut self, mode: PipelineMode, id: Option<String>) -> bool {
        let job = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.mode == mode_name(mode));
        let state = self.state(mode);
        if let Some(id) = &id {
            let in_job = job.is_some_and(|job| job.items.contains_key(id));
            if !state.queue.contains(id) && !in_job {
                return false;
            }
        }
        if state.inspected == id {
            return false;
        }
        self.mode(mode).inspected = id;
        self.dirty = true;
        true
    }

    /// Begins a scan: an empty path resets the queue at once, any other resolves off the actor. A
    /// running job refuses a scan of its own mode, whose queue it runs, also one begun before the
    /// job started and still resolving or inspecting; a start refuses every scan but its own.
    fn scan(
        &mut self,
        mode: PipelineMode,
        path: String,
        force: bool,
        no_cache: bool,
        reply: ScanReply,
    ) {
        if let Some(refusal) = self.scan_refusal(mode, &reply) {
            return self.reply_scan(reply, Err(refusal.into()));
        }
        let state = self.mode(mode);
        state.request += 1;
        // A scan still pending is overtaken, even one whose inspection settles before this
        // request resolves: its token no longer matches.
        state.token += 1;
        let request = state.request;
        let path = path.trim().to_string();
        if path.is_empty() {
            state.queue.reset();
            state.scan = ScanView::Idle;
            self.touch(mode);
            self.clear_snapshot(mode);
            return self.reply_scan(reply, Ok(false));
        }
        let inspector = Arc::clone(&self.inspector);
        let inbox = self.inbox.clone();
        self.runtime.spawn(async move {
            // A path with no identity is inspected as typed, and the inspection reports why.
            let directory = inspector.identity(path.clone()).await.unwrap_or(path);
            if let Some(inbox) = inbox.upgrade() {
                let _ = inbox.send(Msg::Resolved {
                    mode,
                    request,
                    directory,
                    force,
                    no_cache,
                    reply,
                });
            }
        });
    }

    /// Continues a scan whose path resolved: reuses the manifest held for `directory` unless
    /// `force`, else inspects it, sharing an inspection already in flight for the same key.
    fn resolved(
        &mut self,
        mode: PipelineMode,
        request: u64,
        directory: String,
        force: bool,
        no_cache: bool,
        reply: ScanReply,
    ) {
        if self.mode(mode).request != request {
            return self.reply_scan(reply, Ok(false));
        }
        // A start's scan whose start ended or is stopping inspects nothing: its Stop could only
        // cancel an inspection already registered.
        if let &ScanReply::Preflight(attempt) = &reply {
            let stopping = self.coordinator.phase() == Phase::Stopping;
            if stopping || self.current(attempt).is_none() {
                return self.reply_scan(reply, Ok(false));
            }
        }
        if let Some(refusal) = self.scan_refusal(mode, &reply) {
            return self.reply_scan(reply, Err(refusal.into()));
        }
        let state = self.mode(mode);
        state.token += 1;
        if !force && state.queue.reuse(&directory) {
            state.scan = ScanView::Idle;
            self.touch(mode);
            self.clear_snapshot(mode);
            return self.reply_scan(reply, Ok(true));
        }
        let token = state.token;
        state.scan = ScanView::Scanning {
            directory: directory.clone(),
        };
        self.touch(mode);
        let waiter = Waiter {
            mode,
            token,
            directory: directory.clone(),
            reply,
        };
        let key = (directory.clone(), no_cache);
        let Some(id) = self.in_flight.join(key, force, waiter) else {
            return;
        };
        let inspector = Arc::clone(&self.inspector);
        let inbox = self.inbox.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn(async move {
            // The inspection runs in a task of its own so a panic in it still settles its scans,
            // as a failure, instead of leaving the mode scanning and its key in flight.
            let result = runtime
                .spawn(async move { inspector.inspect(directory, no_cache).await })
                .await
                .unwrap_or_else(|e| Err(format!("Inspection failed: {e}")));
            if let Some(inbox) = inbox.upgrade() {
                let _ = inbox.send(Msg::Inspected { id, result });
            }
        });
    }

    /// Settles every scan waiting on inspection `id`: a scan still current accepts the manifest
    /// (under its `canonical_dir`, else the identity it asked for) or fails. One overtaken by a
    /// newer scan only answers; a start's scan whose start ended or is stopping goes back to Idle,
    /// keeps the queue and answers `Ok(false)`.
    fn inspected(&mut self, id: u64, result: Result<InspectionManifest, String>) {
        // One manifest, shared by every queue that accepts it.
        let result = result.map(Arc::new);
        let mut answers = Vec::new();
        for waiter in self.in_flight.settle(id) {
            // A start's own scan is never refused.
            let job_runs =
                matches!(waiter.reply, ScanReply::Intent(_)) && self.job_runs(waiter.mode);
            // A start's scan that settles after its start ended or while it stops is overtaken:
            // a Stop cancels its inspection, which must not fail the mode's scan.
            let overtaken = match waiter.reply {
                ScanReply::Preflight(attempt) => {
                    self.coordinator.phase() == Phase::Stopping || self.current(attempt).is_none()
                }
                ScanReply::Intent(_) => false,
            };
            let state = self.mode(waiter.mode);
            if state.token != waiter.token {
                answers.push((waiter.reply, Ok(false)));
                continue;
            }
            if overtaken {
                // The queue keeps its rows: no accept, no reset.
                state.scan = ScanView::Idle;
                self.touch(waiter.mode);
                answers.push((waiter.reply, Ok(false)));
                continue;
            }
            if job_runs {
                // A job started while this scan inspected: it keeps the rows it runs.
                state.scan = ScanView::Idle;
                self.touch(waiter.mode);
                answers.push((waiter.reply, Err(SCAN_REFUSED.into())));
                continue;
            }
            let accepted = result.as_ref().is_ok_and(|manifest| {
                let directory = manifest
                    .extra
                    .get("canonical_dir")
                    .and_then(|dir| dir.as_str())
                    .filter(|dir| !dir.is_empty())
                    .map_or(waiter.directory, str::to_string);
                state.queue.accept(Arc::clone(manifest), directory)
            });
            if accepted {
                state.scan = ScanView::Idle;
                self.clear_snapshot(waiter.mode);
            } else {
                state.queue.fail();
                state.scan = ScanView::Failed;
            }
            self.touch(waiter.mode);
            answers.push((waiter.reply, Ok(accepted)));
        }
        self.flush();
        for (reply, answer) in answers {
            self.reply_scan(reply, answer);
        }
    }

    /// Why a scan of `mode` that answers `reply` is refused, if it is.
    fn scan_refusal(&self, mode: PipelineMode, reply: &ScanReply) -> Option<&'static str> {
        if matches!(reply, ScanReply::Preflight(_)) {
            None
        } else if self
            .start
            .as_ref()
            .is_some_and(|start| start.step != Step::Launched)
        {
            Some(SCAN_STARTING)
        } else {
            self.job_runs(mode).then_some(SCAN_REFUSED)
        }
    }

    /// Answers a scan: its Intent after the publication of what led to it, or its start's
    /// preflight.
    fn reply_scan(&mut self, reply: ScanReply, answer: Result<bool, String>) {
        match reply {
            ScanReply::Intent(reply) => self.answer(reply, answer),
            ScanReply::Preflight(attempt) => self.preflight_scanned(attempt, answer),
        }
    }

    /// Whether a job is running in `mode`, whose queue it runs.
    fn job_runs(&self, mode: PipelineMode) -> bool {
        self.active
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mode == mode_name(mode))
    }

    /// An accepted scan, an Inspection Reuse or a reset of `mode` drops the Job Snapshot of
    /// that mode, but never while a job runs.
    fn clear_snapshot(&mut self, mode: PipelineMode) {
        if !self.active
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mode == mode_name(mode))
        {
            self.touch(mode);
            self.snapshot = None;
            self.job_started_at = None;
        }
    }

    /// The view of `mode` at `now`.
    fn mode_view(&mut self, mode: PipelineMode, now: StdInstant) -> ModeView {
        let job = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.mode == mode_name(mode))
            .zip(self.job_started_at.map(Instant::into_std));
        let state = match mode {
            PipelineMode::Remux => &mut self.remux,
            PipelineMode::Extract => &mut self.extract,
        };
        state.view(job, now)
    }

    fn publish(&mut self) {
        let job = self.snapshot.as_ref().map(|snapshot| JobView {
            job_id: snapshot.job_id,
            mode: snapshot.mode.clone(),
            status: snapshot.status,
            active: self.active,
            input_dir: snapshot.input_dir.clone(),
            effective_settings: snapshot.effective_settings.clone(),
        });
        // Each mode's view is rebuilt only when it changed, so a job's progress costs the rows of
        // its own mode, not those of the other. Its elapsed times are those of now.
        let now = Instant::now().into_std();
        if std::mem::take(&mut self.remux.changed) {
            self.last_published.remux = self.mode_view(PipelineMode::Remux, now);
        }
        if std::mem::take(&mut self.extract.changed) {
            self.last_published.extract = self.mode_view(PipelineMode::Extract, now);
        }
        self.last_published.seq += 1;
        self.last_published.job = job;
        self.last_published.run = RunView {
            phase: self.coordinator.phase(),
            mode: self.coordinator.owner(),
            settings: self.run_settings.clone(),
        };
        self.last_published.busy = self.busy();
        self.last_published.defaults.clone_from(&self.defaults);
        self.last_published
            .telemetry
            .clone_from(&self.telemetry.view);
        self.last_published
            .remux
            .draft
            .clone_from(&self.remux.draft);
        self.last_published
            .extract
            .draft
            .clone_from(&self.extract.draft);
        // Laid over on every publication, at the cost of one row each: they follow the draft, the
        // pick and who holds the batch, none of which rebuilds a mode.
        let hold = Hold {
            owner: self.coordinator.owner().or_else(|| {
                let snapshot = self.snapshot.as_ref().filter(|_| self.active)?;
                mode_of(&snapshot.mode)
            }),
            stopping: self.coordinator.phase() == Phase::Stopping,
            busy: self.busy(),
        };
        let batch = self.run_settings.as_ref().or_else(|| {
            let snapshot = self.snapshot.as_ref().filter(|_| self.active)?;
            Some(&snapshot.effective_settings)
        });
        self.last_published.lay_controls(&hold, batch);
        self.last_published.remux.inspector = self.inspector(PipelineMode::Remux);
        self.last_published.extract.inspector = self.inspector(PipelineMode::Extract);
        self.dirty = false;
        self.last_published_at = Some(Instant::now());
        self.publisher.state(&self.last_published);
    }

    /// The Episode Inspector of `mode` over its view as it is about to be published.
    fn inspector(&self, mode: PipelineMode) -> Option<InspectorView> {
        let state = self.state(mode);
        let view = match mode {
            PipelineMode::Remux => &self.last_published.remux,
            PipelineMode::Extract => &self.last_published.extract,
        };
        let (at, row) = inspected_row(&view.rows, state.inspected.as_deref())?;
        let job_error = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.mode == mode_name(mode))
            .and_then(|snapshot| snapshot.items.get(&row.id))
            .and_then(|item| item.error.as_deref());
        let entry = state.queue.entry(&row.id);
        Some(inspector_view(
            mode == PipelineMode::Remux,
            view,
            at,
            row,
            entry,
            job_error,
        ))
    }

    fn set_outcome(&mut self, mode: PipelineMode, outcome: Option<Outcome>) {
        let state = self.mode(mode);
        if state.outcome != outcome {
            state.outcome = outcome;
            self.touch(mode);
        }
    }
}

/// The mode a Job Snapshot or `job-done` names.
fn mode_of(name: &str) -> Option<PipelineMode> {
    [PipelineMode::Remux, PipelineMode::Extract]
        .into_iter()
        .find(|&mode| mode_name(mode) == name)
}

/// A change to a mode's queue, as an Intent asks it.
enum QueueChange {
    Select(String, bool),
    SelectAll(bool),
    Reorder(String, Direction),
    Assign(TrackAssignment),
    Clear(String),
}

#[cfg(test)]
mod tests;
