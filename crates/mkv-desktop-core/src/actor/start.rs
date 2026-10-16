//! The start of a Run (ADR-0016): from its acceptance through resolving its request, the forced
//! scan and preflight, to the launch of its job, and the Stop that cancels a start or tears its job
//! down. The Desktop Core actor drives it from its inbox, and the Job Coordinator holds its phase.
//! What a view shows of it in the Run Log is written here too: a Run's error, the Stop note and a
//! failed teardown, and the clear of the log once a Run's request resolved.

use super::{Core, CoreHandle, Msg, Outcome, Publisher, Reply, ScanReply, ScanView};
use crate::control::Launcher;
use crate::coordinator::{preflight, Captured, Phase};
use crate::inspection::Inspector;
use crate::telemetry::Sampler;
use mkv_core::log_line::LogLine;
use mkv_core::pipeline::PipelineMode;
use mkv_core::request::{BatchRequest, EffectiveSettings};
use std::future::Future;
use std::sync::Arc;

/// How far a start has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Resolving,
    Scanning,
    Launching,
    /// Its job runs.
    Launched,
}

/// A Run from its acceptance until its start or its job ends.
pub(super) struct Start {
    attempt: u64,
    mode: PipelineMode,
    pub(super) step: Step,
    request: BatchRequest,
    captured: Captured,
    /// From its `JobStarted` on.
    pub(super) job_id: Option<u64>,
    /// Answered when the start settles.
    run_reply: Option<Reply>,
    /// Whether a teardown is in flight, whose `StopSettled` answers the Stops waiting on it.
    stop_issued: bool,
}

/// The answer to a Run while another start or job holds the batch.
const RUN_BUSY: &str = "A job is already running or stopping. Wait for it to finish.";
/// The answer to a Run while its mode scans.
const RUN_SCANNING: &str = "The folder is being scanned; a batch can start once the scan ends.";
/// The answer to a Run whose forced scan failed.
const INSPECTION_FAILED: &str = "Input inspection failed; job was not started.";
/// The Run Log line of a Stop of a launched job, whose teardown can take a while.
const STOP_REQUESTED: &str = "Stop requested; waiting for shutdown.";

impl<P: Publisher, I: Inspector, L: Launcher, S: Sampler> Core<P, I, L, S> {
    /// Runs `task` in a task of its own and sends its result to the inbox as `message`. The inner
    /// task is what turns a panic in `task` itself (a fake, or adapter code outside its
    /// `spawn_blocking`) into an `Err`, so the Run or Stop still answers, as a failure.
    pub(super) fn off_actor<T: Send + 'static>(
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
    pub(super) fn current(&mut self, attempt: u64) -> Option<&mut Start> {
        self.start.as_mut().filter(|start| start.attempt == attempt)
    }

    /// Whether the current start launched job `job_id`.
    pub(super) fn start_job_is(&self, job_id: u64) -> bool {
        self.start
            .as_ref()
            .is_some_and(|start| start.job_id == Some(job_id))
    }

    /// Begins a Run of `mode`: builds its request from the draft, then captures the queue and
    /// resolves the request off the actor. A draft that builds no request flags its field and
    /// answers at once, with no start.
    pub(super) fn run_batch(&mut self, mode: PipelineMode, reply: Reply) {
        if self.busy() {
            return self.answer_run(mode, reply, Err(RUN_BUSY.into()));
        }
        let state = self.mode(mode);
        if matches!(state.scan, ScanView::Scanning { .. }) {
            return self.answer_run(mode, reply, Err(RUN_SCANNING.into()));
        }
        // A Run clears the flag of the last one, and sets its own.
        let unflagged = state.draft.invalid.take().is_some();
        let built = state.draft.request(mode);
        let request = match built {
            Ok(request) => request,
            Err((field, error)) => {
                state.draft.invalid = Some(field);
                self.dirty = true;
                return self.answer_run(mode, reply, Err(error));
            }
        };
        let captured = Captured::of(&state.queue);
        self.dirty |= unflagged;
        let begun = self.coordinator.begin(mode);
        debug_assert!(begun, "an idle coordinator begins");
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

    /// Answers a Run of `mode` like `answer`, writing a refusal or a failed start into the mode's
    /// Run Log first.
    fn answer_run(&mut self, mode: PipelineMode, reply: Reply, answer: Result<bool, String>) {
        if let Err(error) = &answer {
            self.log(mode, &[LogLine::error(format!("ERROR: {error}"))]);
        }
        self.answer(reply, answer);
    }

    /// A start's request resolved: its Effective Settings are published, the mode's Run Log is
    /// cleared for the batch, and the forced scan of its input directory begins. A refused one
    /// ends the start, and its error stays under the lines before it.
    pub(super) fn request_resolved(
        &mut self,
        attempt: u64,
        result: Result<EffectiveSettings, String>,
    ) {
        let Some(start) = self.start.as_mut().filter(|start| start.attempt == attempt) else {
            return;
        };
        if self.coordinator.phase() == Phase::Stopping {
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
                let cleared = self.mode(mode).log.clear();
                self.publisher.logs(&cleared);
                // Published at once: a quick preflight would otherwise end the start within one
                // publication interval, and the view would never see its settings.
                self.flush();
                self.scan(mode, path, true, no_cache, ScanReply::Preflight(attempt));
            }
        }
    }

    /// A start's forced scan settled: preflight checks the captured Queue Selection against it,
    /// and the job launches off the actor.
    pub(super) fn preflight_scanned(&mut self, attempt: u64, answer: Result<bool, String>) {
        let Some(start) = self.start.as_mut().filter(|start| start.attempt == attempt) else {
            return;
        };
        if self.coordinator.phase() == Phase::Stopping {
            return;
        }
        if answer != Ok(true) {
            return self.finish_start(Outcome::Failed, Err(INSPECTION_FAILED.into()));
        }
        // Field by field, so the start stays borrowed across its mode's queue.
        let queue = match start.mode {
            PipelineMode::Remux => &self.remux.queue,
            PipelineMode::Extract => &self.extract.queue,
        };
        let batch = match preflight(start.mode, &start.captured, queue) {
            Ok(batch) => batch,
            Err(error) => return self.finish_start(Outcome::Failed, Err(error)),
        };
        let Some(inbox) = self.inbox.upgrade() else {
            return;
        };
        let sink = CoreHandle(inbox).job_sink();
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
    pub(super) fn launched(&mut self, attempt: u64, result: Result<u64, String>) {
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
    pub(super) fn stop_batch(&mut self, mode: PipelineMode, reply: Reply) {
        let owned = self
            .start
            .as_ref()
            .filter(|_| self.coordinator.owner() == Some(mode))
            .map(|start| (start.attempt, start.step, start.job_id));
        let Some((attempt, step, job_id)) = owned else {
            return self.answer(reply, Ok(false));
        };
        // Only a launched job's teardown is announced: a start still launching stops once it
        // has, and one before that is cancelled at once.
        if self.coordinator.phase() == Phase::Running {
            self.log(mode, &[LogLine::note(STOP_REQUESTED)]);
        }
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
        let Some(start) = self.current(attempt) else {
            return;
        };
        start.stop_issued = true;
        let mode = start.mode;
        let launcher = Arc::clone(&self.launcher);
        self.off_actor(async move { launcher.stop(job_id).await }, move |result| {
            Msg::StopSettled {
                attempt,
                mode,
                result,
            }
        });
    }

    /// A teardown is over. Before the launch it cancelled the start. After it the job's `Done`
    /// came first (the supervisor sends it before it releases the Job Slot, which the teardown
    /// waits on) and ended the start, unless the teardown failed, which leaves the job running.
    /// A failed teardown is written into the Run Log of the start's `mode` once, however many
    /// Stops wait on it.
    pub(super) fn stop_settled(
        &mut self,
        attempt: u64,
        mode: PipelineMode,
        result: Result<(), String>,
    ) {
        if let Err(error) = &result {
            self.log(mode, &[LogLine::error(format!("ERROR: {error}"))]);
        }
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
    pub(super) fn finish_start(&mut self, outcome: Outcome, run_answer: Result<bool, String>) {
        let Some(start) = self.start.take() else {
            return;
        };
        self.coordinator.finish();
        self.run_settings = None;
        self.dirty = true;
        self.set_outcome(start.mode, Some(outcome));
        let stops = if start.stop_issued {
            Vec::new()
        } else {
            self.take_stop_waiters(start.attempt)
        };
        self.flush();
        if let Some(reply) = start.run_reply {
            self.answer_run(start.mode, reply, run_answer);
        }
        for reply in stops {
            let _ = reply.send(Ok(true));
        }
    }
}

#[cfg(test)]
mod tests {
    // The actor's fakes and helpers, shared with its own tests.
    use crate::actor::tests::*;
    use crate::actor::*;
    use crate::queue::test_support::manifest_of;
    use crate::snapshot::test_support::effective_settings;

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
    async fn test_a_failed_teardown_while_resolving_answers_the_stop_its_error_and_still_cancels() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        launcher.script().stop_error = Some("Timed out waiting for job cleanup to finish.".into());
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE / 2).await;

        assert_eq!(
            core.dispatch(stop(REMUX)).await,
            Err("Timed out waiting for job cleanup to finish.".into())
        );
        assert_eq!(running.await.unwrap(), Ok(false));
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
    async fn test_a_stop_that_cancels_the_forced_inspection_leaves_the_scan_idle_and_the_queue() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;
        let rows = core.state().await.unwrap().remux.rows;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + INSPECTION / 2).await;

        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        // The Stop cancels the forced inspection, which then settles as a failure.
        inspector.manifests.lock().unwrap().remove("/anime");
        assert_eq!(running.await.unwrap(), Ok(false));
        tokio::time::sleep(INSPECTION).await;
        assert_eq!(inspector.inspected().len(), 2);
        let state = core.state().await.unwrap();
        assert_eq!(state.remux.outcome, Some(Outcome::Cancelled));
        assert_eq!(
            state.remux.scan,
            ScanView::Idle,
            "a Stop is not a failed scan"
        );
        assert_eq!(state.remux.rows, rows);
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

    /// The texts of `mode`'s Run Log, with its generation.
    async fn log_of(core: &CoreHandle, mode: PipelineMode) -> (u64, Vec<String>) {
        let log = core.log(mode).await.unwrap();
        let texts = log.lines.into_iter().map(|line| line.text).collect();
        (log.generation, texts)
    }

    #[tokio::test(start_paused = true)]
    async fn test_every_run_answered_with_an_error_writes_it_into_its_modes_run_log() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;

        // Refused before its request resolved: logged, with nothing cleared.
        let refused = "Worker thread count must be between 1 and 64";
        launcher.script().resolve_error = Some(refused.into());
        assert!(core.dispatch(run(REMUX)).await.is_err());
        assert_eq!(
            log_of(&core, REMUX).await,
            (0, vec![format!("ERROR: {refused}")])
        );
        let line = &core.log(REMUX).await.unwrap().lines[0];
        assert_eq!(line.kind, mkv_core::log_line::LogKind::Error);

        // A failed launch, after the resolve cleared the log.
        launcher.script().resolve_error = None;
        let closing = "Application is closing; no new job can start.";
        launcher.script().launch_error = Some(closing.into());
        assert!(core.dispatch(run(REMUX)).await.is_err());
        assert_eq!(
            log_of(&core, REMUX).await,
            (1, vec![format!("ERROR: {closing}")])
        );
        launcher.script().launch_error = None;

        // A draft that builds no request, a scan in progress and a held batch.
        type_input(&core, EXTRACT, "").await;
        assert!(core.dispatch(run(EXTRACT)).await.is_err());
        let scanning = pending(&core, scan(EXTRACT, "/fast", false));
        tokio::time::sleep(INSPECTION / 2).await;
        assert!(core.dispatch(run(EXTRACT)).await.is_err());
        assert_eq!(scanning.await.unwrap(), Ok(true));
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        assert!(core.dispatch(run(EXTRACT)).await.is_err());
        assert_eq!(
            log_of(&core, EXTRACT).await,
            (
                0,
                vec![
                    "ERROR: Input directory is required.".to_string(),
                    "ERROR: The folder is being scanned; a batch can start once the scan ends."
                        .into(),
                    "ERROR: A job is already running or stopping. Wait for it to finish.".into(),
                ]
            )
        );
        // The remux Run that launched wrote nothing but cleared its own log.
        assert_eq!(log_of(&core, REMUX).await, (2, Vec::<String>::new()));
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_failed_forced_scan_writes_its_error() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, inspector) = run_core(&launcher).await;
        inspector.manifests.lock().unwrap().remove("/anime");
        assert!(core.dispatch(run(REMUX)).await.is_err());
        assert_eq!(
            log_of(&core, REMUX).await,
            (
                1,
                vec!["ERROR: Input inspection failed; job was not started.".to_string()]
            )
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_stop_of_a_launched_job_writes_the_note_and_one_while_launching_does_not() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        let running = pending(&core, run(REMUX));
        tokio::time::sleep(RESOLVE + INSPECTION + LAUNCH / 2).await;
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(running.await.unwrap(), Ok(true));
        assert_eq!(log_of(&core, REMUX).await.1, Vec::<String>::new());

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(true));
        assert_eq!(
            log_of(&core, REMUX).await.1,
            ["Stop requested; waiting for shutdown."]
        );
        let line = &core.log(REMUX).await.unwrap().lines[0];
        assert_eq!(line.kind, mkv_core::log_line::LogKind::Note);
        // A Stop with nothing to stop writes nothing.
        assert_eq!(core.dispatch(stop(REMUX)).await, Ok(false));
        assert_eq!(log_of(&core, REMUX).await.1.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_failed_teardown_is_written_once_however_many_stops_wait_on_it() {
        let launcher = FakeLauncher::default();
        let (core, _publisher, _inspector) = run_core(&launcher).await;
        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let timeout = "Timed out waiting for job cleanup to finish.";
        launcher.script().stop_error = Some(timeout.into());

        let first = pending(&core, stop(REMUX));
        tokio::time::sleep(STOP / 2).await;
        // Stopping already: no second note.
        assert_eq!(core.dispatch(stop(REMUX)).await, Err(timeout.into()));
        assert_eq!(first.await.unwrap(), Err(timeout.into()));
        assert_eq!(
            log_of(&core, REMUX).await.1,
            [
                "Stop requested; waiting for shutdown.".to_string(),
                format!("ERROR: {timeout}"),
            ]
        );
        assert_eq!(core.log(REMUX).await.unwrap().errors, 1);
        assert_eq!(log_of(&core, EXTRACT).await.1, Vec::<String>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn test_the_log_clears_once_per_start_when_its_request_resolves_and_only_its_own() {
        let launcher = FakeLauncher::default();
        let (core, publisher, _inspector) = run_core(&launcher).await;
        type_input(&core, EXTRACT, "").await;
        assert!(core.dispatch(run(EXTRACT)).await.is_err());
        launcher.script().resolve_error = Some("refused".into());
        assert!(core.dispatch(run(REMUX)).await.is_err());
        launcher.script().resolve_error = None;
        let before = publisher.logs().len();

        assert_eq!(core.dispatch(run(REMUX)).await, Ok(true));
        let batches = publisher.logs()[before..].to_vec();
        assert_eq!(batches.len(), 1, "{batches:?}");
        let clear = &batches[0];
        assert_eq!(clear.mode, REMUX);
        assert_eq!((clear.generation, clear.first, clear.keep_from), (1, 0, 0));
        assert!(clear.lines.is_empty());
        assert_eq!(log_of(&core, REMUX).await, (1, Vec::<String>::new()));
        // The other mode keeps its lines.
        assert_eq!(
            log_of(&core, EXTRACT).await,
            (0, vec!["ERROR: Input directory is required.".to_string()])
        );
        // The job's lines follow, under the new generation.
        launcher.script().jobs[0]
            .2
            .logs(&[mkv_core::log_line::LogLine::note("probing")]);
        assert_eq!(log_of(&core, REMUX).await, (1, vec!["probing".to_string()]));
    }
}
