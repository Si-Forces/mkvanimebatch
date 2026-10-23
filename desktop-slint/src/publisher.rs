//! The Slint side of the Desktop Core: the `Publisher` that hands each publication to the window's
//! event loop. The actor publishes on a tokio worker and Slint's properties live on the UI thread,
//! so publications wait in one pending slot, and one hop to the event loop applies whatever
//! gathered there meanwhile.

use mkv_anime_batch_slint_ui::App;
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::LogBatch;
use std::sync::{Arc, Mutex, PoisonError};

/// What waits for the next hop to the event loop.
#[derive(Default)]
struct Pending {
    /// The newest View State not yet taken.
    state: Option<ViewState>,
    /// The ends of jobs, in order, each after the state it ended in.
    dones: Vec<JobDone>,
    /// The `seq` of the newest View State ever pushed: an older one is never applied.
    newest: Option<u64>,
    /// Whether a hop is on its way, which takes everything pushed until it runs.
    scheduled: bool,
}

/// The pending slot. A push answers whether the caller must schedule a hop; the hop calls `take`.
#[derive(Default)]
pub(crate) struct Coalescer(Mutex<Pending>);

impl Coalescer {
    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        // A panic while held leaves plain data; the next publication overwrites it.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `state` unless a state as new is already pushed. Returns whether to schedule a hop.
    pub(crate) fn push_state(&self, state: &ViewState) -> bool {
        let mut pending = self.lock();
        if pending.newest.is_some_and(|newest| newest >= state.seq) {
            return false;
        }
        pending.newest = Some(state.seq);
        pending.state = Some(state.clone());
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Queues `done` behind the states pushed before it. Returns whether to schedule a hop.
    pub(crate) fn push_done(&self, done: &JobDone) -> bool {
        let mut pending = self.lock();
        pending.dones.push(done.clone());
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Everything pushed since the last take: the state to apply first, then the dones. Clears
    /// the scheduled flag under the same lock, so a push after this schedules its own hop.
    pub(crate) fn take(&self) -> (Option<ViewState>, Vec<JobDone>) {
        let mut pending = self.lock();
        pending.scheduled = false;
        (pending.state.take(), std::mem::take(&mut pending.dones))
    }
}

/// Publishes to the window through a `slint::Weak`, so a closed window drops publications.
pub(crate) struct SlintPublisher {
    window: slint::Weak<App>,
    pending: Arc<Coalescer>,
}

impl SlintPublisher {
    pub(crate) fn new(window: slint::Weak<App>) -> Self {
        Self {
            window,
            pending: Arc::default(),
        }
    }

    fn schedule(&self) {
        let pending = Arc::clone(&self.pending);
        let hop = Arc::clone(&pending);
        let scheduled = self.window.upgrade_in_event_loop(move |app| {
            let (state, _dones) = hop.take();
            if let Some(state) = state {
                apply(&app, &state);
            }
            // The job controller renders a job's end (P5); nothing shows it yet.
        });
        // Fails only once the event loop is gone, when nothing is left to show. Clear the flag all
        // the same, so a later push never waits on a hop that will not come.
        if scheduled.is_err() {
            pending.take();
        }
    }
}

impl Publisher for SlintPublisher {
    fn state(&self, state: &ViewState) {
        if self.pending.push_state(state) {
            self.schedule();
        }
    }

    // The Run Log view (P5) reads `get_log` at boot and renders each batch; nothing shows one yet.
    fn logs(&self, _batch: &LogBatch) {}

    fn done(&self, done: &JobDone) {
        if self.pending.push_done(done) {
            self.schedule();
        }
    }
}

/// Binds the published values the window shows. The remux view until P2 brings the mode tabs.
pub(crate) fn apply(app: &App, state: &ViewState) {
    app.set_mode_title(state.remux.title.as_str().into());
    app.set_count_text(state.remux.count_text.as_str().into());
    app.set_busy(state.busy);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(seq: u64) -> ViewState {
        ViewState {
            seq,
            ..ViewState::new(None)
        }
    }

    fn done(job_id: u64) -> JobDone {
        JobDone {
            mode: "remux",
            job_id,
            code: 0,
        }
    }

    #[test]
    fn a_burst_schedules_one_hop_that_takes_the_newest_state() {
        let pending = Coalescer::default();
        assert!(pending.push_state(&state(1)));
        assert!(!pending.push_state(&state(3)));
        // An older state never replaces a newer one.
        assert!(!pending.push_state(&state(2)));

        let (taken, dones) = pending.take();
        assert_eq!(taken.map(|s| s.seq), Some(3));
        assert!(dones.is_empty());
        // Nothing is left, and an older state after the take is still dropped.
        assert!(!pending.push_state(&state(2)));
        assert_eq!(pending.take().0.map(|s| s.seq), None);
        // A newer one after the take schedules its own hop.
        assert!(pending.push_state(&state(4)));
        assert_eq!(pending.take().0.map(|s| s.seq), Some(4));
    }

    #[test]
    fn a_done_is_taken_with_the_state_it_ended_in_and_applied_after_it() {
        let pending = Coalescer::default();
        assert!(pending.push_state(&state(5)));
        assert!(!pending.push_done(&done(7)));

        let (taken, dones) = pending.take();
        assert_eq!(taken.map(|s| s.seq), Some(5));
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [7]);

        // A done alone schedules a hop; the state it ended in went out with an earlier one.
        assert!(pending.push_done(&done(8)));
        let (taken, dones) = pending.take();
        assert!(taken.is_none());
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [8]);
    }
}
