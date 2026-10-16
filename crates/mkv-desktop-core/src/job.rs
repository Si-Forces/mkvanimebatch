//! The active job of the Engine Bridge: the Job Slot, the supervisor that takes a job's pipeline
//! to one terminal, and the two-phase stop. The supervisor settles a job from its own private Job
//! Snapshot (the settlement ledger) and reports through a `JobSink`, so it runs, and is tested,
//! without a UI toolkit; its tasks run on the tokio runtime the adapter hands `JobState::new`.

use crate::snapshot::{attach_context, timestamp_ms, BatchStatus, JobSnapshot};
use mkv_core::log_line::LogLine;
use mkv_core::pipeline::{
    CancellationToken, EventReceiver, JobEvent, LogReceiver, PipelineSummary,
};
use mkv_core::request::EffectiveSettings;
use serde::Serialize;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::task::JoinError;

/// Log lines are forwarded to the sink in batches of at most this many lines...
const LOG_BATCH_MAX_LINES: usize = 25;
/// ...or whenever this interval elapses with lines pending.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_millis(25);

/// The end of a job: the `job-done` payload a UI adapter emits.
#[derive(Debug, Clone, Serialize)]
pub struct JobDone {
    pub mode: &'static str,
    pub job_id: u64,
    /// `0` completed, `-1` cancelled, `1` otherwise.
    pub code: i32,
}

/// The start of a job, as `JobState::start` reports it to the job's sink.
#[derive(Debug, Clone)]
pub struct JobStarted {
    pub job_id: u64,
    pub mode: &'static str,
    pub input_dir: String,
    pub effective_settings: EffectiveSettings,
    /// The job's wall-clock start, read once, so the supervisor's ledger and every sink's copy
    /// share it.
    pub start_time_ms: u64,
}

impl JobStarted {
    /// The Job Snapshot of this job as it starts.
    pub fn snapshot(&self) -> JobSnapshot {
        JobSnapshot::begin(
            self.job_id,
            self.mode,
            self.start_time_ms,
            self.input_dir.clone(),
            self.effective_settings.clone(),
        )
    }
}

/// Where a job's start, events, log batches and completion go: the Desktop Core actor's
/// `ActorSink` in the app; tests record them.
pub trait JobSink: Send + Sync + 'static {
    /// Called once, after the job has reserved the Job Slot and before its supervisor runs.
    fn started(&self, job: &JobStarted);
    fn event(&self, event: &JobEvent);
    fn logs(&self, lines: &[LogLine]);
    fn done(&self, done: &JobDone);
}

/// What a job runs with and is identified by when it starts.
pub struct JobConfig {
    pub mode: &'static str,
    pub input_dir: String,
    pub effective_settings: EffectiveSettings,
    pub cancel_token: CancellationToken,
}

/// The Job Slot's occupant: the one job the app runs at a time.
struct JobSlot {
    id: u64,
    cancel_token: CancellationToken,
    pipeline_abort: tokio::task::AbortHandle,
}

struct Inner {
    slot: Mutex<Option<JobSlot>>,
    next_id: AtomicU64,
    accepting_starts: AtomicBool,
    /// Where a job's pipeline and supervisor tasks run.
    runtime: Handle,
}

/// The active job of the app: its Job Slot and the gate that refuses starts
/// once shutdown begins. Clones share one state; a job's supervisor holds one for its whole life.
#[derive(Clone)]
pub struct JobState(Arc<Inner>);

impl JobState {
    /// A state with an empty Job Slot whose jobs run on `runtime`.
    pub fn new(runtime: Handle) -> Self {
        Self(Arc::new(Inner {
            slot: Mutex::new(None),
            next_id: AtomicU64::new(1),
            accepting_starts: AtomicBool::new(true),
            runtime,
        }))
    }

    /// Reserves the Job Slot, reports the start to `sink`, and runs `pipeline_future` under a
    /// supervisor that reports to `sink`. Returns the job's ID.
    pub fn start<S, F>(
        &self,
        sink: S,
        config: JobConfig,
        event_rx: EventReceiver,
        log_rx: LogReceiver,
        pipeline_future: F,
    ) -> Result<u64, String>
    where
        S: JobSink,
        F: Future<Output = Result<PipelineSummary, String>> + Send + 'static,
    {
        let JobConfig {
            mode,
            input_dir,
            effective_settings,
            cancel_token,
        } = config;
        let mut lock = self.reserve()?;
        let job_id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        // The supervisor's private settlement ledger: built by the same reducer as the snapshot
        // the UI sees, so teardown never depends on anyone else's copy.
        let started = JobStarted {
            job_id,
            mode,
            input_dir,
            effective_settings,
            start_time_ms: timestamp_ms(),
        };
        let mut ledger = started.snapshot();
        sink.started(&started);

        let pipeline_handle = self.0.runtime.spawn(pipeline_future);
        let pipeline_abort = pipeline_handle.abort_handle();
        let abort_on_supervisor_drop = pipeline_abort.clone();
        let supervisor_cancel = cancel_token.clone();
        let state = self.clone();
        self.0.runtime.spawn(async move {
            struct JobSlotGuard {
                state: JobState,
                job_id: u64,
            }
            impl Drop for JobSlotGuard {
                fn drop(&mut self) {
                    self.state.release_slot(self.job_id);
                }
            }
            struct PipelineAbortGuard(tokio::task::AbortHandle);
            impl Drop for PipelineAbortGuard {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }

            let _slot_guard = JobSlotGuard {
                state: state.clone(),
                job_id,
            };
            let _abort_guard = PipelineAbortGuard(abort_on_supervisor_drop);

            let (pipeline_result, completion, ()) = tokio::join!(
                pipeline_handle,
                forward_events(&sink, &mut ledger, mode, job_id, event_rx),
                forward_logs(&sink, log_rx, LOG_FLUSH_INTERVAL),
            );
            let terminal = pipeline_terminal(
                pipeline_result,
                completion,
                supervisor_cancel.is_cancelled(),
                &sink,
            );
            finish(&sink, &mut ledger, mode, job_id, terminal);
        });

        *lock = Some(JobSlot {
            id: job_id,
            cancel_token,
            pipeline_abort,
        });
        Ok(job_id)
    }

    /// Cancels the job in the slot and waits up to `timeout` for its supervisor to release it,
    /// then aborts the pipeline task and waits one more second. A job other than `expected_id`
    /// is left alone.
    pub(crate) fn stop(&self, expected_id: Option<u64>, timeout: Duration) -> Result<(), String> {
        let target = {
            let mut lock = self.0.slot.lock().map_err(|e| e.to_string())?;
            match &mut *lock {
                Some(slot) if expected_id.is_some_and(|id| id != slot.id) => return Ok(()),
                Some(slot) => {
                    slot.cancel_token.cancel();
                    Some(slot.id)
                }
                None => None,
            }
        };

        let Some(target_id) = target else {
            return Ok(());
        };

        // The supervisor must survive the worker abort to publish terminal events and release
        // the slot.
        if self.wait_until_released(target_id, timeout).is_err() {
            let lock = self.0.slot.lock().map_err(|error| error.to_string())?;
            if let Some(slot) = lock.as_ref().filter(|slot| slot.id == target_id) {
                slot.pipeline_abort.abort();
            }
            drop(lock);
            return self.wait_until_released(target_id, Duration::from_secs(1));
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.0.slot.lock().is_ok_and(|slot| slot.is_some())
    }

    /// Whether the slot holds job `job_id`.
    pub(crate) fn holds_slot(&self, job_id: u64) -> Result<bool, String> {
        let lock = self.0.slot.lock().map_err(|e| e.to_string())?;
        Ok(lock.as_ref().is_some_and(|slot| slot.id == job_id))
    }

    /// Refuses new jobs. A start already past this gate still lands in the slot, which
    /// `cancel_current` then cancels.
    pub(crate) fn close_starts(&self) {
        self.0.accepting_starts.store(false, Ordering::SeqCst);
    }

    pub(crate) fn open_starts(&self) {
        self.0.accepting_starts.store(true, Ordering::SeqCst);
    }

    pub(crate) fn ensure_accepting_starts(&self) -> Result<(), String> {
        if self.0.accepting_starts.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Application is closing; no new job can start.".into())
        }
    }

    /// Cancels the job in the slot, if any, without waiting for it.
    pub(crate) fn cancel_current(&self) {
        if let Ok(lock) = self.0.slot.lock() {
            if let Some(slot) = lock.as_ref() {
                slot.cancel_token.cancel();
            }
        }
    }

    pub(crate) fn wait_until_released(
        &self,
        target_id: u64,
        timeout: Duration,
    ) -> Result<(), String> {
        wait_until(timeout, "job", || {
            Ok(!self
                .0
                .slot
                .lock()
                .map_err(|e| e.to_string())?
                .as_ref()
                .is_some_and(|slot| slot.id == target_id))
        })
    }

    pub(crate) fn wait_until_empty(&self, timeout: Duration) -> Result<(), String> {
        wait_until(timeout, "job", || {
            Ok(self.0.slot.lock().map_err(|e| e.to_string())?.is_none())
        })
    }

    /// The slot, once this start may take it: shutdown has not begun and no job holds it. The
    /// caller keeps the lock until the job is in the slot, so a shutdown that begins meanwhile
    /// cancels it.
    fn reserve(&self) -> Result<MutexGuard<'_, Option<JobSlot>>, String> {
        let lock = self.0.slot.lock().map_err(|e| e.to_string())?;
        self.ensure_accepting_starts()?;
        if lock.is_some() {
            return Err("A job is already running or stopping. Wait for it to finish.".into());
        }
        Ok(lock)
    }

    fn release_slot(&self, job_id: u64) {
        if let Ok(mut lock) = self.0.slot.lock() {
            if lock.as_ref().is_some_and(|slot| slot.id == job_id) {
                *lock = None;
            }
        }
    }
}

/// Ends job `job_id` at `terminal`: settles the items the pipeline left open, applies and
/// sends the terminal, and reports `job-done` with the code the status of `ledger`, the
/// supervisor's own Job Snapshot, gives.
fn finish(
    sink: &impl JobSink,
    ledger: &mut JobSnapshot,
    mode: &'static str,
    job_id: u64,
    mut terminal: JobEvent,
) {
    attach_context(&mut terminal, mode, job_id);
    // Item Settlement: every item the pipeline left open ends before the job does, through
    // the same event the pipeline uses, so the ledger and the published snapshot agree.
    for mut event in ledger.settlement_events(&terminal) {
        attach_context(&mut event, mode, job_id);
        ledger.apply(&event);
        sink.event(&event);
    }
    ledger.apply(&terminal);
    let code = match ledger.status {
        BatchStatus::Completed => 0,
        BatchStatus::Cancelled => -1,
        _ => 1,
    };
    sink.event(&terminal);
    sink.done(&JobDone { mode, job_id, code });
}

/// The terminal a finished pipeline task amounts to: the engine's own `BatchComplete` when it
/// delivered one, else one the bridge synthesizes from how the task ended. `cancelled` is whether
/// the job was told to stop. A worker error or panic is also written to the job log.
fn pipeline_terminal(
    pipeline_result: Result<Result<PipelineSummary, String>, JoinError>,
    completion: Option<JobEvent>,
    cancelled: bool,
    sink: &impl JobSink,
) -> JobEvent {
    match pipeline_result {
        Ok(Ok(summary)) => completion.unwrap_or_else(|| {
            if summary.cancelled {
                bridge_terminal(true, None)
            } else {
                bridge_terminal(
                    false,
                    Some((
                        "error",
                        "Pipeline ended without a batch completion event".to_string(),
                    )),
                )
            }
        }),
        // Stop wins over an incidental worker error: the job was already
        // cancelled, so the terminal must stay cancelled. Either way the
        // error is still surfaced in the job log.
        Ok(Err(error)) => {
            sink.logs(&[LogLine::error(format!("ERROR: Pipeline failed: {error}"))]);
            if cancelled {
                bridge_terminal(true, None)
            } else {
                bridge_terminal(false, Some(("error", error)))
            }
        }
        Err(join_error) if join_error.is_cancelled() && cancelled => bridge_terminal(true, None),
        Err(join_error) if join_error.is_panic() => {
            let payload = join_error.into_panic();
            let message = payload
                .downcast_ref::<&str>()
                .map(|value| (*value).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "Unknown pipeline panic".to_string());
            sink.logs(&[LogLine::error(format!(
                "ERROR: Pipeline panicked: {message}"
            ))]);
            bridge_terminal(false, Some(("panic", message)))
        }
        Err(_) => bridge_terminal(
            false,
            Some(("error", "Pipeline task cancelled unexpectedly".to_string())),
        ),
    }
}

pub(crate) fn wait_until(
    timeout: Duration,
    operation: &str,
    released: impl Fn() -> Result<bool, String>,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if released()? {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "Timed out waiting for {operation} cleanup to finish."
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Forwards the engine's events to `sink`, applying each to `ledger` first. Returns the first
/// `BatchComplete`, held back for the supervisor to settle.
async fn forward_events(
    sink: &impl JobSink,
    ledger: &mut JobSnapshot,
    mode: &str,
    job_id: u64,
    mut event_rx: EventReceiver,
) -> Option<JobEvent> {
    let mut completion = None;
    while let Some(mut event) = event_rx.recv().await {
        if matches!(event, JobEvent::BatchComplete { .. }) {
            // One supervisor owns one job ID. Defer its first terminal until worker exit;
            // an abort or panic can supersede a late cooperative completion.
            if completion.is_none() {
                completion = Some(event);
            }
            continue;
        }
        if completion.is_none() {
            // The engine reports the raw canonical input path, which is
            // verbatim (`\\?\`) on Windows. Normalize it to the same display
            // form the Inspection Manifest uses so both sides of the Queue
            // Tracker's directory-identity comparison agree.
            if let JobEvent::BatchStart { input_dir, .. } = &mut event {
                *input_dir = display_canonical_path(input_dir);
            }
            attach_context(&mut event, mode, job_id);
            ledger.apply(&event);
            // The sink still gets every event; the ledger and the actor's snapshot drop non-v1
            // events.
            sink.event(&event);
        }
    }
    completion
}

/// Forwards the engine's log lines to `sink` in batches: when `LOG_BATCH_MAX_LINES` are pending,
/// whenever `flush_every` elapses with lines pending, and the rest when the channel closes.
async fn forward_logs(sink: &impl JobSink, mut log_rx: LogReceiver, flush_every: Duration) {
    let flush = |batch: &mut Vec<LogLine>| {
        if !batch.is_empty() {
            sink.logs(batch);
            batch.clear();
        }
    };
    let mut batch = Vec::with_capacity(LOG_BATCH_MAX_LINES);
    // `interval` would tick at once; the first flush is due one period from now.
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + flush_every, flush_every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            line = log_rx.recv() => match line {
                Some(line) => {
                    batch.push(line);
                    if batch.len() >= LOG_BATCH_MAX_LINES {
                        flush(&mut batch);
                    }
                }
                None => {
                    flush(&mut batch);
                    break;
                }
            },
            _ = interval.tick() => flush(&mut batch),
        }
    }
}

/// Strips the Windows verbatim prefix (`\\?\`, `\\?\UNC\`) that `canonicalize`
/// adds, so every path the UI compares uses the same display form.
pub fn display_canonical_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

/// A terminal the supervisor synthesizes when the worker did not deliver one.
fn bridge_terminal(cancelled: bool, problem: Option<(&str, String)>) -> JobEvent {
    let mut extra = serde_json::Map::new();
    if let Some((key, message)) = problem {
        extra.insert(key.to_string(), message.into());
    }
    JobEvent::BatchComplete {
        version: mkv_core::pipeline::EVENT_VERSION,
        success: false,
        cancelled,
        extra,
    }
}

/// Test helpers, also exported to the desktop adapter's tests through the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::*;
    use serde_json::Value;
    use std::sync::{Condvar, OnceLock};

    /// One multi-thread runtime for every test, standing in for the adapter's. A test may block
    /// its own thread (a stop, a `RecordingSink` wait) while the jobs it started run here.
    pub fn runtime() -> Handle {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap()
            })
            .handle()
            .clone()
    }

    /// A state whose jobs run on the shared test `runtime`.
    pub fn job_state() -> JobState {
        JobState::new(runtime())
    }

    pub fn poison<T: Send>(mutex: &Mutex<T>) {
        let _ = thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = mutex.lock().unwrap();
                    panic!("poison {}", std::any::type_name::<T>());
                })
                .join()
        });
    }

    impl JobState {
        /// Fills the Job Slot with job `id` that has no pipeline behind it, so nothing ever
        /// releases it.
        pub fn occupy(&self, id: u64) {
            *self.0.slot.lock().unwrap() = Some(JobSlot {
                id,
                cancel_token: CancellationToken::new(),
                pipeline_abort: self.0.runtime.spawn(async {}).abort_handle(),
            });
        }

        /// Empties the Job Slot as a supervisor does when its job ends.
        pub fn vacate(&self) {
            self.0.slot.lock().unwrap().take();
        }

        pub fn poison_slot(&self) {
            poison(&self.0.slot);
        }

        /// Whether the job in the slot has been told to stop.
        pub fn current_cancelled(&self) -> bool {
            self.0
                .slot
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|slot| slot.cancel_token.is_cancelled())
        }
    }

    /// Everything a `RecordingSink` was given, in order; events and dones as the webview's JSON.
    /// `snapshot` is the last job's Job Snapshot, begun on `started` and built from its events
    /// by `JobSnapshot::apply`, as the Desktop Core actor builds the published one.
    #[derive(Default)]
    pub struct Recorded {
        pub events: Vec<Value>,
        pub logs: Vec<Vec<LogLine>>,
        pub dones: Vec<Value>,
        pub snapshot: Option<JobSnapshot>,
        /// How many times `started` was called.
        pub started: usize,
    }

    /// A `JobSink` that records what it is given, for a test to read or wait on.
    #[derive(Clone, Default)]
    pub struct RecordingSink(Arc<(Mutex<Recorded>, Condvar)>);

    impl RecordingSink {
        fn record(&self, write: impl FnOnce(&mut Recorded)) {
            let (recorded, changed) = &*self.0;
            write(&mut recorded.lock().unwrap());
            changed.notify_all();
        }

        pub fn events(&self) -> Vec<Value> {
            self.0 .0.lock().unwrap().events.clone()
        }

        pub fn log_batches(&self) -> Vec<Vec<LogLine>> {
            self.0 .0.lock().unwrap().logs.clone()
        }

        pub fn dones(&self) -> Vec<Value> {
            self.0 .0.lock().unwrap().dones.clone()
        }

        pub fn started_count(&self) -> usize {
            self.0 .0.lock().unwrap().started
        }

        /// The Job Snapshot of the last job this sink saw start.
        pub fn snapshot(&self) -> JobSnapshot {
            let snapshot = self.0 .0.lock().unwrap().snapshot.clone();
            snapshot.expect("the sink saw no job start")
        }

        /// Blocks until `ready` holds for what was recorded, or `timeout` passes. Returns
        /// whether it held.
        pub fn wait_until(&self, timeout: Duration, ready: impl Fn(&Recorded) -> bool) -> bool {
            let (recorded, changed) = &*self.0;
            let (_recorded, result) = changed
                .wait_timeout_while(recorded.lock().unwrap(), timeout, |recorded| {
                    !ready(recorded)
                })
                .unwrap();
            !result.timed_out()
        }
    }

    impl JobSink for RecordingSink {
        fn started(&self, job: &JobStarted) {
            let snapshot = job.snapshot();
            self.record(|recorded| {
                recorded.started += 1;
                recorded.snapshot = Some(snapshot);
            });
        }

        fn event(&self, event: &JobEvent) {
            let json = serde_json::to_value(event).unwrap();
            self.record(|recorded| {
                recorded.events.push(json);
                if let Some(snapshot) = &mut recorded.snapshot {
                    snapshot.apply(event);
                }
            });
        }

        fn logs(&self, lines: &[LogLine]) {
            self.record(|recorded| recorded.logs.push(lines.to_vec()));
        }

        fn done(&self, done: &JobDone) {
            let done = serde_json::to_value(done).unwrap();
            self.record(|recorded| recorded.dones.push(done));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::snapshot::test_support::{
        batch_complete, batch_start, effective_settings, item_complete, item_progress, item_start,
    };
    use crate::snapshot::ItemStatus;
    use mkv_core::pipeline::CANCELLED_REASON;
    use serde_json::Value;

    /// A job state and a sink recording everything its jobs report.
    fn observed() -> (JobState, RecordingSink) {
        (job_state(), RecordingSink::default())
    }

    /// Starts a remux job running `worker`, or returns the error that refuses it.
    fn try_start_remux<F>(
        job: &JobState,
        sink: &RecordingSink,
        cancel_token: CancellationToken,
        event_rx: EventReceiver,
        log_rx: LogReceiver,
        worker: F,
    ) -> Result<u64, String>
    where
        F: Future<Output = Result<PipelineSummary, String>> + Send + 'static,
    {
        let config = JobConfig {
            mode: "remux",
            input_dir: "/anime".into(),
            effective_settings: effective_settings(),
            cancel_token,
        };
        job.start(sink.clone(), config, event_rx, log_rx, worker)
    }

    fn start_remux<F>(
        job: &JobState,
        sink: &RecordingSink,
        cancel_token: CancellationToken,
        event_rx: EventReceiver,
        log_rx: LogReceiver,
        worker: F,
    ) -> u64
    where
        F: Future<Output = Result<PipelineSummary, String>> + Send + 'static,
    {
        try_start_remux(job, sink, cancel_token, event_rx, log_rx, worker).unwrap()
    }

    /// A job that ends only when it is stopped, or the error that refuses it.
    fn start_pending(job: &JobState, sink: &RecordingSink) -> Result<u64, String> {
        let (_event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        try_start_remux(
            job,
            sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            std::future::pending::<Result<PipelineSummary, String>>(),
        )
    }

    /// Waits for the supervisor of `job_id` to release the slot. It releases it only after it has
    /// sent `job-done`, so a released slot must already have its done recorded.
    fn wait_released(job: &JobState, sink: &RecordingSink, job_id: u64) {
        job.wait_until_released(job_id, Duration::from_secs(2))
            .unwrap();
        assert!(
            sink.dones().iter().any(|done| done["job_id"] == job_id),
            "job {job_id} released its slot before it sent job-done"
        );
    }

    /// Asserts exactly one payload in `values` matches `job_id` (and `pred`) and returns it.
    fn only_one(values: &[Value], job_id: u64, pred: impl Fn(&Value) -> bool) -> Value {
        let matching: Vec<Value> = values
            .iter()
            .filter(|payload| payload["job_id"] == job_id && pred(payload))
            .cloned()
            .collect();
        assert_eq!(matching.len(), 1, "job {job_id}: {matching:?}");
        matching.into_iter().next().unwrap()
    }

    fn terminal_for(sink: &RecordingSink, job_id: u64) -> Value {
        only_one(&sink.events(), job_id, |event| {
            event["type"] == "batch_complete"
        })
    }

    fn done_for(sink: &RecordingSink, job_id: u64) -> Value {
        only_one(&sink.dones(), job_id, |_| true)
    }

    /// Every event payload of `job_id`, in the order the sink received them.
    fn emitted_for(sink: &RecordingSink, job_id: u64) -> Vec<Value> {
        sink.events()
            .into_iter()
            .filter(|payload| payload["job_id"] == job_id)
            .collect()
    }

    fn kinds(emitted: &[Value]) -> Vec<&str> {
        emitted
            .iter()
            .map(|payload| payload["type"].as_str().unwrap())
            .collect()
    }

    /// The `item_complete` payloads of `emitted`, in order.
    fn item_completes(emitted: &[Value]) -> Vec<&Value> {
        emitted
            .iter()
            .filter(|payload| payload["type"] == "item_complete")
            .collect()
    }

    /// Waits until the webview has been sent `target` as the progress of `id`. The sink
    /// applies each event to its snapshot as it records it, so the snapshot has it too.
    fn wait_for_progress(sink: &RecordingSink, id: &str, target: f64) {
        assert!(
            sink.wait_until(Duration::from_secs(2), |recorded| {
                recorded.events.iter().any(|event| {
                    event["type"] == "item_progress"
                        && event["id"] == id
                        && event["progress"] == target
                })
            }),
            "progress never reached {target}"
        );
    }

    fn note(text: impl Into<String>) -> LogLine {
        LogLine::note(text)
    }

    fn logged(sink: &RecordingSink, needle: &str) -> bool {
        sink.log_batches()
            .iter()
            .flatten()
            .any(|line| line.text.contains(needle))
    }

    async fn panicking_worker(
        event_tx: mkv_core::pipeline::EventSender,
    ) -> Result<PipelineSummary, String> {
        event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
        panic!("bridge worker panicked");
    }

    #[test]
    fn a_partial_batch_flushes_when_the_interval_elapses() {
        let sink = RecordingSink::default();
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(64);
        let forwarder = {
            let sink = sink.clone();
            runtime()
                .spawn(async move { forward_logs(&sink, log_rx, Duration::from_millis(20)).await })
        };

        log_tx.try_send(note("a")).unwrap();
        log_tx.try_send(note("b")).unwrap();
        // The channel is still open and the batch is not full, so only the interval can flush.
        assert!(
            sink.wait_until(Duration::from_secs(2), |recorded| recorded.logs.len() == 1),
            "a partial batch waited for a full one"
        );
        assert_eq!(sink.log_batches()[0], [note("a"), note("b")]);

        drop(log_tx);
        runtime().block_on(forwarder).unwrap();
        assert_eq!(sink.log_batches().len(), 1);
    }

    #[test]
    fn log_lines_flush_at_a_full_batch_and_the_rest_when_the_channel_closes() {
        let sink = RecordingSink::default();
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(64);
        // An hour apart, so only a full batch or the closed channel can flush.
        let forwarder = {
            let sink = sink.clone();
            runtime()
                .spawn(async move { forward_logs(&sink, log_rx, Duration::from_secs(3600)).await })
        };

        for line in 0..LOG_BATCH_MAX_LINES {
            log_tx.try_send(note(format!("line {line}"))).unwrap();
        }
        // The channel is still open, so only the line count can have flushed this.
        assert!(
            sink.wait_until(Duration::from_secs(2), |recorded| recorded.logs.len() == 1),
            "a full batch waited for the interval"
        );
        assert_eq!(sink.log_batches()[0].len(), LOG_BATCH_MAX_LINES);

        for line in ["tail 1", "tail 2", "tail 3"] {
            log_tx.try_send(note(line)).unwrap();
        }
        drop(log_tx);
        runtime().block_on(forwarder).unwrap();

        let batches = sink.log_batches();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0][0], note("line 0"));
        assert_eq!(batches[0][LOG_BATCH_MAX_LINES - 1], note("line 24"));
        assert_eq!(batches[1], [note("tail 1"), note("tail 2"), note("tail 3")]);
    }

    #[test]
    fn start_is_refused_while_starts_are_closed_or_a_job_holds_the_slot() {
        let (job, sink) = observed();

        job.close_starts();
        assert_eq!(
            start_pending(&job, &sink).unwrap_err(),
            "Application is closing; no new job can start."
        );
        // A refused start never reports a start to the sink.
        assert_eq!(sink.started_count(), 0);
        job.open_starts();
        let job_id = start_pending(&job, &sink).unwrap();
        assert_eq!(
            start_pending(&job, &sink).unwrap_err(),
            "A job is already running or stopping. Wait for it to finish."
        );
        assert_eq!(sink.started_count(), 1);
        assert_eq!(sink.snapshot().job_id, job_id);

        job.stop(Some(job_id), Duration::from_millis(20)).unwrap();
        wait_released(&job, &sink, job_id);
        assert_eq!(done_for(&sink, job_id)["code"], -1);
    }

    #[test]
    fn emitted_job_event_carries_top_level_mode_and_job_id() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            // An engine-supplied `extra` entry doubles as the flattening probe: if
            // `extra` stops being lifted, it nests and `mode` never reaches the top level.
            let mut extra = serde_json::Map::new();
            extra.insert("source".to_string(), "engine".into());
            event_tx
                .send(JobEvent::BatchStart {
                    version: 1,
                    total: 1,
                    input_dir: "/anime".into(),
                    items: None,
                    extra,
                })
                .await
                .unwrap();
            event_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_released(&job, &sink, job_id);

        let emitted = sink.events();
        let start = emitted
            .iter()
            .find(|event| event["type"] == "batch_start")
            .expect("a batch must emit BatchStart");
        assert_eq!(start["mode"], "remux");
        assert_eq!(start["job_id"], job_id);
        assert_eq!(start["source"], "engine");
        // A clean success: the ledger ended Completed.
        assert_eq!(done_for(&sink, job_id)["code"], 0);
    }

    #[test]
    fn forced_stop_aborts_only_worker_and_settles_once() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 70.0)).await.unwrap();
            let mut extra = serde_json::Map::new();
            extra.insert("source".to_string(), "cooperative".into());
            event_tx
                .send(JobEvent::BatchComplete {
                    version: 1,
                    success: false,
                    cancelled: true,
                    extra,
                })
                .await
                .unwrap();
            std::future::pending::<Result<PipelineSummary, String>>().await
        };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_for_progress(&sink, "ep2", 70.0);
        job.stop(Some(job_id), Duration::from_millis(20)).unwrap();

        assert!(!job.is_active());
        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(
            snapshot.ordered_item_ids,
            vec!["ep2".to_string(), "ep1".to_string()]
        );
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.completed_items(), 2);
        assert_eq!(snapshot.items["ep2"].progress, Some(70.0));
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Cancelled);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Cancelled);
        let terminal = terminal_for(&sink, job_id);
        assert_eq!(terminal["cancelled"], true);
        assert!(terminal["source"].is_null());
        assert_eq!(done_for(&sink, job_id)["code"], -1);

        // Item Settlement reached the webview before the terminal, in batch order, tagged like
        // the engine's events. ep2 had started, so it reports how long it ran; ep1 had not.
        let emitted = emitted_for(&sink, job_id);
        assert_eq!(
            kinds(&emitted),
            [
                "batch_start",
                "item_start",
                "item_progress",
                "item_complete",
                "item_complete",
                "batch_complete"
            ]
        );
        let settled = item_completes(&emitted);
        assert_eq!(settled[0]["id"], "ep2");
        assert_eq!(settled[1]["id"], "ep1");
        for item in &settled {
            assert_eq!(item["status"], "cancelled");
            assert_eq!(item["reason"], CANCELLED_REASON);
            assert_eq!(item["mode"], "remux");
        }
        assert!(settled[0]["duration_sec"].is_number());
        assert!(settled[1].get("duration_sec").is_none());
        assert!(snapshot.items["ep2"].duration_sec.is_some());
    }

    #[test]
    fn worker_error_before_batch_start_settles_no_items() {
        let (job, sink) = observed();
        // The sender is dropped unused: the run fails before it announces a batch.
        let (_, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move { Err::<PipelineSummary, String>("selected item vanished".into()) };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_released(&job, &sink, job_id);

        // Nothing is open, so the terminal is the only event.
        assert_eq!(kinds(&emitted_for(&sink, job_id)), ["batch_complete"]);
        assert_eq!(
            terminal_for(&sink, job_id)["error"],
            "selected item vanished"
        );
        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Error);
        assert!(snapshot.items.is_empty());
        assert_eq!(done_for(&sink, job_id)["code"], 1);
    }

    #[test]
    fn worker_error_after_stop_settles_as_cancelled() {
        let (job, sink) = observed();
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            log_tx.send(note("probing batch")).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 40.0)).await.unwrap();
            worker_token.cancelled().await;
            Err("scan failed after stop".to_string())
        };
        let job_id = start_remux(&job, &sink, token, event_rx, log_rx, worker);
        wait_for_progress(&sink, "ep2", 40.0);
        job.stop(Some(job_id), Duration::from_secs(1)).unwrap();

        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(snapshot.completed_items(), 2);
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Cancelled);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Cancelled);
        let terminal = terminal_for(&sink, job_id);
        assert_eq!(terminal["cancelled"], true);
        assert!(terminal["error"].is_null());
        assert_eq!(done_for(&sink, job_id)["code"], -1);
        assert!(
            logged(&sink, "scan failed after stop"),
            "worker error must stay in the job log"
        );
        // Cancelled, not failed: the stop wins over the incidental worker error.
        let emitted = emitted_for(&sink, job_id);
        let settled = item_completes(&emitted);
        assert_eq!(settled.len(), 2);
        for item in settled {
            assert_eq!(item["status"], "cancelled");
            assert!(item["error"].is_null());
        }
    }

    #[test]
    fn worker_error_without_stop_settles_as_failed_with_the_error() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(16);
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            log_tx.send(note("probing batch")).await.unwrap();
            Err("scan failed before any stop".to_string())
        };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_released(&job, &sink, job_id);

        let terminal = terminal_for(&sink, job_id);
        assert_eq!(terminal["cancelled"], false);
        assert_eq!(terminal["error"], "scan failed before any stop");
        assert_eq!(done_for(&sink, job_id)["code"], 1);
        assert!(
            sink.log_batches().concat().contains(&LogLine::error(
                "ERROR: Pipeline failed: scan failed before any stop"
            )),
            "worker error must reach the job log as an Error line"
        );
        // No item ever started: each ends as an error carrying the run's own message, in batch
        // order and before the terminal, with no duration.
        let emitted = emitted_for(&sink, job_id);
        assert_eq!(
            kinds(&emitted),
            [
                "batch_start",
                "item_complete",
                "item_complete",
                "batch_complete"
            ]
        );
        let settled = item_completes(&emitted);
        assert_eq!(settled[0]["id"], "ep2");
        assert_eq!(settled[1]["id"], "ep1");
        for item in settled {
            assert_eq!(item["status"], "error");
            assert_eq!(item["error"], "scan failed before any stop");
            assert!(item.get("duration_sec").is_none());
        }
    }

    #[test]
    fn success_that_leaves_items_open_settles_them_and_fails_the_job() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(16);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_complete("ep2", 3.0)).await.unwrap();
            event_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_released(&job, &sink, job_id);

        let emitted = emitted_for(&sink, job_id);
        assert_eq!(
            kinds(&emitted),
            [
                "batch_start",
                "item_start",
                "item_complete",
                "item_complete",
                "batch_complete"
            ]
        );
        assert_eq!(emitted[3]["id"], "ep1");
        assert_eq!(emitted[3]["status"], "error");
        assert_eq!(
            emitted[3]["error"],
            "Protocol error: missing completion event"
        );
        // The engine's own terminal is emitted unchanged, but the snapshot calls the job failed.
        assert_eq!(terminal_for(&sink, job_id)["success"], true);
        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Error);
        assert_eq!(done_for(&sink, job_id)["code"], 1);
    }

    #[test]
    fn pipeline_returning_without_a_completion_settles_open_items_as_failed() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(16);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep1"])).await.unwrap();
            event_tx.send(item_start("ep1")).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            worker,
        );
        wait_released(&job, &sink, job_id);

        let message = "Pipeline ended without a batch completion event";
        let emitted = emitted_for(&sink, job_id);
        assert_eq!(
            kinds(&emitted),
            [
                "batch_start",
                "item_start",
                "item_complete",
                "batch_complete"
            ]
        );
        assert_eq!(emitted[2]["status"], "error");
        assert_eq!(emitted[2]["error"], message);
        assert!(emitted[2]["duration_sec"].is_number());
        assert_eq!(terminal_for(&sink, job_id)["error"], message);
        assert_eq!(done_for(&sink, job_id)["code"], 1);
    }

    #[test]
    fn cooperative_stop_ignores_stale_id_and_duplicate_completion() {
        let (job, sink) = observed();
        let (previous_tx, previous_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, previous_logs) = mkv_core::pipeline::create_log_channel(8);
        let previous_worker = async move {
            previous_tx.send(batch_start(&[])).await.unwrap();
            previous_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let previous_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            previous_rx,
            previous_logs,
            previous_worker,
        );
        wait_released(&job, &sink, previous_id);
        assert_eq!(done_for(&sink, previous_id)["code"], 0);

        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 70.0)).await.unwrap();
            worker_token.cancelled().await;
            event_tx.send(batch_complete(false, true)).await.unwrap();
            event_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary {
                cancelled: true,
                ..Default::default()
            })
        };
        let job_id = start_remux(&job, &sink, token.clone(), event_rx, log_rx, worker);
        wait_for_progress(&sink, "ep2", 70.0);
        job.stop(Some(previous_id), Duration::from_millis(10))
            .unwrap();
        assert!(!token.is_cancelled());
        assert!(job.is_active());
        job.stop(Some(job_id), Duration::from_secs(1)).unwrap();

        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(snapshot.completed_items(), 2);
        assert_eq!(terminal_for(&sink, job_id)["cancelled"], true);
        done_for(&sink, job_id);
    }

    #[test]
    fn worker_panic_unwinds_into_one_error_terminal() {
        let (job, sink) = observed();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let job_id = start_remux(
            &job,
            &sink,
            CancellationToken::new(),
            event_rx,
            log_rx,
            panicking_worker(event_tx),
        );
        wait_released(&job, &sink, job_id);

        let snapshot = sink.snapshot();
        assert_eq!(snapshot.status, BatchStatus::Error);
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Error);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Error);
        assert_eq!(
            terminal_for(&sink, job_id)["panic"],
            "bridge worker panicked"
        );
        assert_eq!(done_for(&sink, job_id)["code"], 1);
        assert_eq!(
            sink.log_batches(),
            [[LogLine::error(
                "ERROR: Pipeline panicked: bridge worker panicked"
            )]]
        );

        // Each open item ended as an error carrying the panic text, in batch order, before the
        // terminal.
        let emitted = emitted_for(&sink, job_id);
        assert_eq!(
            kinds(&emitted),
            [
                "batch_start",
                "item_complete",
                "item_complete",
                "batch_complete"
            ]
        );
        let settled = item_completes(&emitted);
        assert_eq!(settled[0]["id"], "ep2");
        assert_eq!(settled[1]["id"], "ep1");
        for item in settled {
            assert_eq!(item["status"], "error");
            assert_eq!(item["error"], "bridge worker panicked");
        }
    }

    #[test]
    fn display_canonical_path_preserves_unc_and_drive_paths() {
        assert_eq!(
            display_canonical_path(r"\\?\C:\Anime\Show"),
            r"C:\Anime\Show"
        );
        assert_eq!(
            display_canonical_path(r"\\?\UNC\server\share\Show"),
            r"\\server\share\Show"
        );
        assert_eq!(display_canonical_path("/home/user/show"), "/home/user/show");
    }

    #[test]
    fn test_wait_for_job_slot_release_waits_for_cleanup() {
        let state = job_state();
        state.occupy(42);

        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                state.vacate();
            });
            assert!(state
                .wait_until_released(42, Duration::from_secs(1))
                .is_ok());
        });
    }

    #[test]
    fn test_wait_for_job_slot_release_times_out_while_owned() {
        let state = job_state();
        state.occupy(42);

        let result = state.wait_until_released(42, Duration::from_millis(10));
        assert!(result.unwrap_err().contains("Timed out"));
    }

    #[test]
    fn test_stop_reports_unreleased_slot() {
        let state = job_state();
        state.occupy(42);

        let result = state.stop(None, Duration::from_millis(50));
        assert!(result.unwrap_err().contains("Timed out"));
        assert!(state.is_active());
    }
}
