//! The Slint side of the Desktop Core: the `Publisher` that hands each publication to the window's
//! event loop. The actor publishes on a tokio worker and Slint's properties live on the UI thread,
//! so publications wait in one pending slot, and one hop to the event loop applies whatever
//! gathered there meanwhile. Each mode's rows are mapped and diffed before the hop (`queue.rs`),
//! so the event loop only writes the rows that changed into the mode's kept `VecModel`.

use crate::queue::{map_rows, ModeList, RowPatch, Update};
use mkv_anime_batch_slint_ui::{App, Mode, QueueFilter, QueueRow};
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::LogBatch;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
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
    /// The remux and the extract list.
    lists: [ModeList; 2],
}

/// The index of `mode`'s list.
fn index(mode: Mode) -> usize {
    match mode {
        Mode::Remux => 0,
        Mode::Extract => 1,
    }
}

/// What a hop takes: the newest state, each mode's update, and the dones.
pub(crate) type Taken = (Option<ViewState>, [Update; 2], Vec<JobDone>);

/// The pending slot. A push answers whether the caller must schedule a hop; the hop calls `take`.
#[derive(Default)]
pub(crate) struct Coalescer(Mutex<Pending>);

impl Coalescer {
    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        // A panic while held leaves plain data; the next publication overwrites it.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `state` unless a state as new is already pushed, with each mode's update against
    /// what its model holds. Returns whether to schedule a hop.
    pub(crate) fn push_state(&self, state: &ViewState) -> bool {
        let stale = |pending: &Pending| pending.newest.is_some_and(|newest| newest >= state.seq);
        let views = [&state.remux, &state.extract];
        let filters = {
            let pending = self.lock();
            if stale(&pending) {
                return false;
            }
            [0, 1].map(|i| pending.lists[i].wants_rows(views[i]))
        };
        // Mapped off the lock, so a hop's take never waits on it.
        let mut mapped = [0, 1].map(|i| {
            filters[i].map(|filter| (filter, map_rows(&views[i].rows, &views[i].library, filter)))
        });
        let mut pending = self.lock();
        if stale(&pending) {
            return false;
        }
        pending.newest = Some(state.seq);
        for i in [0, 1] {
            pending.lists[i].put(views[i].clone(), mapped[i].take());
        }
        pending.state = Some(state.clone());
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Shows `filter`'s rows of `mode`. Returns whether to schedule a hop.
    pub(crate) fn set_filter(&self, mode: Mode, filter: QueueFilter) -> bool {
        let mut pending = self.lock();
        pending.lists[index(mode)].set_filter(filter);
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Queues `done` behind the states pushed before it. Returns whether to schedule a hop.
    pub(crate) fn push_done(&self, done: &JobDone) -> bool {
        let mut pending = self.lock();
        pending.dones.push(done.clone());
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Everything pushed since the last take: the state and each mode's update to apply first,
    /// then the dones. Clears the scheduled flag under the same lock, so a push after this
    /// schedules its own hop.
    pub(crate) fn take(&self) -> Taken {
        let mut pending = self.lock();
        pending.scheduled = false;
        let lists = pending.lists.each_mut().map(ModeList::take);
        (
            pending.state.take(),
            lists,
            std::mem::take(&mut pending.dones),
        )
    }
}

/// Publishes to the window through a `slint::Weak`, so a closed window drops publications.
#[derive(Clone)]
pub(crate) struct SlintPublisher {
    window: slint::Weak<App>,
    pending: Arc<Coalescer>,
}

impl SlintPublisher {
    /// Gives `app` the two modes' row models, which every publication then updates in place.
    pub(crate) fn new(app: &App) -> Self {
        app.set_remux_rows(ModelRc::from(Rc::new(VecModel::<QueueRow>::default())));
        app.set_extract_rows(ModelRc::from(Rc::new(VecModel::<QueueRow>::default())));
        Self {
            window: app.as_weak(),
            pending: Arc::default(),
        }
    }

    fn schedule(&self) {
        let pending = Arc::clone(&self.pending);
        let window = self.window.clone();
        // Not `upgrade_in_event_loop`: it drops the closure when the window is gone, which would
        // leave the slot scheduled for good. The take always runs; only the apply needs the window.
        let scheduled = slint::invoke_from_event_loop(move || hop(&pending, &window));
        // Fails only once the event loop is gone, when nothing is left to show. Clear the flag all
        // the same, so a later push never waits on a hop that will not come.
        if scheduled.is_err() {
            self.pending.take();
        }
    }

    /// Shows `filter`'s rows of `mode`. Maps them on the calling thread, so call it off the event
    /// loop.
    pub(crate) fn set_filter(&self, mode: Mode, filter: QueueFilter) {
        if self.pending.set_filter(mode, filter) {
            self.schedule();
        }
    }

    /// Applies `state` at once, on the event loop's thread: the hop, without the hop.
    #[cfg(test)]
    pub(crate) fn show(&self, state: &ViewState) {
        self.pending.push_state(state);
        hop(&self.pending, &self.window);
    }
}

/// Applies everything pending to the window, on the event loop.
fn hop(pending: &Coalescer, window: &slint::Weak<App>) {
    let (state, [remux, extract], _dones) = pending.take();
    let Some(app) = window.upgrade() else {
        return;
    };
    if let Some(state) = state {
        app.set_busy(state.busy);
    }
    apply(&app, Mode::Remux, remux);
    apply(&app, Mode::Extract, extract);
    // The job controller renders a job's end (P5); nothing shows it yet.
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

/// Writes `mode`'s update: its header when it changed, and its rows into the mode's model.
fn apply(app: &App, mode: Mode, update: Update) {
    let (rows, head) = match mode {
        Mode::Remux => (app.get_remux_rows(), app.get_remux_head()),
        Mode::Extract => (app.get_extract_rows(), app.get_extract_head()),
    };
    if let Some(new) = update.head.filter(|new| *new != head) {
        match mode {
            Mode::Remux => app.set_remux_head(new),
            Mode::Extract => app.set_extract_head(new),
        }
    }
    let Some(model) = rows.as_any().downcast_ref::<VecModel<QueueRow>>() else {
        return;
    };
    match update.rows {
        Some(RowPatch::Reset(rows)) => model.set_vec(rows),
        Some(RowPatch::Rows(rows)) => {
            for (index, row) in rows {
                model.set_row_data(index, row);
            }
        }
        None => {}
    }
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

        let (taken, _, dones) = pending.take();
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

        let (taken, _, dones) = pending.take();
        assert_eq!(taken.map(|s| s.seq), Some(5));
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [7]);

        // A done alone schedules a hop; the state it ended in went out with an earlier one.
        assert!(pending.push_done(&done(8)));
        let (taken, _, dones) = pending.take();
        assert!(taken.is_none());
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [8]);
    }
}
