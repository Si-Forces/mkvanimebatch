//! The Desktop Core actor (ADR-0016): one task that owns the queue and the Settings Draft of each
//! mode, the Job Coordinator, the published Job Snapshot and the scans in flight, and publishes the
//! whole View State through a `Publisher`, at most about 15 times a second. Everything reaches it through one
//! inbox, so a job's start, events, log batches and completion arrive in the order the supervisor
//! sent them, and an Intent sees every change sent before it. A UI adapter spawns it once with
//! `Core::spawn` and talks to it through the `CoreHandle`.

use crate::control::{mode_name, Launcher};
use crate::coordinator::{preflight, Captured, JobCoordinator, Phase};
use crate::draft::{DraftChanges, SettingsDraft};
use crate::inspection::{InFlight, Inspector};
use crate::job::{JobDone, JobSink, JobStarted};
use crate::queue::{Direction, Queue, Row};
use crate::snapshot::{BatchStatus, JobSnapshot};
use mkv_core::log_line::LogLine;
use mkv_core::manifest::InspectionManifest;
use mkv_core::pipeline::{JobEvent, PipelineMode};
use mkv_core::preset::{get_preset_profiles, PresetProfile};
use mkv_core::request::{BatchRequest, EffectiveSettings, TrackAssignment};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// The shortest gap between two View State publications: at most about 15 a second.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(66);

/// Where the actor publishes: a UI adapter's seam (the Tauri one emits to the webview).
pub trait Publisher: Send + 'static {
    /// The whole View State, at most once per `PUBLISH_INTERVAL` except for the flush before a
    /// `done` or the reply to an Intent that is not a draft Intent.
    fn state(&self, state: &ViewState);
    /// A job's log batch, as soon as it arrives.
    fn logs(&self, lines: &[LogLine]);
    /// A job's end, after the publication of the state it ended in.
    fn done(&self, done: &JobDone);
}

/// What a view asks the core to do. Each names the mode whose queue it acts on.
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
    /// Remux only.
    AssignTrack {
        mode: PipelineMode,
        assignment: TrackAssignment,
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
}

/// What the core publishes. `seq` grows by one with every publication, so a view can drop a
/// stale one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewState {
    pub seq: u64,
    pub job: Option<JobView>,
    pub run: RunView,
    /// The Effective Settings of a request that sets nothing, for the `auto (N)` hints: `None`
    /// until the actor has read the CPU topology once it started.
    pub defaults: Option<EffectiveSettings>,
    /// The built-in Preset Profiles.
    pub presets: Vec<PresetProfile>,
    pub remux: ModeView,
    pub extract: ModeView,
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
    pub start_time_ms: u64,
    pub input_dir: String,
    pub effective_settings: EffectiveSettings,
    /// The item IDs of the job's batch, in batch order.
    pub participants: Vec<String>,
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
    pub rows: Vec<Row>,
    pub vanished_selected_ids: Vec<String>,
    /// Track Assignments by item ID (remux only).
    pub assignments: BTreeMap<String, TrackAssignment>,
    pub draft: SettingsDraft,
    /// How the mode's last start or job ended: `None` before the first and while one runs.
    pub outcome: Option<Outcome>,
}

impl ViewState {
    /// What a core that holds nothing yet publishes: empty queues, each mode's draft as it
    /// starts, the catalog and `defaults`.
    pub fn new(defaults: Option<EffectiveSettings>) -> Self {
        Self {
            seq: 0,
            job: None,
            run: RunView::default(),
            defaults,
            presets: get_preset_profiles(),
            remux: ModeState::new(PipelineMode::Remux).view(None),
            extract: ModeState::new(PipelineMode::Extract).view(None),
        }
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
    Logs(Vec<LogLine>),
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
    /// The teardown a Stop of start `attempt` issued is over.
    StopSettled {
        attempt: u64,
        result: Result<(), String>,
    },
}

/// The way into the actor. Clones share its one inbox.
#[derive(Clone)]
pub struct CoreHandle(mpsc::UnboundedSender<Msg>);

impl CoreHandle {
    fn send(&self, msg: Msg) {
        // Only a stopped runtime drops the actor, and nothing is left to show then.
        let _ = self.0.send(msg);
    }

    async fn ask<T>(&self, msg: impl FnOnce(oneshot::Sender<T>) -> Msg) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        self.send(msg(reply));
        answer
            .await
            .map_err(|_| "The Desktop Core is not running.".to_string())
    }

    /// A `JobSink` for one job that feeds this actor.
    pub(crate) fn job_sink(&self) -> ActorSink {
        ActorSink {
            core: self.clone(),
            job_id: OnceLock::new(),
        }
    }

    /// The last published View State.
    pub async fn state(&self) -> Result<ViewState, String> {
        self.ask(Msg::GetState).await
    }

    /// Applies `intent`. Its effect is published before the answer, but for a draft Intent
    /// (`EditDraft`, `SetPreset`), which answers at once and is published with the next
    /// coalesced publication: `Ok(true)` when it applied, `Ok(false)` when it changed nothing
    /// (see `Intent::ScanDirectory`, `Intent::Run` and `Intent::Stop` for those), and an error
    /// when the core refuses it.
    pub async fn dispatch(&self, intent: Intent) -> Result<bool, String> {
        self.ask(|reply| Msg::Dispatch(intent, reply)).await?
    }
}

/// The `JobSink` of one job: turns what the supervisor reports into inbox messages, in its order.
pub struct ActorSink {
    core: CoreHandle,
    job_id: OnceLock<u64>,
}

impl JobSink for ActorSink {
    fn started(&self, job: &JobStarted) {
        let _ = self.job_id.set(job.job_id);
        self.core.send(Msg::JobStarted(job.clone()));
    }

    fn event(&self, event: &JobEvent) {
        // `JobState::start` reports the start before the supervisor sends any event.
        if let Some(&job_id) = self.job_id.get() {
            self.core.send(Msg::JobEvent {
                job_id,
                event: event.clone(),
            });
        }
    }

    fn logs(&self, lines: &[LogLine]) {
        self.core.send(Msg::Logs(lines.to_vec()));
    }

    fn done(&self, done: &JobDone) {
        self.core.send(Msg::Done(done.clone()));
    }
}

impl Drop for ActorSink {
    fn drop(&mut self) {
        // The supervisor owns the sink, so this reaches the actor even when it dies before `done`.
        if let Some(&job_id) = self.job_id.get() {
            self.core.send(Msg::SinkClosed { job_id });
        }
    }
}

/// One mode's queue, scan, Settings Draft and outcome.
struct ModeState {
    queue: Queue,
    draft: SettingsDraft,
    scan: ScanView,
    outcome: Option<Outcome>,
    /// The newest scan request: a request answered after a newer one began is overtaken.
    request: u64,
    /// The scan the queue waits for: an inspection that settles after a newer scan began, a reuse
    /// or a reset is overtaken.
    token: u64,
    /// Whether the queue, the scan or the Job Snapshot laid over the rows changed since the last
    /// publication. Only a changed mode's view is rebuilt; the other is published as last built.
    changed: bool,
}

impl ModeState {
    fn new(mode: PipelineMode) -> Self {
        Self {
            queue: Queue::default(),
            draft: SettingsDraft::new(mode),
            scan: ScanView::Idle,
            outcome: None,
            request: 0,
            token: 0,
            changed: false,
        }
    }

    /// The view of this mode, with `job`, the Job Snapshot of this mode if any, over its rows.
    fn view(&self, job: Option<&JobSnapshot>) -> ModeView {
        let queue = &self.queue;
        ModeView {
            directory: queue.directory().map(str::to_string),
            scan: self.scan.clone(),
            manifest_rev: queue.manifest_rev(),
            subtitle_candidates: queue.subtitle_candidates(),
            rows: queue.rows(job),
            vanished_selected_ids: queue.vanished_selected_ids().to_vec(),
            assignments: queue.assignments().clone(),
            draft: self.draft.clone(),
            outcome: self.outcome,
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

/// How far a start has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Resolving,
    Scanning,
    Launching,
    /// Its job runs.
    Launched,
}

/// A Run from its acceptance until its start or its job ends.
struct Start {
    attempt: u64,
    mode: PipelineMode,
    step: Step,
    request: BatchRequest,
    captured: Captured,
    /// From its `JobStarted` on.
    job_id: Option<u64>,
    /// Answered when the start settles.
    run_reply: Option<Reply>,
    /// Whether a teardown is in flight, whose `StopSettled` answers the Stops waiting on it.
    stop_issued: bool,
}

/// The actor's state.
pub struct Core<P, I, L> {
    publisher: P,
    inspector: Arc<I>,
    launcher: Arc<L>,
    runtime: Handle,
    /// Where the scans the actor spawns send their results; weak, so the loop still ends once
    /// every `CoreHandle` is gone.
    inbox: mpsc::WeakUnboundedSender<Msg>,
    remux: ModeState,
    extract: ModeState,
    in_flight: InFlight<Waiter>,
    snapshot: Option<JobSnapshot>,
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
    /// Whether the state changed since the last publication.
    dirty: bool,
    last_published: ViewState,
    last_published_at: Option<Instant>,
}

/// The answer to a scan of the running job's own mode.
const SCAN_REFUSED: &str = "A batch is running; its folder cannot be scanned until it ends.";
/// The answer to a scan of either mode while a start runs its forced scan.
const SCAN_STARTING: &str = "A batch is starting; no folder can be scanned until it has started.";
/// The answer to a Settings Draft edit while a job runs.
const DRAFT_REFUSED: &str = "A batch is running; the settings are locked until it ends.";
/// The answer to a Run while another start or job holds the batch.
const RUN_BUSY: &str = "A job is already running or stopping. Wait for it to finish.";
/// The answer to a Run while its mode scans.
const RUN_SCANNING: &str = "The folder is being scanned; a batch can start once the scan ends.";
/// The answer to a Run whose forced scan failed.
const INSPECTION_FAILED: &str = "Input inspection failed; job was not started.";

impl<P: Publisher, I: Inspector, L: Launcher> Core<P, I, L> {
    /// Spawns the actor on `runtime`, publishing through `publisher`, scanning through
    /// `inspector` and running jobs through `launcher`.
    pub fn spawn(runtime: Handle, publisher: P, inspector: I, launcher: L) -> CoreHandle {
        Self::start(runtime, publisher, inspector, launcher, None)
    }

    /// `spawn`, with the defaults already known when `defaults` holds them, else read on the
    /// blocking pool (`EffectiveSettings::defaults` reads the CPU topology) and published then.
    fn start(
        runtime: Handle,
        publisher: P,
        inspector: I,
        launcher: L,
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
            inspector: Arc::new(inspector),
            launcher: Arc::new(launcher),
            runtime: runtime.clone(),
            inbox: inbox_tx.downgrade(),
            remux: ModeState::new(PipelineMode::Remux),
            extract: ModeState::new(PipelineMode::Extract),
            in_flight: InFlight::default(),
            snapshot: None,
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
        loop {
            // Biased: messages already queued go before the tick. A steady stream cannot starve
            // publication, since the due check below runs after every step, whichever arm ran.
            tokio::select! {
                biased;
                msg = inbox.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                () = &mut tick, if armed => armed = false,
            }
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
                    snapshot.apply(&event);
                    self.touch_job();
                }
            }
            Msg::Logs(lines) => self.publisher.logs(&lines),
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
                let snapshot = self
                    .snapshot
                    .as_ref()
                    .filter(|snapshot| self.active && snapshot.job_id == job_id);
                if let Some(mode) = snapshot.map(|snapshot| mode_of(&snapshot.mode)) {
                    self.active = false;
                    self.dirty = true;
                    if let Some(mode) = mode {
                        self.set_outcome(mode, Some(Outcome::Failed));
                    }
                    if self.start_job_is(job_id) {
                        self.finish_start(Outcome::Failed, Ok(true));
                    }
                }
            }
            Msg::Defaults(defaults) => {
                self.defaults = Some(defaults);
                self.dirty = true;
            }
            Msg::RequestResolved { attempt, result } => self.request_resolved(attempt, result),
            Msg::Launched { attempt, result } => self.launched(attempt, result),
            Msg::StopSettled { attempt, result } => self.stop_settled(attempt, result),
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
            Intent::SetSelected { mode, id, selected } => (mode, QueueChange::Select(id, selected)),
            Intent::SetAllSelected { mode, selected } => (mode, QueueChange::SelectAll(selected)),
            Intent::Reorder {
                mode,
                id,
                direction,
            } => (mode, QueueChange::Reorder(id, direction)),
            Intent::AssignTrack { mode, assignment } => (mode, QueueChange::Assign(assignment)),
            Intent::ClearTrack { mode, id } => (mode, QueueChange::Clear(id)),
            // A draft Intent answers without a flush (ADR-0016, S3): a burst of keystrokes is
            // coalesced into the run loop's publications instead of forcing one each.
            Intent::EditDraft { mode, changes } => {
                let _ = reply.send(self.change_draft(mode, |draft| draft.edit(changes)));
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
        let changed = change(&mut self.mode(mode).draft)?;
        // Only the draft changed, which `publish` copies in, so a keystroke rebuilds no rows.
        self.dirty |= changed;
        Ok(changed)
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
    /// (under its `canonical_dir`, else the identity it asked for) or fails; an overtaken one only
    /// answers.
    fn inspected(&mut self, id: u64, result: Result<InspectionManifest, String>) {
        // One manifest, shared by every queue that accepts it.
        let result = result.map(Arc::new);
        let mut answers = Vec::new();
        for waiter in self.in_flight.settle(id) {
            // A start's own scan is never refused.
            let job_runs =
                matches!(waiter.reply, ScanReply::Intent(_)) && self.job_runs(waiter.mode);
            let state = self.mode(waiter.mode);
            if state.token != waiter.token {
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
        }
    }

    fn mode_view(&self, mode: PipelineMode) -> ModeView {
        let state = self.state(mode);
        let job = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.mode == mode_name(mode));
        state.view(job)
    }

    fn publish(&mut self) {
        let job = self.snapshot.as_ref().map(|snapshot| JobView {
            job_id: snapshot.job_id,
            mode: snapshot.mode.clone(),
            status: snapshot.status,
            active: self.active,
            start_time_ms: snapshot.start_time_ms,
            input_dir: snapshot.input_dir.clone(),
            effective_settings: snapshot.effective_settings.clone(),
            participants: snapshot.ordered_item_ids.clone(),
        });
        // Each mode's view is rebuilt only when it changed, so a job's progress costs the rows of
        // its own mode, not those of the other.
        if std::mem::take(&mut self.remux.changed) {
            self.last_published.remux = self.mode_view(PipelineMode::Remux);
        }
        if std::mem::take(&mut self.extract.changed) {
            self.last_published.extract = self.mode_view(PipelineMode::Extract);
        }
        self.last_published.seq += 1;
        self.last_published.job = job;
        self.last_published.run = RunView {
            phase: self.coordinator.phase(),
            mode: self.coordinator.owner(),
            settings: self.run_settings.clone(),
        };
        self.last_published.defaults.clone_from(&self.defaults);
        self.last_published
            .remux
            .draft
            .clone_from(&self.remux.draft);
        self.last_published
            .extract
            .draft
            .clone_from(&self.extract.draft);
        self.dirty = false;
        self.last_published_at = Some(Instant::now());
        self.publisher.state(&self.last_published);
    }

    fn set_outcome(&mut self, mode: PipelineMode, outcome: Option<Outcome>) {
        let state = self.mode(mode);
        if state.outcome != outcome {
            state.outcome = outcome;
            self.touch(mode);
        }
    }

    /// Runs `task` in a task of its own, so a panic still answers, as a failure, and sends its
    /// result to the inbox as `message`.
    fn off_actor<T: Send + 'static>(
        &self,
        task: impl Future<Output = Result<T, String>> + Send + 'static,
        message: impl FnOnce(Result<T, String>) -> Msg + Send + 'static,
    ) {
        let (inbox, runtime) = (self.inbox.clone(), self.runtime.clone());
        self.runtime.spawn(async move {
            let result = runtime
                .spawn(task)
                .await
                .unwrap_or_else(|e| Err(format!("Async runtime error: {e}")));
            if let Some(inbox) = inbox.upgrade() {
                let _ = inbox.send(message(result));
            }
        });
    }

    /// The start of `attempt`, unless a newer one replaced it or it ended.
    fn current(&mut self, attempt: u64) -> Option<&mut Start> {
        self.start.as_mut().filter(|start| start.attempt == attempt)
    }

    /// Whether the current start launched job `job_id`.
    fn start_job_is(&self, job_id: u64) -> bool {
        self.start
            .as_ref()
            .is_some_and(|start| start.job_id == Some(job_id))
    }

    /// Begins a Run of `mode`: builds its request from the draft, then captures the queue and
    /// resolves the request off the actor. A draft that builds no request flags its field and
    /// answers at once, with no start.
    fn run_batch(&mut self, mode: PipelineMode, reply: Reply) {
        if self.busy() {
            return self.answer(reply, Err(RUN_BUSY.into()));
        }
        let state = self.mode(mode);
        if matches!(state.scan, ScanView::Scanning { .. }) {
            return self.answer(reply, Err(RUN_SCANNING.into()));
        }
        // A Run clears the flag of the last one, and sets its own.
        let unflagged = state.draft.invalid.take().is_some();
        let built = state.draft.request(mode);
        let request = match built {
            Ok(request) => request,
            Err((field, error)) => {
                state.draft.invalid = Some(field);
                self.dirty = true;
                return self.answer(reply, Err(error));
            }
        };
        let captured = Captured::of(&state.queue);
        self.dirty |= unflagged;
        self.coordinator.begin(mode);
        self.set_outcome(mode, None);
        self.attempt += 1;
        let attempt = self.attempt;
        self.start = Some(Start {
            attempt,
            mode,
            step: Step::Resolving,
            request: request.clone(),
            captured,
            job_id: None,
            run_reply: Some(reply),
            stop_issued: false,
        });
        self.dirty = true;
        let launcher = Arc::clone(&self.launcher);
        self.off_actor(
            async move { launcher.resolve(request).await },
            move |result| Msg::RequestResolved { attempt, result },
        );
        self.flush();
    }

    /// A start's request resolved: its Effective Settings are published, and the forced scan of
    /// its input directory begins. A refused one ends the start.
    fn request_resolved(&mut self, attempt: u64, result: Result<EffectiveSettings, String>) {
        let stopping = self.coordinator.phase() == Phase::Stopping;
        let Some(start) = self.current(attempt) else {
            return;
        };
        if stopping {
            // Its `StopSettled` ends it.
            return;
        }
        match result {
            Err(error) => self.finish_start(Outcome::Failed, Err(error)),
            Ok(settings) => {
                start.step = Step::Scanning;
                let mode = start.mode;
                let path = start.request.input_dir.to_string_lossy().into_owned();
                let no_cache = start.request.no_cache == Some(true);
                self.run_settings = Some(settings);
                self.dirty = true;
                // Published at once: the view clears its log when they appear, and a quick
                // preflight would otherwise end the start within one publication interval.
                self.flush();
                self.scan(mode, path, true, no_cache, ScanReply::Preflight(attempt));
            }
        }
    }

    /// A start's forced scan settled: preflight checks the captured Queue Selection against it,
    /// and the job launches off the actor.
    fn preflight_scanned(&mut self, attempt: u64, answer: Result<bool, String>) {
        let stopping = self.coordinator.phase() == Phase::Stopping;
        let Some(start) = self.start.as_ref().filter(|start| start.attempt == attempt) else {
            return;
        };
        if stopping {
            return;
        }
        if answer != Ok(true) {
            return self.finish_start(Outcome::Failed, Err(INSPECTION_FAILED.into()));
        }
        let batch = preflight(start.mode, &start.captured, &self.state(start.mode).queue);
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => return self.finish_start(Outcome::Failed, Err(error)),
        };
        let Some(inbox) = self.inbox.upgrade() else {
            return;
        };
        let sink = CoreHandle(inbox).job_sink();
        let start = self.current(attempt).expect("the start was found above");
        start.request.selected_items = Some(batch.selected_items);
        start.request.assignments = batch.assignments;
        start.step = Step::Launching;
        let request = start.request.clone();
        let launcher = Arc::clone(&self.launcher);
        self.off_actor(
            async move { launcher.launch(request, sink).await },
            move |result| Msg::Launched { attempt, result },
        );
    }

    /// A start's launch returned: its job runs, unless a Stop came while it launched, which
    /// stops it now. The Run answers either way.
    fn launched(&mut self, attempt: u64, result: Result<u64, String>) {
        // A start whose job already ended is gone: its `Done` answered the Run.
        let Some(start) = self.current(attempt) else {
            return;
        };
        let job_id = match result {
            Ok(job_id) => job_id,
            Err(error) => return self.finish_start(Outcome::Failed, Err(error)),
        };
        start.job_id = Some(job_id);
        start.step = Step::Launched;
        let reply = start.run_reply.take();
        if !self.coordinator.launched() {
            // Stopping: the Stop waited for the job ID.
            self.issue_stop(attempt, Some(job_id));
        }
        self.dirty = true;
        if let Some(reply) = reply {
            self.answer(reply, Ok(true));
        }
    }

    /// Stops `mode`'s start or job. A start still resolving or scanning is cancelled by stopping
    /// the inspection; one launching is stopped once its job ID is known.
    fn stop_batch(&mut self, mode: PipelineMode, reply: Reply) {
        let owned = self
            .start
            .as_ref()
            .filter(|_| self.coordinator.owner() == Some(mode))
            .map(|start| (start.attempt, start.step, start.job_id));
        let Some((attempt, step, job_id)) = owned else {
            return self.answer(reply, Ok(false));
        };
        self.stop_waiters.push((attempt, reply));
        // A Stop while one is pending waits on the same teardown.
        if self.coordinator.request_stop() {
            self.dirty = true;
            match step {
                Step::Resolving | Step::Scanning => self.issue_stop(attempt, None),
                Step::Launching => {}
                Step::Launched => self.issue_stop(attempt, job_id),
            }
        }
        self.flush();
    }

    /// Tears down the start of `attempt` off the actor: job `job_id`, or none yet, and the
    /// Directory Inspection.
    fn issue_stop(&mut self, attempt: u64, job_id: Option<u64>) {
        if let Some(start) = self.current(attempt) {
            start.stop_issued = true;
        }
        let launcher = Arc::clone(&self.launcher);
        self.off_actor(async move { launcher.stop(job_id).await }, move |result| {
            Msg::StopSettled { attempt, result }
        });
    }

    /// A teardown is over. Before the launch it cancelled the start. After it the job's `Done`
    /// came first (the supervisor sends it before it releases the Job Slot, which the teardown
    /// waits on) and ended the start, unless the teardown failed, which leaves the job running.
    fn stop_settled(&mut self, attempt: u64, result: Result<(), String>) {
        let waiters = self.take_stop_waiters(attempt);
        let step = self.current(attempt).map(|start| start.step);
        match (step, &result) {
            (Some(Step::Resolving | Step::Scanning), _) => {
                self.finish_start(Outcome::Cancelled, Ok(false));
            }
            (Some(Step::Launched), Ok(())) => {
                // Released without a `Done`: its supervisor died, and its sink closes next.
                self.finish_start(Outcome::Failed, Ok(true));
            }
            (Some(Step::Launched), Err(_)) => {
                if let Some(start) = self.current(attempt) {
                    start.stop_issued = false;
                }
                self.coordinator.stop_failed();
                self.dirty = true;
            }
            _ => {}
        }
        self.flush();
        let answer = result.map(|()| true);
        for reply in waiters {
            let _ = reply.send(answer.clone());
        }
    }

    /// The Stops waiting on the start of `attempt`.
    fn take_stop_waiters(&mut self, attempt: u64) -> Vec<Reply> {
        let (stops, rest) = std::mem::take(&mut self.stop_waiters)
            .into_iter()
            .partition::<Vec<_>, _>(|(waiting, _)| *waiting == attempt);
        self.stop_waiters = rest;
        stops.into_iter().map(|(_, reply)| reply).collect()
    }

    /// Ends the current start, with `outcome` for its mode: the Run gets `run_answer` if it is
    /// still waiting, and so does every Stop no teardown answers.
    fn finish_start(&mut self, outcome: Outcome, run_answer: Result<bool, String>) {
        let Some(start) = self.start.take() else {
            return;
        };
        self.coordinator.finish();
        self.run_settings = None;
        self.dirty = true;
        self.set_outcome(start.mode, Some(outcome));
        let mut replies: Vec<(Reply, Result<bool, String>)> = Vec::new();
        if let Some(reply) = start.run_reply {
            replies.push((reply, run_answer));
        }
        if !start.stop_issued {
            let stops = self.take_stop_waiters(start.attempt);
            replies.extend(stops.into_iter().map(|reply| (reply, Ok(true))));
        }
        self.flush();
        for (reply, answer) in replies {
            let _ = reply.send(answer);
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
mod tests {
    use super::*;
    use crate::queue::test_support::manifest_of;
    use crate::snapshot::test_support::{
        batch_complete, batch_start, effective_settings, item_complete, item_progress, item_start,
    };
    use crate::snapshot::ItemStatus;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::Notify;

    /// One thing the actor published, dones as JSON.
    #[derive(Debug, Clone)]
    enum Published {
        State(Box<ViewState>),
        Logs(Vec<LogLine>),
        Done(Value),
    }

    #[derive(Clone, Default)]
    struct RecordingPublisher {
        published: Arc<Mutex<Vec<Published>>>,
        /// Notified on every state publication.
        stated: Arc<Notify>,
    }

    impl RecordingPublisher {
        fn all(&self) -> Vec<Published> {
            self.published.lock().unwrap().clone()
        }

        fn states(&self) -> Vec<ViewState> {
            self.all()
                .into_iter()
                .filter_map(|published| match published {
                    Published::State(state) => Some(*state),
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

        fn logs(&self, lines: &[LogLine]) {
            let lines = Published::Logs(lines.to_vec());
            self.published.lock().unwrap().push(lines);
        }

        fn done(&self, done: &JobDone) {
            let done = serde_json::to_value(done).unwrap();
            self.published.lock().unwrap().push(Published::Done(done));
        }
    }

    /// A core whose defaults are known from the start, so they publish nothing of their own.
    fn ready<P: Publisher, I: Inspector>(publisher: P, inspector: I) -> CoreHandle {
        ready_with(publisher, inspector, FakeLauncher::default())
    }

    fn ready_with<P: Publisher, I: Inspector, L: Launcher>(
        publisher: P,
        inspector: I,
        launcher: L,
    ) -> CoreHandle {
        Core::start(
            Handle::current(),
            publisher,
            inspector,
            launcher,
            Some(effective_settings()),
        )
    }

    fn spawn_core() -> (CoreHandle, RecordingPublisher) {
        let publisher = RecordingPublisher::default();
        let core = ready(publisher.clone(), FakeInspector::default());
        (core, publisher)
    }

    fn started(job_id: u64) -> JobStarted {
        JobStarted {
            job_id,
            mode: "remux",
            input_dir: "/anime".into(),
            effective_settings: effective_settings(),
            start_time_ms: 1_700_000_000_000,
        }
    }

    fn done(job_id: u64, code: i32) -> JobDone {
        JobDone {
            mode: "remux",
            job_id,
            code,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_burst_of_events_publishes_at_most_once_per_interval_and_ends_on_the_final_state(
    ) {
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
        assert_eq!(job.participants, ["ep1"]);
        assert_eq!(core.state().await.unwrap(), last);
        let row = &last.remux.rows[0];
        assert_eq!(row.progress, Some(99.99));
        assert_eq!(row.duration_sec, Some(9.0));
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
        assert_eq!(state.job.unwrap().participants, ["ep1", "ep2"]);
        assert_eq!(publisher.states().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_events_logs_and_done_keep_the_supervisors_order_and_the_last_state_precedes_done()
    {
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
                Published::Logs(lines) => Some(format!("log {}", lines[0].text)),
                Published::Done(done) => Some(format!("done {}", done["code"])),
            })
            .collect();
        assert_eq!(order, ["log one", "log two", "done -1"]);
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
        assert!(state.job.as_ref().unwrap().participants.is_empty());

        // Past the trailing edge, not on it, so the tick cannot race the read.
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        let state = core.state().await.unwrap();
        assert_eq!(&state, publisher.states().last().unwrap());
        assert_eq!(state.job.unwrap().participants, ["ep1"]);
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

        let job = core.state().await.unwrap().job.unwrap();
        assert_eq!(job.job_id, 2);
        assert_eq!(job.mode, "extract");
        assert_eq!(job.input_dir, "/other");
        // The supervisor's start time, not the actor's.
        assert_eq!(job.start_time_ms, 1_700_000_000_001);
        assert_eq!(job.effective_settings, EffectiveSettings::defaults());
        assert!(job.participants.is_empty());
    }

    #[test]
    fn test_view_state_wire_shape() {
        let mut remux = ModeView {
            directory: Some("/anime".into()),
            scan: ScanView::Scanning {
                directory: "/anime".into(),
            },
            manifest_rev: 2,
            vanished_selected_ids: vec!["ep9".into()],
            ..empty(REMUX)
        };
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
        let state = ViewState {
            seq: 3,
            run: RunView {
                phase: Phase::Running,
                mode: Some(REMUX),
                settings: Some(effective_settings()),
            },
            job: Some(JobView {
                job_id: 7,
                mode: "remux".into(),
                status: BatchStatus::Running,
                active: true,
                start_time_ms: 1_700_000_000_000,
                input_dir: "/anime".into(),
                effective_settings: effective_settings(),
                participants: vec!["ep2".into(), "ep1".into()],
            }),
            defaults: Some(EffectiveSettings::defaults()),
            presets: get_preset_profiles()[..1].to_vec(),
            remux,
            extract: ModeView {
                scan: ScanView::Failed,
                ..empty(EXTRACT)
            },
        };
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
                    "start_time_ms": 1_700_000_000_000_u64,
                    "input_dir": "/anime",
                    "effective_settings": {
                        "workers": 3,
                        "io_jobs": 5,
                        "ffmpeg_threads": 2,
                        "bitrate": "256k",
                        "complexity": 8
                    },
                    "participants": ["ep2", "ep1"]
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
                "defaults": serde_json::to_value(EffectiveSettings::defaults()).unwrap(),
                "presets": [
                    { "id": "balanced", "name": "Balanced", "bitrate": "192k", "complexity": 5 }
                ],
                "remux": {
                    "directory": "/anime",
                    "scan": { "state": "scanning", "directory": "/anime" },
                    "manifest_rev": 2,
                    "subtitle_candidates": { ".": ["ep1.ass"] },
                    "rows": [],
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
                    "outcome": "failed"
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
                    "outcome": null
                }
            })
        );
        let idle = serde_json::to_value(ViewState::new(None)).unwrap();
        assert_eq!(idle["job"], Value::Null);
        assert_eq!(idle["defaults"], Value::Null);
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
        assert!(serde_json::from_value::<Intent>(serde_json::json!({
            "type": "reorder", "mode": "remux", "id": "ep1", "direction": "left"
        }))
        .is_err());
    }

    /// A Track Assignment of `path` to `id`, with no language.
    fn subtitle(id: &str, path: &str, title: Option<&str>) -> TrackAssignment {
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
    struct FakeInspector {
        manifests: Arc<Mutex<HashMap<String, InspectionManifest>>>,
        inspected: Arc<Mutex<Vec<(String, bool)>>>,
    }

    const SLOW: Duration = Duration::from_millis(500);
    const INSPECTION: Duration = Duration::from_millis(200);

    impl FakeInspector {
        fn with(self, directory: &str, manifest: InspectionManifest) -> Self {
            self.manifests
                .lock()
                .unwrap()
                .insert(directory.into(), manifest);
            self
        }

        fn inspected(&self) -> Vec<(String, bool)> {
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
    }

    fn scan(mode: PipelineMode, path: &str, force: bool) -> Intent {
        Intent::ScanDirectory {
            mode,
            path: path.into(),
            force,
            no_cache: false,
        }
    }

    const REMUX: PipelineMode = PipelineMode::Remux;
    const EXTRACT: PipelineMode = PipelineMode::Extract;

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
        ModeState::new(mode).view(None)
    }

    fn ids(view: &ModeView) -> Vec<&str> {
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

            fn logs(&self, _lines: &[LogLine]) {}

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

    fn edit(mode: PipelineMode, changes: DraftChanges) -> Intent {
        Intent::EditDraft { mode, changes }
    }

    fn set_preset(mode: PipelineMode, preset: &str) -> Intent {
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
        let core = Core::spawn(
            Handle::current(),
            publisher.clone(),
            FakeInspector::default(),
            FakeLauncher::default(),
        );
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
    struct LauncherScript {
        /// Each call, in order: `resolve`, `launch`, `stop None` or `stop Some(id)`.
        calls: Vec<String>,
        launched: Vec<BatchRequest>,
        resolve_error: Option<String>,
        launch_error: Option<String>,
        stop_error: Option<String>,
        /// Ends each job with this `job-done` code before its launch returns.
        end_at_launch: Option<i32>,
        /// The jobs still running: ID, mode and sink.
        jobs: Vec<(u64, &'static str, ActorSink)>,
        next_id: u64,
    }

    /// A `Launcher` that answers from a script, after a delay on the paused clock. A launched job
    /// runs until the test ends it or a Stop does, which ends it cancelled before the Stop
    /// settles, as the supervisor's `done` precedes the slot release.
    #[derive(Clone, Default)]
    struct FakeLauncher(Arc<Mutex<LauncherScript>>);

    const RESOLVE: Duration = Duration::from_millis(100);
    const LAUNCH: Duration = Duration::from_millis(100);
    const STOP: Duration = Duration::from_millis(50);

    impl FakeLauncher {
        fn script(&self) -> std::sync::MutexGuard<'_, LauncherScript> {
            self.0.lock().unwrap()
        }

        fn calls(&self) -> Vec<String> {
            self.script().calls.clone()
        }

        fn launched(&self) -> Vec<BatchRequest> {
            self.script().launched.clone()
        }

        /// Ends the running job with `code`, as its supervisor would.
        fn end_job(&self, code: i32) {
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

    /// `scanning_core` over `launcher`, with the remux queue on `/anime` and its draft too.
    async fn run_core(launcher: &FakeLauncher) -> (CoreHandle, RecordingPublisher, FakeInspector) {
        let inspector = FakeInspector::default()
            .with("/anime", manifest_of(&["ep1", "ep2"]))
            .with("/fast", manifest_of(&["fast"]));
        let publisher = RecordingPublisher::default();
        let core = ready_with(publisher.clone(), inspector.clone(), launcher.clone());
        assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
        type_input(&core, REMUX, "/anime").await;
        (core, publisher, inspector)
    }

    async fn type_input(core: &CoreHandle, mode: PipelineMode, path: &str) {
        let changes = DraftChanges {
            input_dir: Some(path.into()),
            ..DraftChanges::default()
        };
        core.dispatch(edit(mode, changes)).await.unwrap();
    }

    /// Dispatches `intent` in a task of its own, for a test to act while it is pending.
    fn pending(core: &CoreHandle, intent: Intent) -> tokio::task::JoinHandle<Result<bool, String>> {
        let core = core.clone();
        tokio::spawn(async move { core.dispatch(intent).await })
    }

    fn run(mode: PipelineMode) -> Intent {
        Intent::Run { mode }
    }

    fn stop(mode: PipelineMode) -> Intent {
        Intent::Stop { mode }
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_run_resolves_scans_and_launches_then_its_done_ends_it() {
        let launcher = FakeLauncher::default();
        let (core, publisher, inspector) = run_core(&launcher).await;
        let published = publisher.states().len();

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        // Resolve, then the forced scan, then the launch.
        assert_eq!(launcher.calls(), ["resolve", "launch"]);
        assert_eq!(
            inspector.inspected(),
            [("/anime".to_string(), false), ("/anime".to_string(), false)]
        );
        let request = &launcher.launched()[0];
        assert_eq!(request.input_dir, std::path::PathBuf::from("/anime"));
        assert_eq!(
            request.selected_items,
            Some(vec!["ep1".into(), "ep2".into()])
        );
        assert_eq!(request.assignments, None);
        let state = core.state().await.unwrap();
        assert_eq!(
            state.run,
            RunView {
                phase: Phase::Running,
                mode: Some(REMUX),
                settings: Some(effective_settings()),
            }
        );
        assert!(state.job.unwrap().active);
        // Each step was published: starting, its settings, the scan, then running.
        let runs: Vec<_> = publisher.states()[published..]
            .iter()
            .map(|state| (state.run.phase, state.run.settings.is_some()))
            .collect();
        assert_eq!(runs.first(), Some(&(Phase::Starting, false)));
        assert!(runs.contains(&(Phase::Starting, true)));
        assert_eq!(runs.last(), Some(&(Phase::Running, true)));

        launcher.end_job(0);
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Done));
        assert_eq!(state.extract.outcome, None);
        assert!(!state.job.unwrap().active);
        // The `job-done` is still published.
        assert!(publisher
            .all()
            .iter()
            .any(|published| matches!(published, Published::Done(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn test_the_outcome_follows_the_job_done_code() {
        for (code, outcome) in [
            (0, Outcome::Done),
            (-1, Outcome::Cancelled),
            (1, Outcome::Failed),
        ] {
            let launcher = FakeLauncher::default();
            let (core, _publisher, _inspector) = run_core(&launcher).await;
            assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
            launcher.end_job(code);
            let state = core.state().await.unwrap();
            assert_eq!(state.remux.outcome, Some(outcome), "{code}");
            // The next Run clears it while it starts.
            let next = pending(&core, run(REMUX));
            tokio::time::sleep(RESOLVE / 2).await;
            assert_eq!(core.state().await.unwrap().remux.outcome, None);
            assert_eq!(next.await.unwrap(), Ok(true));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stale_done_does_not_end_the_running_job() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));

        // Another job's `job-done`: only the snapshot's own job ends its start.
        core.job_sink().done(&done(99, 0));
        let state = core.state().await.unwrap();
        assert_eq!(state.run.phase, Phase::Running);
        launcher.end_job(0);
        assert_eq!(core.state().await.unwrap().run.phase, Phase::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_job_that_ends_before_its_launch_returns_answers_the_run_and_ends_the_start() {
        let launcher = FakeLauncher::default();
        launcher.script().end_at_launch = Some(0);
        let (core, _publisher, _inspector) = run_core(&launcher).await;

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Done));
        // The late launch answer changes nothing: the next Run starts at once.
        tokio::time::sleep(LAUNCH).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
    }

    #[tokio::test(start_paused = true)]
    async fn test_stop_answers_after_the_jobs_done_so_the_state_already_shows_it_ended() {
        let launcher = FakeLauncher::default();
        let (core, publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));

        let stopping = pending(&core, stop(REMUX));
        tokio::time::sleep(STOP / 2).await;
        assert_eq!(core.state().await.unwrap().run.phase, Phase::Stopping);
        // A second Stop waits on the same teardown.
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(stopping.await.unwrap(), Ok(true));
        assert_eq!(launcher.calls(), ["resolve", "launch", "stop Some(1)"]);
        // Read straight after the answer: the job is over, not stopping.
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert!(!state.job.as_ref().unwrap().active);
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
        assert_eq!(&state, publisher.states().last().unwrap());
        // Nothing left to stop.
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(false));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_failed_stop_leaves_the_job_running_and_can_be_asked_again() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));

        launcher.script().stop_error = Some("Timed out waiting for job cleanup to finish.".into());
        assert_eq!(
            core.dispatch(stop(REMUX)).await,
            Err("Timed out waiting for job cleanup to finish.".into())
        );
        let state = core.state().await.unwrap();
        assert_eq!(state.run.phase, Phase::Running);
        assert_eq!(state.remux.outcome, None);

        launcher.script().stop_error = None;
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(core.state().await.unwrap().run.phase, Phase::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_job_that_ends_after_a_failed_stop_ends_with_its_own_outcome() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        launcher.script().stop_error = Some("Timed out waiting for job cleanup to finish.".into());
        assert!(core.dispatch(stop(REMUX)).await.is_err());

        launcher.end_job(0);
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Done));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_job_that_completes_while_a_stop_is_pending_ends_done_and_the_stop_succeeds() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));

        let stopping = pending(&core, stop(REMUX));
        tokio::time::sleep(STOP / 2).await;
        launcher.end_job(0);
        assert_eq!(stopping.await.unwrap(), Ok(true));
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Done));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_that_fails_after_the_jobs_done_answers_its_error_and_stays_idle() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        launcher.script().stop_error = Some("Timed out waiting for job cleanup to finish.".into());

        let stopping = pending(&core, stop(REMUX));
        tokio::time::sleep(STOP / 2).await;
        launcher.end_job(-1);
        assert_eq!(
            stopping.await.unwrap(),
            Err("Timed out waiting for job cleanup to finish.".into())
        );
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_while_the_forced_scan_resolves_its_path_inspects_nothing() {
        let launcher = FakeLauncher::default();
        let inspector = FakeInspector::default().with("/slow", manifest_of(&["slow"]));
        let core = ready_with(
            RecordingPublisher::default(),
            inspector.clone(),
            launcher.clone(),
        );
        assert_eq!(core.dispatch(scan(REMUX, "/slow", false)).await, Ok(true));
        type_input(&core, REMUX, "/slow").await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + SLOW / 2).await;

        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(running.await.unwrap(), Ok(false));
        tokio::time::sleep(SLOW + INSPECTION).await;
        assert_eq!(
            inspector.inspected().len(),
            1,
            "the forced scan inspected nothing"
        );
        let state = core.state().await.unwrap();
        assert_eq!(state.remux.scan, ScanView::Idle);
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_of_the_other_mode_or_with_nothing_running_stops_nothing() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(false));
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        assert_eq!(core.dispatch(stop(EXTRACT)).await, Ok(false));
        assert_eq!(launcher.calls(), ["resolve", "launch"]);
        assert_eq!(core.state().await.unwrap().run.phase, Phase::Running);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_while_resolving_cancels_the_start_without_a_scan_or_a_launch() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE / 2).await;

        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(running.await.unwrap(), Ok(false));
        tokio::time::sleep(RESOLVE).await;
        assert_eq!(launcher.calls(), ["resolve", "stop None"]);
        assert_eq!(inspector.inspected().len(), 1, "no forced scan");
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_while_scanning_cancels_the_start_and_its_scan_launches_nothing() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + INSPECTION / 2).await;
        assert!(matches!(
            core.state().await.unwrap().remux.scan,
            ScanView::Scanning { .. }
        ));

        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(running.await.unwrap(), Ok(false));
        // The forced scan still settles, after the start ended.
        tokio::time::sleep(INSPECTION).await;
        assert_eq!(inspector.inspected().len(), 2);
        assert_eq!(launcher.calls(), ["resolve", "stop None"]);
        let state = core.state().await.unwrap();
        assert_eq!(state.run.phase, Phase::Idle);
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
        assert_eq!(state.remux.scan, ScanView::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_while_launching_stops_the_job_once_it_launched() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + INSPECTION + LAUNCH / 2).await;
        assert_eq!(launcher.calls(), ["resolve", "launch"]);

        let stopping = pending(&core, stop(REMUX));
        // The launch did start the job.
        assert_eq!(running.await.unwrap(), Ok(true));
        assert_eq!(stopping.await.unwrap(), Ok(true));
        assert_eq!(launcher.calls(), ["resolve", "launch", "stop Some(1)"]);
        let state = core.state().await.unwrap();
        assert_eq!(state.run.phase, Phase::Idle);
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_while_launching_a_job_that_fails_to_launch_ends_failed() {
        let launcher = FakeLauncher::default();
        launcher.script().launch_error =
            Some("Application is closing; no new job can start.".into());
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + INSPECTION + LAUNCH / 2).await;

        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(
            running.await.unwrap(),
            Err("Application is closing; no new job can start.".into())
        );
        assert_eq!(launcher.calls(), ["resolve", "launch"]);
        assert_eq!(
            core.state().await.unwrap().remux.outcome,
            Some(Outcome::Failed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_refused_request_or_launch_fails_the_start_with_its_text() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;

        let refused = "Worker thread count must be between 1 and 64";
        launcher.script().resolve_error = Some(refused.into());
        assert_eq!(core.dispatch(run(REMUX)).await, Err(refused.into()));
        assert_eq!(inspector.inspected().len(), 1, "no forced scan");
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Failed));

        launcher.script().resolve_error = None;
        launcher.script().launch_error =
            Some("A job is already running or stopping. Wait for it to finish.".into());
        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("A job is already running or stopping. Wait for it to finish.".into())
        );
        let state = core.state().await.unwrap();
        assert_eq!(state.run.phase, Phase::Idle);
        assert_eq!(state.remux.outcome, Some(Outcome::Failed));
        // The queue is as it was, and unlocked.
        assert_eq!(ids(&state.remux), ["ep1", "ep2"]);
        let deselect = Intent::SetAllSelected {
            mode: REMUX,
            selected: false,
        };
        assert_eq!(core.dispatch(deselect).await, Ok(true));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_failed_forced_scan_fails_the_start() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        type_input(&core, REMUX, "/gone").await;
        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("Input inspection failed; job was not started.".into())
        );
        assert_eq!(launcher.calls(), ["resolve"]);
        assert_eq!(
            core.state().await.unwrap().remux.outcome,
            Some(Outcome::Failed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_run_with_nothing_selected_launches_nothing() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let deselect = Intent::SetAllSelected {
            mode: REMUX,
            selected: false,
        };
        core.dispatch(deselect).await.unwrap();
        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("No episodes selected in queue.".into())
        );
        assert_eq!(launcher.calls(), ["resolve"]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_selected_episode_that_vanished_or_reappeared_is_refused_by_name() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;
        // The forced scan no longer finds ep2.
        inspector
            .manifests
            .lock()
            .unwrap()
            .insert("/anime".into(), manifest_of(&["ep1"]));
        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("Selected episodes disappeared from the input directory: ep2. Change the queue selection to continue; job was not started.".into())
        );
        // The queue now remembers ep2 as vanished; a Run whose scan finds it again is refused too.
        assert_eq!(
            core.state().await.unwrap().remux.vanished_selected_ids,
            ["ep2"]
        );
        inspector
            .manifests
            .lock()
            .unwrap()
            .insert("/anime".into(), manifest_of(&["ep1", "ep2"]));
        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("Queue selection changed during inspection; previously missing selected episodes reappeared: ep2. Review the queue and press Run again; job was not started.".into())
        );
        assert!(launcher.launched().is_empty());
        // Its rows are back, so the next Run runs both.
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let selected = launcher.launched()[0].selected_items.clone();
        assert_eq!(selected, Some(vec!["ep1".into(), "ep2".into()]));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_run_of_the_queues_directory_sends_its_selection_and_assignments_even_by_alias()
    {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let reorder = Intent::Reorder {
            mode: REMUX,
            id: "ep2".into(),
            direction: Direction::Up,
        };
        core.dispatch(reorder).await.unwrap();
        for id in ["ep1", "ep2"] {
            let assignment = subtitle(id, &format!("/subs/{id}.ass"), None);
            let assign = Intent::AssignTrack {
                mode: REMUX,
                assignment,
            };
            core.dispatch(assign).await.unwrap();
        }
        let deselect = Intent::SetSelected {
            mode: REMUX,
            id: "ep1".into(),
            selected: false,
        };
        core.dispatch(deselect).await.unwrap();
        // Another spelling of the directory the queue shows.
        type_input(&core, REMUX, " /anime/. ").await;

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let request = &launcher.launched()[0];
        assert_eq!(request.input_dir, std::path::PathBuf::from("/anime/."));
        assert_eq!(request.selected_items, Some(vec!["ep2".into()]));
        let assignments = request.assignments.clone().unwrap();
        assert_eq!(assignments, [subtitle("ep2", "/subs/ep2.ass", None)]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_run_of_another_directory_runs_all_it_finds_without_the_old_assignments() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let assign = Intent::AssignTrack {
            mode: REMUX,
            assignment: subtitle("ep1", "/subs/ep1.ass", None),
        };
        core.dispatch(assign).await.unwrap();
        type_input(&core, REMUX, "/fast").await;

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let request = &launcher.launched()[0];
        assert_eq!(request.selected_items, Some(vec!["fast".into()]));
        assert_eq!(request.assignments, None);
        let state = core.state().await.unwrap();
        assert_eq!(state.remux.directory.as_deref(), Some("/fast"));
    }

    #[tokio::test(start_paused = true)]
    async fn test_an_extract_run_sends_no_preset_and_no_assignments() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        type_input(&core, EXTRACT, "/anime").await;
        assert_eq!(core.dispatch(run(EXTRACT)).await, Ok(true));
        let request = &launcher.launched()[0];
        assert_eq!(request.mode, EXTRACT);
        assert_eq!(
            (request.preset.as_ref(), request.assignments.as_ref()),
            (None, None)
        );
        assert_eq!(
            request.selected_items,
            Some(vec!["ep1".into(), "ep2".into()])
        );
        let state = core.state().await.unwrap();
        assert_eq!(state.run.mode, Some(EXTRACT));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_draft_that_builds_no_request_flags_its_field_and_starts_nothing() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let offset = |raw: &str| DraftChanges {
            sub_offset: Some(raw.into()),
            ..DraftChanges::default()
        };
        core.dispatch(edit(REMUX, offset("abc"))).await.unwrap();

        assert_eq!(
            core.dispatch(run(REMUX)).await,
            Err("Invalid sub-offset value 'abc': must be a number of seconds within range.".into())
        );
        // Published before the answer: the view flags the field from the state it reads.
        let state = core.state().await.unwrap();
        assert_eq!(state.remux.draft.invalid, Some("sub_offset"));
        assert_eq!((state.run.phase, state.remux.outcome), (Phase::Idle, None));
        assert!(launcher.calls().is_empty());

        // An edit of another field keeps the flag; the next Run clears it, and sets its own.
        type_input(&core, REMUX, "").await;
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(
            core.state().await.unwrap().remux.draft.invalid,
            Some("sub_offset")
        );
        let error = core.dispatch(run(REMUX)).await.unwrap_err();
        assert_eq!(error, "Input directory is required.");
        assert_eq!(
            core.state().await.unwrap().remux.draft.invalid,
            Some("input_dir")
        );
        type_input(&core, REMUX, "/anime").await;
        core.dispatch(edit(REMUX, offset("0"))).await.unwrap();
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        assert_eq!(core.state().await.unwrap().remux.draft.invalid, None);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_run_is_refused_while_the_batch_is_held_or_its_mode_scans() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        type_input(&core, EXTRACT, "/anime").await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE / 2).await;
        for mode in [REMUX, EXTRACT] {
            assert_eq!(
                core.dispatch(run(mode)).await,
                Err("A job is already running or stopping. Wait for it to finish.".into())
            );
        }
        assert_eq!(running.await.unwrap(), Ok(true));
        assert_eq!(
            core.dispatch(run(EXTRACT)).await,
            Err("A job is already running or stopping. Wait for it to finish.".into())
        );
        launcher.end_job(0);

        let scanning = pending(&core, scan(EXTRACT, "/fast", false));
        tokio::time::sleep(INSPECTION / 2).await;
        assert_eq!(
            core.dispatch(run(EXTRACT)).await,
            Err("The folder is being scanned; a batch can start once the scan ends.".into())
        );
        assert_eq!(scanning.await.unwrap(), Ok(true));
        assert_eq!(launcher.calls(), ["resolve", "launch"]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_start_refuses_every_scan_and_locks_both_queues_and_drafts() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        // While it resolves, then while its own forced scan inspects.
        for wait in [RESOLVE / 2, RESOLVE / 2 + INSPECTION / 2] {
            tokio::time::sleep(wait).await;
            assert_eq!(core.state().await.unwrap().run.phase, Phase::Starting);
            for mode in [REMUX, EXTRACT] {
                assert_eq!(
                    core.dispatch(scan(mode, "/fast", false)).await,
                    Err(SCAN_STARTING.into())
                );
                let select = Intent::SetAllSelected {
                    mode,
                    selected: false,
                };
                let error = core.dispatch(select).await.unwrap_err();
                assert!(error.contains("locked"), "{error}");
                let workers = DraftChanges {
                    workers: Some("2".into()),
                    ..DraftChanges::default()
                };
                assert_eq!(
                    core.dispatch(edit(mode, workers)).await,
                    Err(DRAFT_REFUSED.into())
                );
            }
            let error = core.dispatch(set_preset(REMUX, "fast")).await.unwrap_err();
            assert_eq!(error, DRAFT_REFUSED);
        }
        assert_eq!(running.await.unwrap(), Ok(true));
        // Once it runs, only the job's own mode refuses a scan.
        let error = core
            .dispatch(scan(REMUX, "/fast", false))
            .await
            .unwrap_err();
        assert_eq!(error, SCAN_REFUSED);
        assert_eq!(core.dispatch(scan(EXTRACT, "/fast", false)).await, Ok(true));
        assert_eq!(
            launcher.launched()[0]
                .selected_items
                .as_ref()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_job_whose_sink_closes_without_done_ends_failed() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        // A supervisor that died drops its sink without `done`.
        drop(launcher.script().jobs.remove(0));
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        let state = core.state().await.unwrap();
        assert_eq!(state.run, RunView::default());
        assert_eq!(state.remux.outcome, Some(Outcome::Failed));
        assert!(!state.job.unwrap().active);
    }

    /// A Run through the real engine: `EngineLauncher` resolves and launches a dry-run extract of
    /// a directory the real inspector scanned, and the job's `job-done` ends it.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_a_run_through_the_engine_launches_its_job_and_ends_with_it() {
        use crate::control::{EngineLauncher, InspectCacheState, ScanState};
        use crate::inspection::EngineInspector;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Show - 01.mkv"), b"video").unwrap();
        let (scan_state, cache) = (Arc::new(ScanState::default()), InspectCacheState::default());
        let publisher = RecordingPublisher::default();
        let core = ready_with(
            publisher.clone(),
            EngineInspector {
                scan: Arc::clone(&scan_state),
                cache: cache.clone(),
            },
            EngineLauncher {
                job: crate::job::test_support::job_state(),
                scan: scan_state,
                cache,
            },
        );
        let changes = DraftChanges {
            input_dir: Some(tmp.path().to_string_lossy().into_owned()),
            dry_run: Some(true),
            // Never the default cache file.
            no_cache: Some(true),
            ..DraftChanges::default()
        };
        core.dispatch(edit(EXTRACT, changes)).await.unwrap();

        assert_eq!(core.dispatch(run(EXTRACT)).await, Ok(true));
        let ended = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = core.state().await.unwrap();
                if state.run.phase == Phase::Idle {
                    return state;
                }
                publisher.stated.notified().await;
            }
        })
        .await
        .expect("the job never ended");
        assert!(ended.extract.outcome.is_some());
        let job = ended.job.unwrap();
        assert_eq!(job.mode, "extract");
        assert_eq!(
            std::fs::canonicalize(&job.input_dir).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }
}
