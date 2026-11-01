//! The Slint side of the Desktop Core: the `Publisher` that hands each publication to the window's
//! event loop. The actor publishes on a tokio worker and Slint's properties live on the UI thread,
//! so publications wait in one pending slot, and one hop to the event loop applies whatever
//! gathered there meanwhile. Each mode's rows are mapped and diffed before the hop (`queue.rs`),
//! so the event loop only writes the rows that changed into the mode's kept `VecModel`.

use crate::inspector::{map_inspector, write_form, LANGUAGES};
use crate::jobs;
use crate::queue::{map_rows, ModeList, RowPatch, Update};
use crate::run_log::{self, Console, LogPatch};
use crate::settings::{self, Edits, BITRATES};
use crate::telemetry;
use mkv_anime_batch_slint_ui::{App, Focus, Mode, QueueFilter, QueueRow};
use mkv_core::pipeline::PipelineMode;
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::{LogBatch, LogView};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

/// What waits for the next hop to the event loop.
struct Pending {
    /// The newest View State not yet taken.
    state: Option<ViewState>,
    /// The `seq` of the newest View State ever pushed: an older one is never applied.
    newest: Option<u64>,
    /// Whether a hop is on its way, which takes everything pushed until it runs.
    scheduled: bool,
    /// The remux and the extract list.
    lists: [ModeList; 2],
    /// The remux and the extract console.
    logs: [Console; 2],
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            state: None,
            newest: None,
            scheduled: false,
            lists: [ModeList::new(Mode::Remux), ModeList::new(Mode::Extract)],
            logs: Default::default(),
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
    /// The remux and the extract console's patch.
    pub(crate) logs: [LogPatch; 2],
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

    /// Draws `batch` in its mode's console, or keeps it for after the load. Returns whether to
    /// schedule a hop.
    pub(crate) fn push_log(&self, batch: &LogBatch) -> bool {
        let mut pending = self.lock();
        pending.logs[run_log::slot(batch.mode)].apply(batch);
        !std::mem::replace(&mut pending.scheduled, true)
    }

    /// Loads `mode`'s console with `get_log`'s answer. Returns whether to schedule a hop: not for
    /// a second load, which changes nothing.
    pub(crate) fn load_log(&self, mode: PipelineMode, view: Option<LogView>) -> bool {
        let mut pending = self.lock();
        pending.logs[run_log::slot(mode)].load(view)
            && !std::mem::replace(&mut pending.scheduled, true)
    }

    /// `mode`'s console's raw text, for Copy.
    pub(crate) fn log_text(&self, mode: PipelineMode) -> String {
        self.lock().logs[run_log::slot(mode)].text()
    }

    /// No hop is on its way any more, nor will one come. What is pending stays, bounded (the
    /// newest state, each list's update, the consoles' kept lines).
    fn unschedule(&self) {
        self.lock().scheduled = false;
    }

    /// Everything pushed since the last take: the state, each mode's update and each console's
    /// patch. Clears the scheduled flag under the same lock, so a push after this
    /// schedules its own hop.
    pub(crate) fn take(&self) -> Taken {
        let mut pending = self.lock();
        pending.scheduled = false;
        Taken {
            lists: pending.lists.each_mut().map(ModeList::take),
            state: pending.state.take(),
            logs: pending.logs.each_mut().map(Console::take_patch),
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
    app.set_sheet_limits(settings::limits());
    run_log::install_models(app);
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
        // the same, so a later push never waits on a hop that will not come; the bounded rest of
        // what is pending stays for one (a test's `flush`).
        if scheduled.is_err() {
            self.pending.unschedule();
        }
    }

    /// Loads `mode`'s console with `get_log`'s answer (`None` when it failed).
    pub(crate) fn load_log(&self, mode: PipelineMode, view: Option<LogView>) {
        if self.pending.load_log(mode, view) {
            self.schedule();
        }
    }

    /// `mode`'s Run Log as Copy writes it.
    pub(crate) fn log_text(&self, mode: PipelineMode) -> String {
        self.pending.log_text(mode)
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

    /// Loads `mode`'s console with `view` at once, on the event loop's thread.
    #[cfg(test)]
    pub(crate) fn show_load(&self, mode: PipelineMode, view: Option<LogView>) {
        self.pending.load_log(mode, view);
        hop(&self.pending, &self.edits, &self.window);
    }

    /// Draws `batches` at once, on the event loop's thread, as one hop would.
    #[cfg(test)]
    pub(crate) fn show_logs(&self, batches: &[LogBatch]) {
        for batch in batches {
            self.pending.push_log(batch);
        }
        hop(&self.pending, &self.edits, &self.window);
    }

    /// Applies what is pending at once, on the event loop's thread.
    #[cfg(test)]
    pub(crate) fn flush(&self) {
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
        logs: [remux_log, extract_log],
    } = pending.take();
    let Some(app) = window.upgrade() else {
        return;
    };
    if let Some(state) = state {
        app.set_busy(state.busy);
        jobs::show(&app, &state);
        settings::show(&app, &state, edits);
        telemetry::show(&app, state.telemetry.as_ref());
    }
    apply(&app, Mode::Remux, remux);
    apply(&app, Mode::Extract, extract);
    run_log::apply(&app, Mode::Remux, remux_log);
    run_log::apply(&app, Mode::Extract, extract_log);
}

impl Publisher for SlintPublisher {
    fn state(&self, state: &ViewState) {
        if self.pending.push_state(state) {
            self.schedule();
        }
    }

    fn logs(&self, batch: &LogBatch) {
        if self.pending.push_log(batch) {
            self.schedule();
        }
    }

    // Nothing renders a job's end, as in the webview: the View State it ended in shows it.
    fn done(&self, _done: &JobDone) {}
}

/// Shows `rows` in `model`: in place when it is a `VecModel` of as many rows, writing only the rows
/// that differ, so the repeated control with the focus keeps it (a replaced model makes every row
/// again, and a destroyed control takes the focus with it). Else hands `set` a new model, unless
/// `model` already shows `rows`. Returns whether it did: the caller then hands the focus on.
pub(crate) fn show_rows<T: Clone + PartialEq + 'static>(
    model: &ModelRc<T>,
    rows: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) -> bool {
    if let Some(kept) = model
        .as_any()
        .downcast_ref::<VecModel<T>>()
        .filter(|kept| kept.row_count() == rows.len())
    {
        for (index, row) in rows.into_iter().enumerate() {
            if kept.row_data(index).as_ref() != Some(&row) {
                kept.set_row_data(index, row);
            }
        }
        return false;
    }
    if model.iter().eq(rows.iter().cloned()) {
        return false;
    }
    set(ModelRc::new(VecModel::from(rows)));
    true
}

/// The name queue.slint keys `mode`'s controls by.
fn name(mode: Mode) -> &'static str {
    match mode {
        Mode::Remux => "remux",
        Mode::Extract => "extract",
    }
}

/// The id of `mode`'s row whose control has the focus, from `current` (`Focus.current`):
/// queue.slint keys a row's controls `<control>:<mode>:<row id>`.
fn focused_row(current: &str, mode: Mode) -> Option<&str> {
    let (control, rest) = current.split_once(':')?;
    matches!(control, "check" | "pick" | "up" | "down")
        .then(|| rest.strip_prefix(name(mode))?.strip_prefix(':'))
        .flatten()
}

/// Writes `mode`'s update: its header when it changed, its rows into the mode's model, its Library
/// and Inspector when they changed, then scrolls to a revealed release. A card that replaced the
/// rows, or another folder's view, since the last hop puts the list back at the top, as the
/// webview's `clearList` does: the rows that come after it start there, even when a later head
/// replaced that one before this hop (`Update::to_top`).
fn apply(app: &App, mode: Mode, update: Update) {
    let (rows, head) = match mode {
        Mode::Remux => (app.get_remux_rows(), app.get_remux_head()),
        Mode::Extract => (app.get_extract_rows(), app.get_extract_head()),
    };
    if let Some(new) = update.head.filter(|new| *new != head) {
        // The card's button goes with its action (Open folder…, then the Scanning card).
        let button_gone = !new.card_shown || new.card_action.is_empty();
        match mode {
            Mode::Remux => app.set_remux_head(new),
            Mode::Extract => app.set_extract_head(new),
        }
        if button_gone
            && app.global::<Focus>().get_current() == format!("{}:open-folder", name(mode))
        {
            app.invoke_focus_keys();
        }
    }
    if let Some(items) = update.library {
        let library = match mode {
            Mode::Remux => app.get_remux_library(),
            Mode::Extract => app.get_extract_library(),
        };
        let replaced = show_rows(&library, items, |items| match mode {
            Mode::Remux => app.set_remux_library(items),
            Mode::Extract => app.set_extract_library(items),
        });
        // app.slint shows the Library of `mode` only.
        if replaced
            && app.get_mode() == mode
            && app.global::<Focus>().get_current().starts_with("library:")
        {
            app.invoke_focus_keys();
        }
    }
    if let Some(update) = update.inspector {
        let inspector = map_inspector(update.view.as_ref(), mode, &update.directory);
        // Only remux has the subtitle section; its typed fields are written over the Inspector
        // shown until now, which is set below.
        if let Some(subtitles) = update.subtitles {
            app.set_sub_ladder(ModelRc::new(VecModel::from(subtitles.ladder)));
            app.set_sub_options(ModelRc::new(VecModel::from(subtitles.options)));
            app.set_sub_values(ModelRc::new(VecModel::from(subtitles.values)));
            write_form(app, &app.get_remux_inspector(), &inspector);
        }
        // No episode inspected: nothing typed is left to commit.
        if mode == Mode::Remux && !inspector.shown {
            app.set_sub_dirty(false);
        }
        match mode {
            Mode::Remux => app.set_remux_inspector(inspector),
            Mode::Extract => app.set_extract_inspector(inspector),
        }
    }
    if let Some(model) = rows.as_any().downcast_ref::<VecModel<QueueRow>>() {
        match update.rows {
            // A row whose controls go takes the focus with it, and Slint then delivers no key:
            // the window's key scope takes it, so Esc and Ctrl+T still reach it. A reset drops
            // every row (every Run's rescan resets them); a patch hides a row the filter no longer
            // shows (a status change under To do, mid-run).
            // A reorder resets them too: the moved episode's Move up / Move down takes the focus
            // back as its new row is made, unless it is disabled or not made (scrolled out).
            Some(RowPatch::Reset(rows)) => {
                let focus = app.global::<Focus>();
                let current = focus.get_current();
                let dropped = focused_row(&current, mode);
                let follows = dropped.is_some_and(|id| {
                    (current.starts_with("up:") || current.starts_with("down:"))
                        && rows.iter().any(|row| row.id == id)
                });
                let dropped = dropped.is_some();
                model.set_vec(rows);
                app.invoke_unpin(mode);
                if dropped {
                    app.invoke_focus_keys();
                }
                if follows {
                    focus.set_follow_key(current);
                }
            }
            Some(RowPatch::Rows(rows)) => {
                let current = app.global::<Focus>().get_current();
                let focused = focused_row(&current, mode);
                let mut dropped = false;
                for (index, row) in rows {
                    dropped |= !row.shown && focused == Some(row.id.as_str());
                    model.set_row_data(index, row);
                }
                if dropped {
                    app.invoke_focus_keys();
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

    #[test]
    fn a_burst_schedules_one_hop_that_takes_the_newest_state() {
        let pending = Coalescer::default();
        assert!(pending.push_state(&state(1)));
        assert!(!pending.push_state(&state(3)));
        // An older state never replaces a newer one.
        assert!(!pending.push_state(&state(2)));

        let taken = pending.take();
        assert_eq!(taken.state.map(|s| s.seq), Some(3));
        // Nothing is left, and an older state after the take is still dropped.
        assert!(!pending.push_state(&state(2)));
        assert_eq!(pending.take().state.map(|s| s.seq), None);
        // A newer one after the take schedules its own hop.
        assert!(pending.push_state(&state(4)));
        assert_eq!(pending.take().state.map(|s| s.seq), Some(4));
    }
}
