//! The telemetry cadence of the Desktop Core actor (ADR-0016, S4): the System Telemetry Provider
//! is sampled every second while a start or a job holds the batch and every 30 seconds otherwise,
//! never while the view is hidden, and never twice at once. A sample is taken at once when the
//! actor starts, when the view is shown again and when the batch is taken or released.

use super::{Core, Msg, Publisher};
use crate::control::Launcher;
use crate::inspection::Inspector;
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
            poll_at: None,
            busy: false,
            view: None,
        }
    }
}

impl<P: Publisher, I: Inspector, L: Launcher, S: Sampler> Core<P, I, L, S> {
    /// Samples now and polls from now on at the period the batch calls for, unless the view is
    /// hidden, which stops polling.
    pub(super) fn restart_telemetry(&mut self) {
        if !self.telemetry.visible {
            self.telemetry.poll_at = None;
            return;
        }
        let period = if self.telemetry.busy {
            BUSY_PERIOD
        } else {
            IDLE_PERIOD
        };
        self.telemetry.poll_at = Some(Instant::now() + period);
        self.sample();
    }

    /// The poll fired: the next one is due a period after it was.
    pub(super) fn poll_due(&mut self) {
        let period = if self.telemetry.busy {
            BUSY_PERIOD
        } else {
            IDLE_PERIOD
        };
        self.telemetry.poll_at = self.telemetry.poll_at.map(|at| at + period);
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
        true
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
    }
}

#[cfg(test)]
mod tests {
    use super::{BUSY_PERIOD, IDLE_PERIOD};
    use crate::actor::tests::*;
    use crate::actor::*;
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
        tokio::time::sleep(PUBLISH_INTERVAL).await;
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
        tokio::time::sleep(PUBLISH_INTERVAL).await;
        let shown = core.state().await.unwrap().telemetry.unwrap();
        assert_eq!(shown.cpu, "2%");
        tokio::time::sleep(IDLE_PERIOD).await;
        settle().await;
        tokio::time::sleep(PUBLISH_INTERVAL).await;
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
        assert_eq!(core.dispatch(visible(false)).await, Ok(true));
        // The first sample's publication.
        tokio::time::sleep(PUBLISH_INTERVAL).await;
        let (calls, published) = (sampler.calls(), publisher.all().len());

        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(sampler.calls(), calls);
        assert_eq!(publisher.all().len(), published);
    }
}
