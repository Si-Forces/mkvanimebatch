//! The job controls (P5): each mode's published controls mapped into the bottom bar and its tab's
//! status pill, Run and Stop, and the confirm dialog behind "Clear inspection cache…".
//! `job-controller.js` and `app.js` (`showModal`) are the reference. The controls' labels,
//! counts, status and progress are the core's (`presentation.rs`), as is the Close-failed
//! notice's title and text; the cache dialog's texts mirror `index.html`'s markup, and the
//! notice's OK mirrors `app.js` `showCleanupError`.

use crate::intents::{pipeline, Intents};
use crate::settings::open_on_flagged;
use mkv_anime_batch_slint_ui::{App, DialogTexts, JobBar, Mode, Sheet, StatusTone};
use mkv_desktop_core::actor::{Intent, ModeView, ViewState, RUN_BUSY, RUN_SCANNING};
use mkv_desktop_core::presentation::{close_failed_text, CLOSE_FAILED_TITLE};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The status pill's tone for its published style class.
fn tone(class: &str) -> StatusTone {
    match class {
        "status-running" => StatusTone::Running,
        "status-success" => StatusTone::Success,
        "status-error" => StatusTone::Error,
        "status-cancelled" => StatusTone::Cancelled,
        _ => StatusTone::Idle,
    }
}

/// `view`'s controls, queue count, clock and progress as the bottom bar and the tab show them.
pub(crate) fn map_controls(view: &ModeView) -> JobBar {
    let controls = &view.controls;
    // Percent, as the core computes it; f32 is the UI's float.
    let percent = |value: f64| value as f32;
    JobBar {
        run_label: controls.run_label.as_str().into(),
        run_title: controls.run_title.as_str().into(),
        run_disabled: controls.run_disabled,
        owns: controls.owns,
        stop_label: controls.stop_label.into(),
        stop_disabled: controls.stop_disabled,
        tone: tone(controls.status.class),
        status: controls.status.label.into(),
        count_text: view.count_text.as_str().into(),
        clock: view.clock.as_str().into(),
        done: percent(view.progress.done),
        skipped: percent(view.progress.skipped),
        failed: percent(view.progress.failed),
        live: percent(view.progress.live),
    }
}

/// Shows `state`'s job controls, on the event loop; a mode's are written only when they changed,
/// so a heartbeat redraws the clock alone.
pub(crate) fn show(app: &App, state: &ViewState) {
    let remux = map_controls(&state.remux);
    if app.get_remux_job() != remux {
        app.set_remux_job(remux);
    }
    let extract = map_controls(&state.extract);
    if app.get_extract_job() != extract {
        app.set_extract_job(extract);
    }
}

/// A Run of `mode` was answered, on the event loop: Run and the settings unlock, then a refusal
/// of the draft opens the sheet on its flagged field, with Run as the sheet's opener
/// (job-controller.js `start`). The core publishes the flag before it answers, so the hop that
/// shows it ran first. A Run refused because a batch is held or the mode scans is refused before
/// the core reads the draft, which still flags the last refusal's field: it opens nothing.
pub(crate) fn run_answered(app: &App, mode: Mode, answer: Result<bool, String>) {
    // Unlocked first, so the flagged field can take the focus.
    app.set_run_pending(false);
    if let Err(error) = answer {
        // ponytail: told by the core's exported texts (pinned by start.rs's tests), as the answer
        // is the Tauri contract's `Result<bool, String>`; a typed refusal if a third kind comes.
        if error != RUN_BUSY && error != RUN_SCANNING {
            open_on_flagged(app, mode);
        }
    }
}

/// The confirm dialog before a cache clear (job-controller.js `clearCache`).
fn clear_cache_dialog() -> DialogTexts {
    DialogTexts {
        title: "Clear inspection cache".into(),
        text: "Are you sure you want to clear the persistent inspection cache?".into(),
        confirm: "Clear cache".into(),
        notice: false,
    }
}

/// The notice after a failed Close teardown (app.js `showCleanupError`): OK alone.
fn close_failed_dialog(error: &str) -> DialogTexts {
    DialogTexts {
        title: CLOSE_FAILED_TITLE.into(),
        text: close_failed_text(error).into(),
        confirm: "OK".into(),
        notice: true,
    }
}

/// What a confirm dialog's answer runs: `true` for its confirm button.
type Answered = Box<dyn FnOnce(&App, bool)>;

/// The confirm dialogs asked for, the one shown first: a second waits for the first to be
/// answered (app.js `showModal`'s chain).
#[derive(Default)]
pub(crate) struct Dialogs(RefCell<VecDeque<(DialogTexts, Answered)>>);

impl Dialogs {
    /// Shows a dialog of `texts`, or queues it behind the one shown; `answered` runs once it is.
    pub(crate) fn confirm(
        &self,
        app: &App,
        texts: DialogTexts,
        answered: impl FnOnce(&App, bool) + 'static,
    ) {
        let mut queue = self.0.borrow_mut();
        queue.push_back((texts, Box::new(answered)));
        if queue.len() == 1 {
            display(app, &queue[0].0);
        }
    }

    /// The dialog shown was answered: the next one shows, or none, then its answer runs.
    pub(crate) fn answer(&self, app: &App, confirmed: bool) {
        let answered = {
            let mut queue = self.0.borrow_mut();
            let Some((_, answered)) = queue.pop_front() else {
                return;
            };
            match queue.front() {
                Some((next, _)) => display(app, next),
                None => app.set_dialog_open(false),
            }
            answered
        };
        // Off the borrow: an answer may ask for another dialog.
        answered(app, confirmed);
    }
}

fn display(app: &App, texts: &DialogTexts) {
    app.set_dialog(texts.clone());
    app.set_dialog_serial(app.get_dialog_serial().wrapping_add(1));
    app.invoke_show_dialog();
}

/// Wires Run, Stop and "Clear inspection cache…", each for the mode the window shows.
pub(crate) fn wire(app: &App, send: Intents) {
    // A Run sent locks Run and the settings until it is answered (`run-pending`), so a second
    // click sends no second Run.
    let (s, shown) = (send.clone(), app.as_weak());
    app.on_run(move || {
        let Some(app) = shown.upgrade() else {
            return;
        };
        if app.get_run_pending() {
            return;
        }
        app.set_run_pending(true);
        let (mode, window) = (app.get_mode(), shown.clone());
        let intent = Intent::Run {
            mode: pipeline(mode),
        };
        s.send_answered("run", intent, move |answer| {
            let hop = window.upgrade_in_event_loop(move |app| run_answered(&app, mode, answer));
            // Only once the event loop is gone, with nothing left to unlock.
            if let Err(error) = hop {
                tracing::debug!("[run] the answer was dropped: {error}");
            }
        });
    });

    // The core answers once the teardown is over and logs the wait and a failed teardown.
    let (s, shown) = (send.clone(), app.as_weak());
    app.on_stop(move || {
        if let Some(app) = shown.upgrade() {
            let intent = Intent::Stop {
                mode: pipeline(app.get_mode()),
            };
            s.send("stop", intent, || {});
        }
    });

    // Confirmed, the cache of the sheet's mode is cleared; the core logs it, or its error.
    let dialogs = Rc::new(Dialogs::default());
    let (queue, shown) = (Rc::clone(&dialogs), app.as_weak());
    app.global::<Sheet>().on_clear_cache(move || {
        let Some(app) = shown.upgrade().filter(|app| !app.get_locked()) else {
            return;
        };
        let (mode, s) = (app.get_mode(), send.clone());
        queue.confirm(&app, clear_cache_dialog(), move |_, confirmed| {
            if confirmed {
                let intent = Intent::ClearCache {
                    mode: pipeline(mode),
                };
                s.send("clear_cache", intent, || {});
            }
        });
    });
    let (queue, shown) = (Rc::clone(&dialogs), app.as_weak());
    app.on_close_failed(move |error| {
        if let Some(app) = shown.upgrade() {
            queue.confirm(&app, close_failed_dialog(&error), |_, _| {});
        }
    });
    let (queue, shown) = (Rc::clone(&dialogs), app.as_weak());
    app.on_notice(move |title, text| {
        if let Some(app) = shown.upgrade() {
            let texts = DialogTexts {
                title,
                text,
                confirm: "OK".into(),
                notice: true,
            };
            queue.confirm(&app, texts, |_, _| {});
        }
    });
    let shown = app.as_weak();
    app.on_dialog_answered(move |confirmed| {
        if let Some(app) = shown.upgrade() {
            dialogs.answer(&app, confirmed);
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::intents::tests::{Episodes, NoJobs, NoSamples};
    use crate::publisher::{install_models, SlintPublisher};
    use i_slint_backend_testing::{mock_elapsed_time, AccessibleRole, ElementHandle};
    use mkv_core::pipeline::PipelineMode;
    use mkv_core::request::{BatchRequest, EffectiveSettings};
    use mkv_desktop_core::actor::{ActorSink, Core, CoreHandle, Outcome, Ports};
    use mkv_desktop_core::control::Launcher;
    use mkv_desktop_core::draft::DraftChanges;
    use mkv_desktop_core::presentation::{controls, Hold};
    use std::time::Duration;

    fn new_app() -> App {
        let app = App::new().unwrap();
        install_models(&app);
        app
    }

    pub(crate) fn ticks() {
        for _ in 0..3 {
            mock_elapsed_time(Duration::from_millis(1));
        }
    }

    /// A key pressed and released in the window.
    pub(crate) fn key(app: &App, key: slint::platform::Key) {
        use slint::platform::WindowEvent;
        let text: slint::SharedString = key.into();
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
        ticks();
    }

    /// The one button labelled `label`.
    pub(crate) fn button(app: &App, label: &str) -> ElementHandle {
        let mut found: Vec<_> = ElementHandle::find_by_accessible_label(app, label)
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .collect();
        assert_eq!(found.len(), 1, "one {label:?} button");
        found.pop().unwrap()
    }

    #[test]
    fn the_controls_map_into_the_bar_and_the_tab_pill() {
        let mut view = ViewState::new(None).remux;
        view.count_text = "3 of 5 · 2 done · 1 failed".into();
        view.clock = "1:23 elapsed".into();
        view.progress.done = 40.0;
        view.progress.skipped = 10.0;
        view.progress.failed = 20.0;
        view.progress.live = 12.5;
        view.total_count = 5;
        view.selected_count = 5;
        let owner = Hold {
            owner: Some(PipelineMode::Remux),
            stopping: false,
            busy: true,
        };
        view.controls = controls(PipelineMode::Remux, &view, &owner);
        let bar = map_controls(&view);
        assert_eq!(bar.count_text, "3 of 5 · 2 done · 1 failed");
        assert_eq!(bar.clock, "1:23 elapsed");
        assert_eq!(
            (bar.done, bar.skipped, bar.failed, bar.live),
            (40.0, 10.0, 20.0, 12.5)
        );
        assert_eq!(bar.run_label, "Run remux · 5");
        assert!(bar.run_disabled && bar.owns && !bar.stop_disabled);
        assert_eq!(bar.stop_label, "Stop");
        assert_eq!(
            (bar.tone, bar.status.as_str()),
            (StatusTone::Running, "Running")
        );

        // Another mode's batch: Run disabled with its reason, the pill on this mode's outcome.
        let other = Hold {
            owner: Some(PipelineMode::Extract),
            ..owner
        };
        for (outcome, tone, label) in [
            (None, StatusTone::Idle, "Idle"),
            (Some(Outcome::Done), StatusTone::Success, "Done"),
            (Some(Outcome::Cancelled), StatusTone::Cancelled, "Cancelled"),
            (Some(Outcome::Failed), StatusTone::Error, "Failed"),
        ] {
            view.outcome = outcome;
            view.controls = controls(PipelineMode::Remux, &view, &other);
            let bar = map_controls(&view);
            assert_eq!((bar.tone, bar.status.as_str()), (tone, label));
            assert_eq!(bar.run_title, "Extract batch is running");
            assert!(bar.run_disabled && !bar.owns);
        }
    }

    #[test]
    fn a_second_dialog_waits_for_the_first_to_be_answered() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let dialogs = Dialogs::default();
        let answers = Rc::new(RefCell::new(Vec::new()));
        let texts = |title: &str| DialogTexts {
            title: title.into(),
            ..clear_cache_dialog()
        };
        for name in ["first", "second"] {
            let answers = Rc::clone(&answers);
            dialogs.confirm(&app, texts(name), move |_, confirmed| {
                answers.borrow_mut().push((name, confirmed));
            });
        }
        assert!(app.get_dialog_open());
        assert_eq!(app.get_dialog().title, "first");
        assert_eq!(app.get_dialog_serial(), 1);

        dialogs.answer(&app, true);
        assert_eq!(*answers.borrow(), [("first", true)]);
        assert!(app.get_dialog_open(), "the second shows next");
        assert_eq!(app.get_dialog().title, "second");
        assert_eq!(app.get_dialog_serial(), 2);

        dialogs.answer(&app, false);
        assert_eq!(*answers.borrow(), [("first", true), ("second", false)]);
        assert!(!app.get_dialog_open());
        dialogs.answer(&app, true);
        assert_eq!(answers.borrow().len(), 2, "nothing left to answer");
    }

    /// Starts and launches nothing: a start resolves forever, so it holds the batch.
    struct Held;

    impl Launcher for Held {
        async fn resolve(&self, _: BatchRequest) -> Result<EffectiveSettings, String> {
            std::future::pending().await
        }

        async fn launch(&self, _: BatchRequest, _: ActorSink) -> Result<u64, String> {
            Err("no jobs".into())
        }

        async fn stop(&self, _: Option<u64>) -> Result<(), String> {
            Ok(())
        }
    }

    /// A wired window over a core whose launcher is `launcher`, shown at the narrowest size.
    pub(crate) fn wired<L: Launcher>(
        runtime: &tokio::runtime::Runtime,
        launcher: L,
    ) -> (App, CoreHandle) {
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        let ports = Ports {
            inspector: Episodes,
            launcher,
            sampler: NoSamples,
        };
        let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
        crate::intents::wire(&app, core.clone(), runtime.handle().clone(), publisher);
        app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
        app.show().unwrap();
        ticks();
        (app, core)
    }

    pub(crate) fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Shows the core's last published state, as the hop before an answer does. The wired
    /// publisher cannot hop without an event loop: a fresh one shows it.
    fn show_published(app: &App, runtime: &tokio::runtime::Runtime, core: &CoreHandle) {
        SlintPublisher::new(app).show(&runtime.block_on(core.state()).unwrap());
    }

    /// How many lines remux's Run Log holds: one per refused Run. Asked after every Intent sent
    /// before it, so it counts all of them.
    fn log_lines(runtime: &tokio::runtime::Runtime, core: &CoreHandle) -> usize {
        runtime
            .block_on(core.log(PipelineMode::Remux))
            .unwrap()
            .lines
            .len()
    }

    /// A Run sent locks Run and what a batch locks until it is answered: a second click sends
    /// nothing. Without an event loop the answer never hops back, so the lock holds.
    #[test]
    fn a_pending_run_locks_run_and_the_settings_until_answered() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, core) = wired(&runtime, NoJobs);
        let run = button(&app, "Run remux");
        assert_eq!(run.accessible_enabled(), Some(true));

        run.invoke_accessible_default_action();
        assert!(app.get_run_pending() && app.get_locked() && !app.get_busy());
        ticks();
        assert_eq!(run.accessible_enabled(), Some(false), "Run is locked");
        // The notes that a batch runs follow `busy` alone, as html.job-locked does.
        let hint = "Selection is locked while the batch runs.";
        assert_eq!(
            ElementHandle::find_by_accessible_label(&app, hint).count(),
            0
        );
        run.invoke_accessible_default_action();
        app.invoke_run();
        assert_eq!(log_lines(&runtime, &core), 1, "one Run was sent");

        // The settings are locked too: Clear cache asks nothing.
        app.global::<Sheet>().invoke_clear_cache();
        assert!(!app.get_dialog_open());

        run_answered(&app, Mode::Remux, Err("refused".into()));
        assert!(!app.get_run_pending() && !app.get_locked());
        app.hide().unwrap();
    }

    /// A Run refused for its draft (an empty input folder) opens the sheet on the flagged field,
    /// with its ring, and sends no second Run; closed, the sheet gives the focus back to Run.
    /// A Run refused because a batch is held opens nothing, though the last refusal's flag is
    /// still published.
    #[test]
    fn a_refused_run_hands_off_to_the_flagged_field_only_for_a_draft_refusal() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, core) = wired(&runtime, Held);
        let run = |mode| runtime.block_on(core.dispatch(Intent::Run { mode }));

        app.set_run_pending(true);
        let answer = run(PipelineMode::Remux);
        show_published(&app, &runtime, &core);
        run_answered(&app, Mode::Remux, answer);
        ticks();
        assert!(app.get_sheet_open());
        assert_eq!(app.get_remux_invalid(), "input_dir", "the field is ringed");
        assert_eq!(app.global::<Sheet>().get_focused(), "input_dir");
        assert_eq!(log_lines(&runtime, &core), 1, "no second Run");

        // Closed, Run has the focus back: Return sends it again.
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_sheet_open());
        key(&app, slint::platform::Key::Return);
        assert_eq!(log_lines(&runtime, &core), 2, "Return on Run sent a Run");

        // Extract's start holds the batch: remux's Run is refused before its draft is read.
        let changes = DraftChanges {
            input_dir: Some("/show".into()),
            ..DraftChanges::default()
        };
        let edit = Intent::EditDraft {
            mode: PipelineMode::Extract,
            changes,
        };
        assert_eq!(runtime.block_on(core.dispatch(edit)), Ok(true));
        drop(core.dispatch(Intent::Run {
            mode: PipelineMode::Extract,
        }));
        app.set_run_pending(true);
        let answer = run(PipelineMode::Remux);
        assert_eq!(answer, Err(RUN_BUSY.to_string()));
        show_published(&app, &runtime, &core);
        assert!(app.get_busy());
        assert_eq!(app.get_remux_invalid(), "input_dir", "the old flag stands");
        run_answered(&app, Mode::Remux, answer);
        ticks();
        assert!(!app.get_sheet_open(), "a busy refusal opens nothing");
        assert!(!app.get_run_pending());
        app.hide().unwrap();
    }

    /// "Clear inspection cache…" asks first: Clear cache clears the mode's cache, which the core
    /// logs; Cancel and Esc send nothing. Cancel takes the focus as the dialog shows, and the
    /// sheet's button has it back once it closes.
    #[test]
    fn clear_cache_asks_first_and_sends_only_once_confirmed() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, core) = wired(&runtime, NoJobs);
        // Tall enough for the whole sheet: a search skips the controls scrolled out of view.
        app.window()
            .set_size(slint::LogicalSize::new(920.0, 1200.0));
        let ask = || {
            button(&app, "Clear inspection cache…").invoke_accessible_default_action();
            ticks();
            assert!(app.get_dialog_open());
        };
        app.invoke_open_sheet(Mode::Remux, "no_cache".into());
        ticks();

        ask();
        assert_eq!(app.get_dialog().confirm, "Clear cache");
        // Cancel has the focus: Return on it cancels.
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_dialog_open());
        assert!(app.get_sheet_open(), "the sheet stays");
        // The sheet's button has the focus back: Return asks again, Esc cancels the dialog alone.
        key(&app, slint::platform::Key::Return);
        assert!(app.get_dialog_open());
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_dialog_open());
        assert!(app.get_sheet_open(), "Esc closed the dialog, not the sheet");
        assert_eq!(log_lines(&runtime, &core), 0, "nothing was cleared");

        ask();
        button(&app, "Clear cache").invoke_accessible_default_action();
        ticks();
        assert!(!app.get_dialog_open());
        assert_eq!(log_lines(&runtime, &core), 1, "the cache line");
        app.hide().unwrap();
    }

    /// A batch that starts while the cache dialog is open disables the sheet's button: once the
    /// dialog closes, Done has the focus instead (Return closes the sheet).
    #[test]
    fn a_batch_started_under_the_cache_dialog_gives_the_focus_to_done() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.window()
            .set_size(slint::LogicalSize::new(920.0, 1200.0));
        app.invoke_open_sheet(Mode::Remux, "no_cache".into());
        ticks();
        button(&app, "Clear inspection cache…").invoke_accessible_default_action();
        ticks();
        assert!(app.get_dialog_open());

        app.set_busy(true);
        ticks();
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_dialog_open());
        assert!(app.get_sheet_open());
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_sheet_open(), "Return on Done closed the sheet");
        app.hide().unwrap();
    }

    /// A failed Close's notice waits behind a dialog shown, then shows OK alone, with the focus:
    /// Return answers it at once.
    #[test]
    fn a_failed_close_shows_an_ok_notice_queued_with_the_dialogs() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.window()
            .set_size(slint::LogicalSize::new(920.0, 1200.0));
        app.invoke_open_sheet(Mode::Remux, "no_cache".into());
        ticks();
        app.global::<Sheet>().invoke_clear_cache();
        ticks();
        app.invoke_close_failed("cleanup broke".into());
        ticks();
        assert_eq!(app.get_dialog().confirm, "Clear cache", "the notice waits");

        key(&app, slint::platform::Key::Escape);
        assert!(app.get_dialog_open());
        assert_eq!(app.get_dialog(), close_failed_dialog("cleanup broke"));
        assert!(app.get_dialog().text.contains("cleanup broke"));
        let cancels = ElementHandle::find_by_accessible_label(&app, "Cancel")
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .count();
        assert_eq!(cancels, 0, "no Cancel");
        // OK has the focus as it shows: Return answers it.
        button(&app, "OK");
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_dialog_open(), "Return on OK answered it");
        app.hide().unwrap();
    }

    /// The host's notice (no log file this session) is an OK-only dialog, queued
    /// with the others.
    #[test]
    fn a_host_notice_shows_ok_alone_behind_a_dialog_shown() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.invoke_close_failed("cleanup broke".into());
        app.invoke_notice(
            "Could not save Engine Diagnostics".into(),
            "C:\\logs".into(),
        );
        assert_eq!(app.get_dialog(), close_failed_dialog("cleanup broke"));
        app.invoke_dialog_answered(true);
        assert_eq!(
            app.get_dialog(),
            DialogTexts {
                title: "Could not save Engine Diagnostics".into(),
                text: "C:\\logs".into(),
                confirm: "OK".into(),
                notice: true,
            }
        );
        app.invoke_dialog_answered(true);
        assert!(!app.get_dialog_open());
    }

    /// A notice answered over the open sheet gives the focus to Done, not to "Clear inspection
    /// cache…": Return closes the sheet and asks nothing.
    #[test]
    fn a_notice_answered_over_the_sheet_gives_the_focus_to_done() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.invoke_open_sheet(Mode::Remux, "no_cache".into());
        ticks();
        app.invoke_close_failed("cleanup broke".into());
        ticks();
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_dialog_open(), "Return on OK answered it");
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_dialog_open(), "no cache dialog");
        assert!(!app.get_sheet_open(), "Return on Done closed the sheet");
        app.hide().unwrap();
    }

    /// A sheet closed with no opener to take the focus back (opened for no field, or by a refused
    /// Run that a scan then disabled) leaves the focus with the window: Esc still closes the Log
    /// drawer.
    #[test]
    fn a_sheet_closed_with_no_opener_leaves_esc_closing_the_drawer() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, core) = wired(&runtime, Held);
        app.set_log_open(true);
        app.set_run_pending(true);
        let answer = runtime.block_on(core.dispatch(Intent::Run {
            mode: PipelineMode::Remux,
        }));
        show_published(&app, &runtime, &core);
        run_answered(&app, Mode::Remux, answer);
        ticks();
        assert!(app.get_sheet_open());
        // A scan of the folder typed disables Run.
        app.set_remux_job(JobBar {
            run_disabled: true,
            ..app.get_remux_job()
        });
        ticks();
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_sheet_open() && app.get_log_open());
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_log_open(), "Esc closed the drawer");

        app.set_log_open(true);
        app.invoke_open_sheet(Mode::Remux, "".into());
        ticks();
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_sheet_open() && app.get_log_open());
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_log_open(), "Esc closed the drawer again");
        app.hide().unwrap();
    }

    /// A notice too long for the narrowest window elides its text: OK stays in the window.
    #[test]
    fn a_long_close_notice_keeps_ok_in_the_narrowest_window() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
        app.invoke_close_failed(
            "Job cleanup failed: /a/long/path/to/an/episode.mkv; "
                .repeat(40)
                .into(),
        );
        ticks();
        let ok = button(&app, "OK");
        let bottom = ok.absolute_position().y + ok.size().height;
        assert!(bottom <= 520.0 - 36.0, "OK ends at {bottom}");
        app.hide().unwrap();
    }

    /// A notice answered with no sheet open gives the focus back to the window, so Esc still
    /// closes the Log drawer, as the webview's document-wide Esc does.
    #[test]
    fn a_notice_answered_with_no_sheet_leaves_esc_closing_the_drawer() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.set_log_open(true);
        app.invoke_close_failed("cleanup broke".into());
        ticks();
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_dialog_open(), "Return on OK answered it");
        key(&app, slint::platform::Key::Escape);
        assert!(!app.get_log_open(), "Esc closed the drawer");
        app.hide().unwrap();
    }

    /// Run, the Log toggle and the drawer's Close take the focus from a pointer press, as a
    /// browser's buttons do: Return then activates the one clicked, and Close hands the focus to
    /// the toggle.
    #[test]
    fn the_job_buttons_take_the_focus_from_a_pointer_press() {
        use crate::ui_tests::pointer;
        use slint::platform::Key::Return;
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, core) = wired(&runtime, NoJobs);
        app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
        ticks();
        let click = |label: &str| {
            let element = button(&app, label);
            pointer(&app, &element, true);
            pointer(&app, &element, false);
            ticks();
        };
        // No event loop: the answer never hops back, so it is answered here.
        let answered = || run_answered(&app, Mode::Remux, Ok(true));

        click("Run remux");
        answered();
        key(&app, Return);
        answered();
        assert_eq!(log_lines(&runtime, &core), 2, "Return on Run sent a Run");

        app.set_log_open(true);
        ticks();
        click("Close");
        assert!(!app.get_log_open());
        key(&app, Return);
        assert!(
            app.get_log_open(),
            "the toggle had the focus: Return opened it"
        );
        app.set_log_open(false);

        click("Run remux");
        answered();
        click("Log");
        assert!(app.get_log_open());
        key(&app, Return);
        assert!(!app.get_log_open(), "Return on the toggle closed it");
        assert_eq!(log_lines(&runtime, &core), 3, "no Run from Return");
        app.hide().unwrap();
    }
}
