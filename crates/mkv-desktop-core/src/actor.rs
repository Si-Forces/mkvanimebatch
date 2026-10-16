//! The Desktop Core actor (ADR-0016): one task that owns the queue of each mode, the published
//! Job Snapshot and the scans in flight, and publishes the whole View State through a
//! `Publisher`, at most about 15 times a second. Everything reaches it through one inbox, so a
//! job's start, events, log batches and completion arrive in the order the supervisor sent them,
//! and an Intent sees every change sent before it. A UI adapter spawns it once with `Core::spawn`
//! and talks to it through the `CoreHandle`.

use crate::control::mode_name;
use crate::inspection::{InFlight, Inspector};
use crate::job::{JobDone, JobSink, JobStarted};
use crate::queue::{Direction, Queue, Row};
use crate::snapshot::{BatchStatus, JobSnapshot};
use mkv_core::log_line::LogLine;
use mkv_core::manifest::InspectionManifest;
use mkv_core::pipeline::{JobEvent, PipelineMode};
use mkv_core::request::{EffectiveSettings, TrackAssignment};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// `done` or an Intent's reply.
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
    /// it, it failed, or an empty `path` reset the queue.
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
}

/// What the core publishes. `seq` grows by one with every publication, so a view can drop a
/// stale one.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ViewState {
    pub seq: u64,
    pub job: Option<JobView>,
    pub remux: ModeView,
    pub extract: ModeView,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
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
        reply: Reply,
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
    pub fn job_sink(&self) -> ActorSink {
        ActorSink {
            core: self.clone(),
            job_id: OnceLock::new(),
        }
    }

    /// The last published View State.
    pub async fn state(&self) -> Result<ViewState, String> {
        self.ask(Msg::GetState).await
    }

    /// Applies `intent`. Its effect is published before the answer: `Ok(true)` when it applied,
    /// `Ok(false)` when it changed nothing (see `Intent::ScanDirectory` for a scan), and an error
    /// when the queue refuses it.
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

/// One mode's queue and scan.
#[derive(Default)]
struct ModeState {
    queue: Queue,
    scan: ScanView,
    /// The newest scan request: a request answered after a newer one began is overtaken.
    request: u64,
    /// The scan the queue waits for: an inspection that settles after a newer scan, a reuse or a
    /// reset is overtaken.
    token: u64,
}

/// A scan waiting on an in-flight inspection.
struct Waiter {
    mode: PipelineMode,
    token: u64,
    directory: String,
    reply: Reply,
}

/// The actor's state.
pub struct Core<P, I> {
    publisher: P,
    inspector: Arc<I>,
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
    /// Whether the state changed since the last publication.
    dirty: bool,
    last_published: ViewState,
    last_published_at: Option<Instant>,
}

impl<P: Publisher, I: Inspector> Core<P, I> {
    /// Spawns the actor on `runtime`, publishing through `publisher` and scanning through
    /// `inspector`.
    pub fn spawn(runtime: Handle, publisher: P, inspector: I) -> CoreHandle {
        // Unbounded on purpose (ADR-0016, 2026-10-16 amendment). The engine already drops
        // `ItemProgress` it cannot send (`try_send`), and the actor only applies events in memory
        // and publishes at most about 15 times a second, so the queue drains faster than a job
        // fills it. A bounded channel would need async `JobSink` methods and let a slow actor
        // stall the supervisor's teardown, which must never depend on the actor.
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        let core = Core {
            publisher,
            inspector: Arc::new(inspector),
            runtime: runtime.clone(),
            inbox: inbox_tx.downgrade(),
            remux: ModeState::default(),
            extract: ModeState::default(),
            in_flight: InFlight::default(),
            snapshot: None,
            active: false,
            dirty: false,
            last_published: ViewState::default(),
            last_published_at: None,
        };
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
                self.snapshot = Some(job.snapshot());
                self.active = true;
                self.dirty = true;
            }
            Msg::JobEvent { job_id, event } => {
                // A stale sink's event is not this snapshot's.
                if let Some(snapshot) = self
                    .snapshot
                    .as_mut()
                    .filter(|snapshot| snapshot.job_id == job_id)
                {
                    snapshot.apply(&event);
                    self.dirty = true;
                }
            }
            Msg::Logs(lines) => self.publisher.logs(&lines),
            Msg::Done(done) => {
                self.active = false;
                // The view sees the state the job ended in before it hears the job ended.
                self.publish();
                self.publisher.done(&done);
            }
            Msg::SinkClosed { job_id } => {
                // A no-op after a normal `Done`, or once a later job holds the snapshot.
                if self.active
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.job_id == job_id)
                {
                    self.active = false;
                    self.dirty = true;
                }
            }
        }
    }

    fn mode(&mut self, mode: PipelineMode) -> &mut ModeState {
        match mode {
            PipelineMode::Remux => &mut self.remux,
            PipelineMode::Extract => &mut self.extract,
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
            } => return self.scan(mode, path, force, no_cache, reply),
            Intent::SetSelected { mode, id, selected } => (mode, QueueChange::Select(id, selected)),
            Intent::SetAllSelected { mode, selected } => (mode, QueueChange::SelectAll(selected)),
            Intent::Reorder {
                mode,
                id,
                direction,
            } => (mode, QueueChange::Reorder(id, direction)),
            Intent::AssignTrack { mode, assignment } => (mode, QueueChange::Assign(assignment)),
            Intent::ClearTrack { mode, id } => (mode, QueueChange::Clear(id)),
        };
        let answer = self.change_queue(mode, change);
        self.answer(reply, answer);
    }

    /// Applies a Queue Selection or Track Assignment change, which a running job or the mode's
    /// own scan refuses: the job runs the selection it started with, and the scan replaces it.
    fn change_queue(&mut self, mode: PipelineMode, change: QueueChange) -> Result<bool, String> {
        if self.active {
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
            QueueChange::Assign(_) if mode == PipelineMode::Extract => {
                return Err("Track Assignments are for remux only.".into());
            }
            QueueChange::Assign(assignment) => queue.assign(assignment),
            QueueChange::Clear(id) => queue.clear_assignment(&id),
        };
        self.dirty |= changed;
        Ok(changed)
    }

    /// Begins a scan: an empty path resets the queue at once, any other resolves off the actor.
    fn scan(
        &mut self,
        mode: PipelineMode,
        path: String,
        force: bool,
        no_cache: bool,
        reply: Reply,
    ) {
        let state = self.mode(mode);
        state.request += 1;
        let request = state.request;
        let path = path.trim().to_string();
        if path.is_empty() {
            // A scan still pending is overtaken: its token no longer matches.
            state.token += 1;
            state.queue.reset();
            state.scan = ScanView::Idle;
            self.dirty = true;
            self.clear_snapshot(mode);
            return self.answer(reply, Ok(false));
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
        reply: Reply,
    ) {
        let state = self.mode(mode);
        if state.request != request {
            return self.answer(reply, Ok(false));
        }
        state.token += 1;
        if !force && state.queue.reuse(&directory) {
            state.scan = ScanView::Idle;
            self.dirty = true;
            self.clear_snapshot(mode);
            return self.answer(reply, Ok(true));
        }
        let token = state.token;
        state.scan = ScanView::Scanning {
            directory: directory.clone(),
        };
        self.dirty = true;
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
        let mut answers = Vec::new();
        for waiter in self.in_flight.settle(id) {
            let state = self.mode(waiter.mode);
            if state.token != waiter.token {
                answers.push((waiter.reply, Ok(false)));
                continue;
            }
            let accepted = result.as_ref().is_ok_and(|manifest| {
                let directory = manifest
                    .extra
                    .get("canonical_dir")
                    .and_then(|dir| dir.as_str())
                    .filter(|dir| !dir.is_empty())
                    .map_or(waiter.directory, str::to_string);
                state.queue.accept(manifest.clone(), directory)
            });
            if accepted {
                state.scan = ScanView::Idle;
                self.clear_snapshot(waiter.mode);
            } else {
                state.queue.fail();
                state.scan = ScanView::Failed;
            }
            self.dirty = true;
            answers.push((waiter.reply, Ok(accepted)));
        }
        self.flush();
        for (reply, answer) in answers {
            let _ = reply.send(answer);
        }
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
            self.snapshot = None;
            self.dirty = true;
        }
    }

    fn mode_view(&self, mode: PipelineMode) -> ModeView {
        let state = match mode {
            PipelineMode::Remux => &self.remux,
            PipelineMode::Extract => &self.extract,
        };
        let queue = &state.queue;
        let job = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.mode == mode_name(mode));
        ModeView {
            directory: queue.directory().map(str::to_string),
            scan: state.scan.clone(),
            manifest_rev: queue.manifest_rev(),
            subtitle_candidates: queue.subtitle_candidates(),
            rows: queue.rows(job),
            vanished_selected_ids: queue.vanished_selected_ids().to_vec(),
            assignments: queue.assignments().clone(),
        }
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
        self.last_published = ViewState {
            seq: self.last_published.seq + 1,
            job,
            remux: self.mode_view(PipelineMode::Remux),
            extract: self.mode_view(PipelineMode::Extract),
        };
        self.dirty = false;
        self.last_published_at = Some(Instant::now());
        self.publisher.state(&self.last_published);
    }
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

    /// One thing the actor published, dones as JSON.
    #[derive(Debug, Clone)]
    enum Published {
        State(Box<ViewState>),
        Logs(Vec<LogLine>),
        Done(Value),
    }

    #[derive(Clone, Default)]
    struct RecordingPublisher(Arc<Mutex<Vec<Published>>>);

    impl RecordingPublisher {
        fn all(&self) -> Vec<Published> {
            self.0.lock().unwrap().clone()
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
            self.0
                .lock()
                .unwrap()
                .push(Published::State(Box::new(state.clone())));
        }

        fn logs(&self, lines: &[LogLine]) {
            self.0.lock().unwrap().push(Published::Logs(lines.to_vec()));
        }

        fn done(&self, done: &JobDone) {
            let done = serde_json::to_value(done).unwrap();
            self.0.lock().unwrap().push(Published::Done(done));
        }
    }

    fn spawn_core() -> (CoreHandle, RecordingPublisher) {
        let publisher = RecordingPublisher::default();
        let core = Core::spawn(
            Handle::current(),
            publisher.clone(),
            FakeInspector::default(),
        );
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
            ..ModeView::default()
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
        let state = ViewState {
            seq: 3,
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
            remux,
            extract: ModeView {
                scan: ScanView::Failed,
                ..ModeView::default()
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
                    }
                },
                "extract": {
                    "directory": null,
                    "scan": { "state": "failed" },
                    "manifest_rev": 0,
                    "subtitle_candidates": {},
                    "rows": [],
                    "vanished_selected_ids": [],
                    "assignments": {}
                }
            })
        );
        let idle = serde_json::to_value(ViewState::default()).unwrap();
        assert_eq!(idle["job"], Value::Null);
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
        let core = Core::spawn(Handle::current(), publisher.clone(), inspector.clone());
        (core, publisher, inspector)
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
        assert_eq!(state.extract, ModeView::default());
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
        let core = Core::spawn(
            Handle::current(),
            RecordingPublisher::default(),
            inspector.clone(),
        );

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
                ..ModeView::default()
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
        let core = Core::spawn(Handle::current(), RecordingPublisher::default(), inspector);

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

        // Refused while the job runs: an accepted scan, a reuse and a reset of its mode.
        assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
        assert_eq!(core.dispatch(scan(REMUX, "/anime", false)).await, Ok(true));
        assert_eq!(core.dispatch(scan(REMUX, "", false)).await, Ok(false));
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
}
