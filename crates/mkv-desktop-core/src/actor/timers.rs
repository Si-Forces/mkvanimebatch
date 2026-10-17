//! The telemetry cadence of the Desktop Core actor (ADR-0016, S4): the System Telemetry Provider
//! is sampled every second while a start or a job holds the batch and every 30 seconds otherwise,
//! never while the view is hidden, and never twice at once. A sample is taken at once when the
//! actor starts, when the view is shown again and when the batch is taken or released (or as soon
//! as the sample in flight is back).
//!
//! The Presentation Heartbeat (S4c) beats once a second while the batch clock runs (a job is
//! active, past a `BatchStart` that named an item, and its batch has not ended) and the view is
//! visible, on whole seconds of the job's clock. A beat only marks the job's mode changed: the
//! publication it leads to presents the elapsed times and the clock as of then, within the
//! publication cap. Showing the view again refreshes them at once.

use super::{Core, Msg, Publisher};
use crate::control::Launcher;
use crate::inspection::Inspector;
use crate::snapshot::BatchStatus;
use crate::telemetry::{Sampler, TelemetryData, TelemetryView};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

/// The sampling period while a start or a job holds the batch.
pub(super) const BUSY_PERIOD: Duration = Duration::from_secs(1);
/// The sampling period otherwise.
pub(super) const IDLE_PERIOD: Duration = Duration::from_secs(30);

/// Where the cadence stands, and the telemetry sampled so far.
pub(super) struct Telemetry {
    /// Whether the view shows: until it says otherwise, it does.
    visible: bool,
    /// Whether a sample is in flight: a poll due meanwhile is skipped.
    sampling: bool,
    /// Whether a restart found a sample in flight: one more is taken when it comes back.
    resample: bool,
    /// When the next poll is due: `None` while hidden.
    pub(super) poll_at: Option<Instant>,
    /// Whether the batch was held when the cadence was last set.
    busy: bool,
    /// `None` until the first sample.
    pub(super) view: Option<TelemetryView>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            visible: true,
            sampling: false,
            resample: false,
            poll_at: None,
            busy: false,
            view: None,
        }
    }
}

impl<P: Publisher, I: Inspector, L: Launcher, S: Sampler> Core<P, I, L, S> {
    /// Samples now, or as soon as the sample in flight is back, and polls from now on at the
    /// period the batch calls for, unless the view is hidden, which stops polling.
    pub(super) fn restart_telemetry(&mut self) {
        if !self.telemetry.visible {
            self.telemetry.poll_at = None;
            self.telemetry.resample = false;
            return;
        }
        let period = if self.telemetry.busy {
            BUSY_PERIOD
        } else {
            IDLE_PERIOD
        };
        self.telemetry.poll_at = Some(Instant::now() + period);
        // The in-flight sample may predate the restart: take another once it is back.
        if self.telemetry.sampling {
            self.telemetry.resample = true;
        } else {
            self.sample();
        }
    }

    /// Polls if the poll is due: the next one is due a period after it was, or a period from now
    /// when that is past already (after a suspend or a stall), so overdue polls never burst.
    pub(super) fn poll_if_due(&mut self) {
        let now = Instant::now();
        let Some(at) = self.telemetry.poll_at.filter(|&at| at <= now) else {
            return;
        };
        let period = if self.telemetry.busy {
            BUSY_PERIOD
        } else {
            IDLE_PERIOD
        };
        let next = at + period;
        self.telemetry.poll_at = Some(if next <= now { now + period } else { next });
        self.sample();
    }

    /// Restarts the cadence when the batch was taken or released since it was set.
    pub(super) fn follow_busy(&mut self) {
        let busy = self.busy();
        if busy != self.telemetry.busy {
            self.telemetry.busy = busy;
            self.restart_telemetry();
        }
    }

    /// The view was shown or hidden: answers whether that changed anything.
    pub(super) fn view_visible(&mut self, visible: bool) -> bool {
        if visible == self.telemetry.visible {
            return false;
        }
        self.telemetry.visible = visible;
        self.restart_telemetry();
        // The labels and the clock stood still while hidden.
        if self.beat_runs() {
            self.touch_job();
        }
        true
    }

    /// Whether the Presentation Heartbeat runs: the view is visible and the batch clock runs.
    fn beat_runs(&self) -> bool {
        self.telemetry.visible
            && self.active
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.status == BatchStatus::Running && !snapshot.ordered_item_ids.is_empty()
            })
    }

    /// Fires a due beat, then arms the next on the job clock's next whole second, or disarms
    /// the heartbeat when it no longer runs. Called after every step of the loop, so a steady
    /// inbox cannot starve it.
    pub(super) fn beat_if_due(&mut self) {
        let (true, Some(started_at)) = (self.beat_runs(), self.job_started_at) else {
            self.beat_at = None;
            return;
        };
        let now = Instant::now();
        if self.beat_at.is_some_and(|at| at <= now) {
            self.touch_job();
            self.beat_at = None;
        }
        if self.beat_at.is_none() {
            let beats = now.saturating_duration_since(started_at).as_secs() + 1;
            self.beat_at = Some(started_at + Duration::from_secs(beats));
        }
    }

    /// Takes a sample off the actor, unless one is in flight.
    fn sample(&mut self) {
        if self.telemetry.sampling {
            return;
        }
        self.telemetry.sampling = true;
        let sampler = Arc::clone(&self.sampler);
        self.off_actor(async move { sampler.sample().await }, Msg::Sampled);
    }

    /// A sample came back: it shows with the next publication, which it does not force. A failed
    /// one keeps the last values.
    pub(super) fn sampled(&mut self, result: Result<TelemetryData, String>) {
        self.telemetry.sampling = false;
        // Nothing to show for an error: the next poll tries again.
        if let Ok(data) = result {
            self.telemetry.view.get_or_insert_default().push(&data);
            self.dirty = true;
        }
        // Hiding the view clears the flag, so this never samples while hidden. The poll
        // deadline the restart set stays as it is.
        if std::mem::take(&mut self.telemetry.resample) {
            self.sample();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BUSY_PERIOD, IDLE_PERIOD};
    use crate::actor::tests::*;
    use crate::actor::*;
    use crate::snapshot::test_support::{batch_complete, batch_start, item_complete, item_start};
    use crate::telemetry::TelemetryData;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// A `Sampler` that counts its calls, takes `delay` on the paused clock, and fails the calls
    /// `fail` names (counted from 1).
    #[derive(Clone, Default)]
    struct FakeSampler {
        calls: Arc<AtomicUsize>,
        delay: Duration,
        fail: Arc<Mutex<Vec<usize>>>,
    }

    impl FakeSampler {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Sampler for FakeSampler {
        async fn sample(&self) -> Result<TelemetryData, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(self.delay).await;
            if self.fail.lock().unwrap().contains(&call) {
                return Err("sampling failed".into());
            }
            Ok(TelemetryData {
                cpu_pct: call as f32,
                ..TelemetryData::default()
            })
        }
    }

    fn core_with(sampler: FakeSampler, launcher: FakeLauncher) -> (CoreHandle, RecordingPublisher) {
        let publisher = RecordingPublisher::default();
        let inspector = FakeInspector::default()
            .with("/anime", crate::queue::test_support::manifest_of(&["ep1"]));
        let core = ready_with_ports(publisher.clone(), inspector, launcher, sampler);
        (core, publisher)
    }

    /// Lets the tasks the actor spawned run, without moving the paused clock.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn visible(visible: bool) -> Intent {
        Intent::ViewVisible { visible }
    }

    const BEAT: Duration = Duration::from_secs(1);

    /// A core whose telemetry sample never comes back, so only the job and the heartbeat
    /// publish, with job 1 started at the instant it returns last.
    async fn beating_core() -> (CoreHandle, RecordingPublisher, ActorSink, Instant) {
        let publisher = RecordingPublisher::default();
        let core = ready(publisher.clone(), FakeInspector::default());
        let sink = core.job_sink();
        let start = Instant::now();
        sink.started(&started(1));
        settle().await;
        (core, publisher, sink, start)
    }

    /// The first remux row's status label and the remux clock, as last published.
    async fn shown(core: &CoreHandle) -> (String, String) {
        let state = core.state().await.unwrap();
        (state.remux.rows[0].label.clone(), state.remux.clock)
    }

    fn pair(label: &str, clock: &str) -> (String, String) {
        (label.into(), clock.into())
    }

    #[tokio::test(start_paused = true)]
    async fn test_the_heartbeat_advances_the_labels_and_the_clock_each_second() {
        let (core, publisher, sink, start) = beating_core().await;
        sink.event(&batch_start(&["ep1", "ep2"]));
        sink.event(&item_start("ep1"));
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(shown(&core).await, pair("Processing · 0s", "0s elapsed"));
        let published = publisher.states().len();
        for second in 1..=3u32 {
            tokio::time::sleep_until(start + BEAT * second).await;
            settle().await;
            assert_eq!(
                shown(&core).await,
                pair(
                    &format!("Processing · {second}s"),
                    &format!("{second}s elapsed")
                )
            );
            // One publication a beat, and only the job's rows and clock changed.
            assert_eq!(publisher.states().len(), published + second as usize);
        }
        // A queued participant shows no time.
        assert_eq!(core.state().await.unwrap().remux.rows[1].label, "Queued");
    }

    #[tokio::test(start_paused = true)]
    async fn test_the_beats_fall_on_whole_seconds_of_the_jobs_clock() {
        let (core, _publisher, sink, start) = beating_core().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        sink.event(&batch_start(&["ep1"]));
        sink.event(&item_start("ep1"));
        tokio::time::sleep_until(start + BEAT - Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(shown(&core).await, pair("Processing · 0s", "0s elapsed"));
        tokio::time::sleep(Duration::from_millis(1)).await;
        settle().await;
        // The item started 300 ms into the job: its label lags the clock.
        assert_eq!(shown(&core).await, pair("Processing · 0s", "1s elapsed"));
        tokio::time::sleep_until(start + BEAT * 2).await;
        settle().await;
        assert_eq!(shown(&core).await, pair("Processing · 1s", "2s elapsed"));
    }

    #[tokio::test(start_paused = true)]
    async fn test_the_heartbeat_runs_only_while_the_batch_clock_runs() {
        let (core, publisher, sink, _) = beating_core().await;
        // Before `BatchStart` the clock has not started.
        tokio::time::sleep(BEAT * 3).await;
        assert_eq!(publisher.states().len(), 1);
        sink.event(&batch_start(&["ep1"]));
        sink.event(&item_start("ep1"));
        tokio::time::sleep(BEAT * 2).await;
        sink.event(&item_complete("ep1", 2.5));
        sink.event(&batch_complete(true, false));
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(shown(&core).await, pair("Done in 2.5s", "Finished"));
        let published = publisher.states().len();
        // The batch ended, though the job is still active until its done.
        tokio::time::sleep(BEAT * 10).await;
        assert_eq!(publisher.states().len(), published);
    }

    #[tokio::test(start_paused = true)]
    async fn test_hiding_the_view_pauses_the_heartbeat_and_showing_it_refreshes_at_once() {
        let (core, publisher, sink, start) = beating_core().await;
        sink.event(&batch_start(&["ep1"]));
        sink.event(&item_start("ep1"));
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        let published = publisher.states().len();
        tokio::time::sleep(BEAT * 5).await;
        assert_eq!(publisher.states().len(), published);

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(core.dispatch(visible(true)).await, Ok(true));
        // At once, not at the next beat: the last publication is long past.
        settle().await;
        assert_eq!(publisher.states().len(), published + 1);
        assert_eq!(shown(&core).await, pair("Processing · 5s", "5s elapsed"));
        // Then on the job clock's whole seconds again.
        tokio::time::sleep_until(start + BEAT * 6).await;
        settle().await;
        assert_eq!(shown(&core).await, pair("Processing · 6s", "6s elapsed"));
    }

    #[tokio::test(start_paused = true)]
    async fn test_telemetry_is_sampled_at_once_then_every_idle_period() {
        let sampler = FakeSampler::default();
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(sampler.calls(), 1);

        tokio::time::sleep(IDLE_PERIOD - Duration::from_millis(1)).await;
        assert_eq!(sampler.calls(), 1);
        tokio::time::sleep(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
        // The sample is published, not forced: within one publication interval.
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        let telemetry = core.state().await.unwrap().telemetry.unwrap();
        assert_eq!(telemetry.cpu, "2%");
        assert_eq!(Vec::from(telemetry.samples.cpu), [1.0, 2.0]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_telemetry_is_sampled_every_second_while_the_batch_is_held() {
        let sampler = FakeSampler::default();
        let launcher = FakeLauncher::default();
        let (core, _publisher) = core_with(sampler.clone(), launcher.clone());
        core.dispatch(scan(REMUX, "/anime", false)).await.unwrap();
        settle().await;
        let before = sampler.calls();
        assert_eq!(before, 1);

        // A Run that holds the batch samples at once, then every second.
        let changes = DraftChanges {
            input_dir: Some("/anime".into()),
            ..DraftChanges::default()
        };
        core.dispatch(edit(REMUX, changes)).await.unwrap();
        let run = tokio::spawn({
            let core = core.clone();
            async move { core.dispatch(Intent::Run { mode: REMUX }).await }
        });
        settle().await;
        assert_eq!(sampler.calls(), 2);
        assert_eq!(run.await.unwrap(), Ok(true));
        let at_launch = sampler.calls();
        tokio::time::sleep(BUSY_PERIOD * 3).await;
        settle().await;
        assert_eq!(sampler.calls(), at_launch + 3);

        // Released: sampled at once, then at the idle period again.
        launcher.end_job(0);
        settle().await;
        let released = sampler.calls();
        assert_eq!(released, at_launch + 4);
        tokio::time::sleep(BUSY_PERIOD * 3).await;
        assert_eq!(sampler.calls(), released);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_poll_due_while_a_sample_is_in_flight_is_skipped() {
        let sampler = FakeSampler {
            delay: Duration::from_secs(65),
            ..FakeSampler::default()
        };
        let (_core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(sampler.calls(), 1);
        // Two polls come due while the first sample takes 65 s: both are skipped.
        tokio::time::sleep(Duration::from_secs(64)).await;
        assert_eq!(sampler.calls(), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(sampler.calls(), 1);
        // The next poll after it came back samples again.
        tokio::time::sleep(Duration::from_secs(24)).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_restart_while_a_sample_is_in_flight_samples_again_when_it_is_back() {
        let sampler = FakeSampler {
            delay: Duration::from_secs(5),
            ..FakeSampler::default()
        };
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(sampler.calls(), 1);
        // Hidden and shown again at 1 s, with the first sample still in flight.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        assert_eq!(core.dispatch(visible(true)).await, Ok(true));
        settle().await;
        assert_eq!(sampler.calls(), 1);
        // Back at 5 s: the restart's sample is taken at once.
        tokio::time::sleep(Duration::from_secs(4)).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
        // The next poll is still a period after the restart, at 31 s.
        tokio::time::sleep(Duration::from_secs(26) - Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
        tokio::time::sleep(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(sampler.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_hiding_the_view_drops_a_pending_resample() {
        let sampler = FakeSampler {
            delay: Duration::from_secs(5),
            ..FakeSampler::default()
        };
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        assert_eq!(core.dispatch(visible(true)).await, Ok(true));
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        // The first sample comes back with the view hidden: nothing more is sampled.
        tokio::time::sleep(IDLE_PERIOD * 3).await;
        settle().await;
        assert_eq!(sampler.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_an_overdue_poll_samples_once_and_the_next_is_a_period_away() {
        let sampler = FakeSampler::default();
        let (_core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(sampler.calls(), 1);
        // A stall: the clock jumps ten and a half periods with the actor not running.
        tokio::time::advance(IDLE_PERIOD * 10 + IDLE_PERIOD / 2).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
        // No catch-up: the next poll is a whole period after the overdue one.
        tokio::time::sleep(IDLE_PERIOD - Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(sampler.calls(), 2);
        tokio::time::sleep(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(sampler.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_steady_inbox_stream_does_not_starve_a_due_poll() {
        let sampler = FakeSampler::default();
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        tokio::time::sleep(IDLE_PERIOD - Duration::from_millis(1)).await;
        assert_eq!(sampler.calls(), 1);
        // Each turn queues far more messages than the actor drains between two (its coop
        // budget), so its inbox never empties while the poll comes due.
        let burst = |core: &CoreHandle| {
            for _ in 0..1000 {
                core.send(Msg::GetState(tokio::sync::oneshot::channel().0));
            }
        };
        let feeder = tokio::spawn({
            let sampler = sampler.clone();
            async move {
                burst(&core);
                tokio::time::advance(Duration::from_millis(2)).await;
                for _ in 0..20 {
                    burst(&core);
                    tokio::task::yield_now().await;
                }
                sampler.calls()
            }
        });
        assert_eq!(feeder.await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_hiding_the_view_pauses_polling_and_showing_it_samples_at_once() {
        let sampler = FakeSampler::default();
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        assert_eq!(core.dispatch(visible(false)).await, Ok(false));
        tokio::time::sleep(IDLE_PERIOD * 3).await;
        assert_eq!(sampler.calls(), 1);

        assert_eq!(core.dispatch(visible(true)).await, Ok(true));
        settle().await;
        assert_eq!(sampler.calls(), 2);
        assert_eq!(core.dispatch(visible(true)).await, Ok(false));
        tokio::time::sleep(IDLE_PERIOD).await;
        settle().await;
        assert_eq!(sampler.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_hiding_the_view_pauses_polling_while_the_batch_is_held() {
        let sampler = FakeSampler::default();
        let launcher = FakeLauncher::default();
        let (core, _publisher) = core_with(sampler.clone(), launcher.clone());
        let sink = core.job_sink();
        // The first sample is back, so the next is not skipped as overlapping.
        tokio::time::sleep(Duration::from_millis(1)).await;
        // A job that holds the batch: its start and end both reach the actor.
        sink.started(&started(1));
        settle().await;
        let busy = sampler.calls();
        assert_eq!(busy, 2);
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        tokio::time::sleep(BUSY_PERIOD * 10).await;
        assert_eq!(sampler.calls(), busy);
        // Released while hidden: still nothing.
        sink.done(&done(1, 0));
        tokio::time::sleep(BUSY_PERIOD * 10).await;
        assert_eq!(sampler.calls(), busy);
    }

    #[tokio::test(start_paused = true)]
    async fn test_a_failed_sample_keeps_the_last_values_and_the_polls_go_on() {
        let sampler = FakeSampler {
            fail: Arc::new(Mutex::new(vec![1, 3])),
            ..FakeSampler::default()
        };
        let (core, _publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        assert_eq!(sampler.calls(), 1);
        assert_eq!(core.state().await.unwrap().telemetry, None);

        tokio::time::sleep(IDLE_PERIOD).await;
        settle().await;
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        let shown = core.state().await.unwrap().telemetry.unwrap();
        assert_eq!(shown.cpu, "2%");
        tokio::time::sleep(IDLE_PERIOD).await;
        settle().await;
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        assert_eq!(sampler.calls(), 3);
        assert_eq!(core.state().await.unwrap().telemetry, Some(shown));
        tokio::time::sleep(IDLE_PERIOD).await;
        settle().await;
        assert_eq!(sampler.calls(), 4);
    }

    /// ADR-0016 S4 acceptance: no timer in the core runs while nothing is in flight and the view
    /// is hidden.
    #[tokio::test(start_paused = true)]
    async fn test_no_timer_runs_while_idle_and_hidden() {
        let sampler = FakeSampler::default();
        let (core, publisher) = core_with(sampler.clone(), FakeLauncher::default());
        settle().await;
        // A job that ran a batch and ended leaves no heartbeat behind.
        let sink = core.job_sink();
        sink.started(&started(1));
        sink.event(&batch_start(&["ep1"]));
        sink.event(&item_start("ep1"));
        tokio::time::sleep(BEAT * 2).await;
        sink.event(&item_complete("ep1", 2.0));
        sink.event(&batch_complete(true, false));
        sink.done(&done(1, 0));
        drop(sink);
        settle().await;
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        // The first sample's publication.
        tokio::time::sleep(PUBLISH_INTERVAL * 2).await;
        let (calls, published) = (sampler.calls(), publisher.all().len());

        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(sampler.calls(), calls);
        assert_eq!(publisher.all().len(), published);
    }
}
