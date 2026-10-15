//! The Desktop Core actor (ADR-0016): one task that owns the published Job Snapshot and publishes
//! the whole View State through a `Publisher`, at most about 15 times a second. Everything reaches
//! it through one inbox, so a job's start, events, log batches and completion arrive in the order
//! the supervisor sent them. A UI adapter spawns it once with `Core::spawn` and talks to it
//! through the `CoreHandle`.

use crate::job::{JobDone, JobSink, JobStarted};
use crate::snapshot::{BatchStatus, JobSnapshot};
use mkv_core::log_line::LogLine;
use mkv_core::pipeline::JobEvent;
use mkv_core::request::EffectiveSettings;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// The shortest gap between two View State publications: at most about 15 a second.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(66);

/// Where the actor publishes: a UI adapter's seam (the Tauri one emits to the webview).
pub trait Publisher: Send + 'static {
    /// The whole View State, at most once per `PUBLISH_INTERVAL` except for the flush before a
    /// `done`.
    fn state(&self, state: &ViewState);
    /// A job's log batch, as soon as it arrives.
    fn logs(&self, lines: &[LogLine]);
    /// A job's end, after the publication of the state it ended in.
    fn done(&self, done: &JobDone);
    /// A job's event, as soon as it arrives. S2a only: keeps the webview's own reducer fed until
    /// S2b moves it onto the View State.
    fn job_event(&self, event: &JobEvent);
}

/// What the core publishes. `seq` grows by one with every publication, so a view can drop a
/// stale one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewState {
    pub seq: u64,
    pub job: Option<JobView>,
}

/// The published Job Snapshot, without its items.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobView {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    /// Whether the job still runs: from its start until its `job-done`.
    pub active: bool,
    pub start_time_ms: u64,
    pub input_dir: String,
    pub effective_settings: EffectiveSettings,
    /// The item IDs of the job's batch, in batch order.
    pub participants: Vec<String>,
}

enum Msg {
    GetState(oneshot::Sender<ViewState>),
    JobStarted(JobStarted),
    JobEvent {
        job_id: u64,
        event: JobEvent,
    },
    Logs(Vec<LogLine>),
    Done(JobDone),
    /// S2a only: the `get_job_snapshot` payload the webview's restore path still reads.
    GetJobSnapshot(oneshot::Sender<Option<JobSnapshot>>),
    /// S2a only: a Directory Inspection began, which drops the snapshot unless a job runs.
    ScanBegan,
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

    /// The published Job Snapshot. S2a only.
    pub async fn job_snapshot(&self) -> Result<Option<JobSnapshot>, String> {
        self.ask(Msg::GetJobSnapshot).await
    }

    /// Reports that a Directory Inspection began: the snapshot is dropped unless a job runs.
    /// S2a only.
    pub fn scan_began(&self) {
        self.send(Msg::ScanBegan);
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

/// The actor's state.
pub struct Core<P> {
    publisher: P,
    snapshot: Option<JobSnapshot>,
    /// Whether a job runs: from its `JobStarted` until its `Done`. A supervisor that dies before
    /// `finish` sends no `Done`, which leaves this set until the next job's `Done`.
    active: bool,
    /// Whether the state changed since the last publication.
    dirty: bool,
    last: ViewState,
    last_published_at: Option<Instant>,
}

impl<P: Publisher> Core<P> {
    /// Spawns the actor on `runtime`, publishing through `publisher`.
    pub fn spawn(runtime: Handle, publisher: P) -> CoreHandle {
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        let core = Core {
            publisher,
            snapshot: None,
            active: false,
            dirty: false,
            last: ViewState { seq: 0, job: None },
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
            tokio::select! {
                msg = inbox.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                // The check below publishes if still due: a `done` flush since arming moved it.
                () = &mut tick, if armed => armed = false,
            }
            if self.dirty && !armed {
                let due = self
                    .last_published_at
                    .map_or_else(Instant::now, |at| at + PUBLISH_INTERVAL);
                if due <= Instant::now() {
                    self.publish();
                } else {
                    tick.as_mut().reset(due);
                    armed = true;
                }
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::GetState(reply) => {
                let _ = reply.send(self.last.clone());
            }
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
                self.publisher.job_event(&event);
            }
            Msg::Logs(lines) => self.publisher.logs(&lines),
            Msg::Done(done) => {
                self.active = false;
                // The view sees the state the job ended in before it hears the job ended.
                self.publish();
                self.publisher.done(&done);
            }
            Msg::GetJobSnapshot(reply) => {
                let _ = reply.send(self.snapshot.clone());
            }
            Msg::ScanBegan => {
                if !self.active && self.snapshot.take().is_some() {
                    self.dirty = true;
                }
            }
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
        self.last = ViewState {
            seq: self.last.seq + 1,
            job,
        };
        self.dirty = false;
        self.last_published_at = Some(Instant::now());
        self.publisher.state(&self.last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::test_support::{
        batch_complete, batch_start, effective_settings, item_complete, item_progress, item_start,
    };
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    /// One thing the actor published, events and dones as JSON.
    #[derive(Debug, Clone)]
    enum Published {
        State(ViewState),
        Logs(Vec<LogLine>),
        Done(Value),
        Event(Value),
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
                    Published::State(state) => Some(state),
                    _ => None,
                })
                .collect()
        }
    }

    impl Publisher for RecordingPublisher {
        fn state(&self, state: &ViewState) {
            self.0.lock().unwrap().push(Published::State(state.clone()));
        }

        fn logs(&self, lines: &[LogLine]) {
            self.0.lock().unwrap().push(Published::Logs(lines.to_vec()));
        }

        fn done(&self, done: &JobDone) {
            let done = serde_json::to_value(done).unwrap();
            self.0.lock().unwrap().push(Published::Done(done));
        }

        fn job_event(&self, event: &JobEvent) {
            let event = serde_json::to_value(event).unwrap();
            self.0.lock().unwrap().push(Published::Event(event));
        }
    }

    fn spawn_core() -> (CoreHandle, RecordingPublisher) {
        let publisher = RecordingPublisher::default();
        (Core::spawn(Handle::current(), publisher.clone()), publisher)
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
        // Every event still reached the webview's reducer.
        assert_eq!(
            publisher
                .all()
                .iter()
                .filter(|published| matches!(published, Published::Event(_)))
                .count(),
            10_004
        );
        // The last publication holds the final snapshot, and nothing is left unpublished.
        let last = states.last().unwrap().clone();
        let job = last.job.clone().unwrap();
        assert_eq!(job.status, BatchStatus::Completed);
        assert_eq!(job.participants, ["ep1"]);
        assert_eq!(core.state().await.unwrap(), last);
        let snapshot = core.job_snapshot().await.unwrap().unwrap();
        assert_eq!(snapshot.items["ep1"].progress, Some(99.99));
        assert_eq!(snapshot.items["ep1"].duration_sec, Some(9.0));
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
                Published::Event(event) => Some(event["type"].as_str().unwrap().to_string()),
            })
            .collect();
        assert_eq!(
            order,
            [
                "batch_start",
                "log one",
                "item_start",
                "log two",
                "batch_complete",
                "done -1"
            ]
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
        core.job_sink().started(&started(2)); // t=30: due at t=86, not at the stale t=66

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
    async fn test_scan_began_is_refused_while_a_job_runs_and_clears_the_snapshot_otherwise() {
        let (core, _publisher) = spawn_core();
        let sink = core.job_sink();

        sink.started(&started(1));
        core.scan_began();
        assert!(core.job_snapshot().await.unwrap().is_some());

        sink.done(&done(1, 0));
        core.scan_began();
        assert!(core.job_snapshot().await.unwrap().is_none());
        // Past the trailing edge, not on it, so the tick cannot race the read.
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(core.state().await.unwrap().job, None);
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

        let snapshot = core.job_snapshot().await.unwrap().unwrap();
        assert_eq!(snapshot.job_id, 2);
        assert_eq!(snapshot.mode, "extract");
        assert_eq!(snapshot.input_dir, "/other");
        // The supervisor's start time, not the actor's.
        assert_eq!(snapshot.start_time_ms, 1_700_000_000_001);
        assert_eq!(snapshot.effective_settings, EffectiveSettings::defaults());
        assert!(snapshot.ordered_item_ids.is_empty());
    }

    #[test]
    fn test_view_state_wire_shape() {
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
        };
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
                }
            })
        );
        assert_eq!(
            serde_json::to_value(ViewState { seq: 0, job: None }).unwrap(),
            serde_json::json!({ "seq": 0, "job": null })
        );
    }
}
