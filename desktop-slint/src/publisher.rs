//! The Slint side of the Desktop Core: the `Publisher` that hands each publication to the window's
//! event loop. The actor publishes on a tokio worker and Slint's properties live on the UI thread,
//! so publications wait in one pending slot, and one hop to the event loop applies whatever
//! gathered there meanwhile. Each mode's rows are mapped and diffed before the hop (`queue.rs`),
//! so the event loop only writes the rows that changed into the mode's kept `VecModel`.

use crate::inspector::{map_inspector, write_form, LANGUAGES};
use crate::queue::{map_rows, ModeList, RowPatch, Update};
use crate::settings::{self, Edits, BITRATES};
use mkv_anime_batch_slint_ui::{App, Mode, QueueFilter, QueueRow};
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::LogBatch;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

/// What waits for the next hop to the event loop.
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

impl Default for Pending {
    fn default() -> Self {
        Self {
            state: None,
            dones: Vec::new(),
            newest: None,
            scheduled: false,
            lists: [ModeList::new(Mode::Remux), ModeList::new(Mode::Extract)],
        }
    }
}

/// The index of `mode`'s list.
pub(crate) fn index(mode: Mode) -> usize {
    match mode {
        Mode::Remux => 0,
        Mode::Extract => 1,
    }
}

/// What a hop takes.
pub(crate) struct Taken {
    /// The newest state, if one was pushed since.
    pub(crate) state: Option<ViewState>,
    /// The remux and the extract list's update.
    pub(crate) lists: [Update; 2],
    /// The ends of jobs, applied after the state.
    pub(crate) dones: Vec<JobDone>,
}

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
        // Mapped off the lock, so a hop's take never waits on the mapping; the diff against the
        // model and a remap for a filter that changed meanwhile run under it.
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

    /// Keeps `filter` for `mode`, which cancels a reveal not shown yet; its rows show once
    /// [`remap`](Self::remap) mapped them.
    pub(crate) fn set_filter(&self, mode: Mode, filter: QueueFilter) {
        self.lock().lists[index(mode)].set_filter(filter);
    }

    /// Cancels `mode`'s reveal not shown yet: the folder's Library entry scrolls to the top.
    pub(crate) fn cancel_reveal(&self, mode: Mode) {
        self.lock().lists[index(mode)].cancel_reveal();
    }

    /// Shows every row of `mode` and scrolls its list to `release`, once [`remap`](Self::remap)
    /// mapped them.
    pub(crate) fn reveal(&self, mode: Mode, release: String) {
        self.lock().lists[index(mode)].reveal(release);
    }

    /// Maps `mode`'s newest rows with its newest filter, off the lock. A filter or rows that
    /// changed meanwhile were mapped by whoever changed them, so a late remap stages nothing.
    /// Returns whether to schedule a hop.
    pub(crate) fn remap(&self, mode: Mode) -> bool {
        let (filter, view) = {
            let pending = self.lock();
            let list = &pending.lists[index(mode)];
            match &list.view {
                Some(view) => (list.filter(), view.clone()),
                None => return false,
            }
        };
        let rows = map_rows(&view.rows, &view.library, filter);
        let mut pending = self.lock();
        if !pending.lists[index(mode)].put_filtered(&view, filter, rows) {
            return false;
        }
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
        Taken {
            lists: pending.lists.each_mut().map(ModeList::take),
            state: pending.state.take(),
            dones: std::mem::take(&mut pending.dones),
        }
    }
}

/// Publishes to the window through a `slint::Weak`, so a closed window drops publications.
#[derive(Clone)]
pub(crate) struct SlintPublisher {
    window: slint::Weak<App>,
    pending: Arc<Coalescer>,
    /// The draft edits on their way to the core, whose fields a publication leaves alone.
    edits: Edits,
}

/// Gives `app` the two modes' row models, which every publication then updates in place, and the
/// language select's entries. Call it once, before the first publication: rows written into
/// another model are lost.
pub(crate) fn install_models(app: &App) {
    app.set_remux_rows(ModelRc::from(Rc::new(VecModel::<QueueRow>::default())));
    app.set_extract_rows(ModelRc::from(Rc::new(VecModel::<QueueRow>::default())));
    let languages: Vec<slint::SharedString> =
        LANGUAGES.iter().map(|(_, label)| (*label).into()).collect();
    app.set_sub_languages(ModelRc::from(Rc::new(VecModel::from(languages))));
    let bitrates: Vec<slint::SharedString> = BITRATES.iter().map(|&entry| entry.into()).collect();
    app.set_bitrates(ModelRc::from(Rc::new(VecModel::from(bitrates))));
}

impl SlintPublisher {
    /// Publishes to `app`, whose row models [`install_models`] put in place.
    pub(crate) fn new(app: &App) -> Self {
        Self {
            window: app.as_weak(),
            pending: Arc::default(),
            edits: Edits::default(),
        }
    }

    /// The draft edits on their way to the core, which the settings sheet sends.
    pub(crate) fn edits(&self) -> Edits {
        Arc::clone(&self.edits)
    }

    fn schedule(&self) {
        let (pending, edits) = (Arc::clone(&self.pending), Arc::clone(&self.edits));
        let window = self.window.clone();
        // Not `upgrade_in_event_loop`: it drops the closure when the window is gone, which would
        // leave the slot scheduled for good. The take always runs; only the apply needs the window.
        let scheduled = slint::invoke_from_event_loop(move || hop(&pending, &edits, &window));
        // Fails only once the event loop is gone, when nothing is left to show. Clear the flag all
        // the same, so a later push never waits on a hop that will not come.
        if scheduled.is_err() {
            self.pending.take();
        }
    }

    /// Keeps `filter` for `mode`, in click order: call it on the event loop, then [`remap`](Self::remap).
    pub(crate) fn set_filter(&self, mode: Mode, filter: QueueFilter) {
        self.pending.set_filter(mode, filter);
    }

    /// Shows every row of `mode` and scrolls its list to `release`'s first, in click order: call
    /// it on the event loop, then [`remap`](Self::remap).
    pub(crate) fn reveal(&self, mode: Mode, release: String) {
        self.pending.reveal(mode, release);
    }

    /// Cancels `mode`'s reveal not shown yet, in click order: call it on the event loop.
    pub(crate) fn cancel_reveal(&self, mode: Mode) {
        self.pending.cancel_reveal(mode);
    }

    /// Shows `mode`'s rows with its newest filter. Maps them on the calling thread, so call it
    /// off the event loop.
    pub(crate) fn remap(&self, mode: Mode) {
        if self.pending.remap(mode) {
            self.schedule();
        }
    }

    /// Applies `state` at once, on the event loop's thread: the hop, without the hop.
    #[cfg(test)]
    pub(crate) fn show(&self, state: &ViewState) {
        self.pending.push_state(state);
        hop(&self.pending, &self.edits, &self.window);
    }

    /// Applies `states` in one hop, as when they all land before the event loop takes them.
    #[cfg(test)]
    pub(crate) fn show_coalesced(&self, states: &[&ViewState]) {
        for state in states {
            self.pending.push_state(state);
        }
        hop(&self.pending, &self.edits, &self.window);
    }

    /// Reveals `release` in `mode` at once, on the event loop's thread, as a Library click would.
    #[cfg(test)]
    pub(crate) fn show_reveal(&self, mode: Mode, release: &str) {
        self.reveal(mode, release.into());
        self.pending.remap(mode);
        hop(&self.pending, &self.edits, &self.window);
    }

    /// Shows `filter`'s rows of `mode` at once, on the event loop's thread, as a click would.
    #[cfg(test)]
    pub(crate) fn show_filter(&self, mode: Mode, filter: QueueFilter) {
        self.set_filter(mode, filter);
        self.pending.remap(mode);
        hop(&self.pending, &self.edits, &self.window);
    }
}

/// Applies everything pending to the window, on the event loop.
pub(crate) fn hop(pending: &Coalescer, edits: &Edits, window: &slint::Weak<App>) {
    let Taken {
        state,
        lists: [remux, extract],
        dones: _unshown,
    } = pending.take();
    let Some(app) = window.upgrade() else {
        return;
    };
    if let Some(state) = state {
        app.set_busy(state.busy);
        settings::show(&app, &state, edits);
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

/// Writes `mode`'s update: its header when it changed, its rows into the mode's model, its Library
/// and Inspector when they changed, then scrolls to a revealed release. A card
/// staged since the last hop puts the list back at the top, as the webview's `clearList` does: the
/// rows that come after it (another folder's, most often) start there, even when they replaced
/// the card in the pending slot before this hop.
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
    if let Some(items) = update.library {
        let items = ModelRc::new(VecModel::from(items));
        match mode {
            Mode::Remux => app.set_remux_library(items),
            Mode::Extract => app.set_extract_library(items),
        }
    }
    if let Some(update) = update.inspector {
        let inspector = map_inspector(update.view.as_ref(), mode);
        if let Some(subtitles) = update.subtitles {
            app.set_sub_ladder(ModelRc::new(VecModel::from(subtitles.ladder)));
            app.set_sub_options(ModelRc::new(VecModel::from(subtitles.options)));
            app.set_sub_values(ModelRc::new(VecModel::from(subtitles.values)));
            write_form(app, &inspector);
        }
        match mode {
            Mode::Remux => app.set_remux_inspector(inspector),
            Mode::Extract => app.set_extract_inspector(inspector),
        }
    }
    if let Some(model) = rows.as_any().downcast_ref::<VecModel<QueueRow>>() {
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
    // After the rows, so the row is the new model's. A row the list scrolls to is measured, where
    // a scroll offset set on a ListView whose rows were replaced lands by its estimated row
    // height (the top's too).
    let reveal = update.reveal.or(update.to_top.then_some(0));
    if let Some(row) = reveal {
        app.invoke_reveal(mode, i32::try_from(row).unwrap_or(i32::MAX));
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

        let taken = pending.take();
        assert_eq!(taken.state.map(|s| s.seq), Some(3));
        assert!(taken.dones.is_empty());
        // Nothing is left, and an older state after the take is still dropped.
        assert!(!pending.push_state(&state(2)));
        assert_eq!(pending.take().state.map(|s| s.seq), None);
        // A newer one after the take schedules its own hop.
        assert!(pending.push_state(&state(4)));
        assert_eq!(pending.take().state.map(|s| s.seq), Some(4));
    }

    #[test]
    fn a_done_is_taken_with_the_state_it_ended_in_and_applied_after_it() {
        let pending = Coalescer::default();
        assert!(pending.push_state(&state(5)));
        assert!(!pending.push_done(&done(7)));

        let Taken { state, dones, .. } = pending.take();
        assert_eq!(state.map(|s| s.seq), Some(5));
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [7]);

        // A done alone schedules a hop; the state it ended in went out with an earlier one.
        assert!(pending.push_done(&done(8)));
        let Taken { state, dones, .. } = pending.take();
        assert!(state.is_none());
        assert_eq!(dones.iter().map(|d| d.job_id).collect::<Vec<_>>(), [8]);
    }
}
