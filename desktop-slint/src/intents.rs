//! The queue's (P2), the Library's and the Episode Inspector's (P3) Intents. Each window callback
//! sends its Intent to the Desktop Core at once, on the UI thread (`CoreHandle::dispatch` sends at
//! call time), so Intents reach the core in the order the user made them; only their answers are
//! awaited, on the host's runtime.

use crate::inspector::{capped_tag, restore_form, typed_fields, SUBTITLE_FILTER};
use crate::publisher::SlintPublisher;
use mkv_anime_batch_slint_ui::{App, Mode};
use mkv_core::pipeline::PipelineMode;
use mkv_desktop_core::actor::{CoreHandle, Intent, ViewState};
use mkv_desktop_core::queue::Direction;
use slint::{ComponentHandle, Model, SharedString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;

pub(crate) fn pipeline(mode: Mode) -> PipelineMode {
    match mode {
        Mode::Remux => PipelineMode::Remux,
        Mode::Extract => PipelineMode::Extract,
    }
}

/// Shows the extract view at boot when a start or a job is in extract; never switches to remux, so
/// a click on Extract before the core's answer stays (app.js).
pub(crate) fn show_boot_mode(app: &App, state: &ViewState) {
    let extract = match state.run.mode {
        Some(mode) => mode == PipelineMode::Extract,
        None => state.job.as_ref().is_some_and(|job| job.mode == "extract"),
    };
    if extract {
        app.set_mode(Mode::Extract);
    }
}

#[derive(Clone)]
pub(crate) struct Intents {
    pub(crate) core: CoreHandle,
    pub(crate) runtime: Handle,
    pub(crate) window: slint::Weak<App>,
}

impl Intents {
    /// Sends `intent` now; awaits its answer on the runtime, logs a refusal, then runs `answered`.
    pub(crate) fn send(
        &self,
        name: &'static str,
        intent: Intent,
        answered: impl FnOnce() + Send + 'static,
    ) {
        self.send_answered(name, intent, |_| answered());
    }

    /// As [`send`](Self::send), with the answer.
    pub(crate) fn send_answered(
        &self,
        name: &'static str,
        intent: Intent,
        answered: impl FnOnce(Result<bool, String>) + Send + 'static,
    ) {
        let answer = self.core.dispatch(intent);
        self.runtime.spawn(async move {
            let answer = answer.await;
            if let Err(error) = &answer {
                tracing::warn!("[{name}] refused: {error}");
            }
            answered(answer);
        });
    }

    /// Sends the Intent `make` builds for the mode the window shows; nothing once it is closed.
    fn send_shown(
        &self,
        name: &'static str,
        make: impl FnOnce(PipelineMode) -> Intent,
        answered: impl FnOnce() + Send + 'static,
    ) {
        if let Some(app) = self.window.upgrade() {
            self.send(name, make(pipeline(app.get_mode())), answered);
        }
    }
}

/// `mode`'s Inspect of `picked` was answered: its list's mark is dropped, unless a later pick in
/// that list is still pending. The other mode's list keeps its own (queue-view.js keeps one per
/// list).
pub(crate) fn inspect_answered(app: &App, mode: Mode, picked: &str) {
    match mode {
        Mode::Remux if app.get_remux_pending_inspect() == picked => {
            app.set_remux_pending_inspect("".into());
        }
        Mode::Extract if app.get_extract_pending_inspect() == picked => {
            app.set_extract_pending_inspect("".into());
        }
        Mode::Remux | Mode::Extract => {}
    }
}

/// Sets the remux Inspector's row's Track Assignment to the subtitle `path` from the fields as
/// typed, or clears it for an empty `path` (app.js `assignSubtitle`); then, answered or refused,
/// puts the fields back on what is published. Nothing while a batch runs but that put-back (the
/// subtitle controls are locked by then; the file dialog may answer after a batch started).
fn assign_subtitle(send: &Intents, app: &App, path: &str) {
    let inspector = app.get_remux_inspector();
    if !inspector.shown {
        return;
    }
    if app.get_locked() {
        restore_form(app);
        return;
    }
    let (mode, id) = (PipelineMode::Remux, inspector.id.to_string());
    // The fields go with the Intent: nothing typed is left to commit.
    app.set_sub_dirty(false);
    let (name, intent) = if path.is_empty() {
        ("clear_track", Intent::ClearTrack { mode, id })
    } else {
        let (language, custom_language, title, offset) = typed_fields(app);
        let intent = Intent::AssignSubtitle {
            mode,
            id,
            path: path.into(),
            language,
            custom_language,
            title,
            offset,
        };
        ("assign_subtitle", intent)
    };
    let window = send.window.clone();
    send.send(name, intent, move || {
        let _ = window.upgrade_in_event_loop(|app| restore_form(&app));
    });
}

/// The remux Inspector's episode: the folder, as the core names it, and the id in it, so the
/// same relative id in another folder is another episode (as `inspector::write_form` compares
/// them).
fn inspected(app: &App) -> (SharedString, SharedString) {
    let inspector = app.get_remux_inspector();
    (inspector.directory, inspector.id)
}

/// The subtitle file dialog answered: `picked` is assigned to `target`, the episode the remux
/// Inspector showed when the dialog opened (see [`inspected`]). Cancelled, or the Inspector moved
/// on to another episode or folder while it was open, the select goes back on what is assigned.
fn subtitle_picked(
    send: &Intents,
    app: &App,
    target: &(SharedString, SharedString),
    picked: Option<&str>,
) {
    match picked {
        Some(path) if inspected(app) == *target => assign_subtitle(send, app, path),
        _ => restore_form(app),
    }
}

/// Sends the remux Inspector's assignment again with the fields as typed (app.js
/// `onAssignOptions`); nothing while none is assigned.
fn resend_assignment(send: &Intents, app: &App) {
    let inspector = app.get_remux_inspector();
    if inspector.assigned {
        assign_subtitle(send, app, &inspector.asset_path);
    }
}

/// Clears the one-dialog flag when dropped, so a dialog that panics cannot leave Open folder dead.
pub(crate) struct Picking(pub(crate) Arc<AtomicBool>);

impl Drop for Picking {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Wires the window's queue callbacks to the core. Each acts on the mode the window shows.
pub(crate) fn wire(app: &App, core: CoreHandle, runtime: Handle, publisher: SlintPublisher) {
    let edits = publisher.edits();
    let window = app.as_weak();
    let intents = Intents {
        core,
        runtime,
        window: window.clone(),
    };

    let send = intents.clone();
    app.on_set_selected(move |id, selected| {
        let id = id.into();
        let intent = |mode| Intent::SetSelected { mode, id, selected };
        send.send_shown("set_selected", intent, || {});
    });

    let send = intents.clone();
    app.on_set_all_selected(move |selected| {
        let intent = |mode| Intent::SetAllSelected { mode, selected };
        send.send_shown("set_all_selected", intent, || {});
    });

    let send = intents.clone();
    app.on_reorder(move |id, up| {
        let direction = if up { Direction::Up } else { Direction::Down };
        let id = id.into();
        let intent = |mode| Intent::Reorder {
            mode,
            id,
            direction,
        };
        send.send_shown("reorder", intent, || {});
    });

    // The window marks the row at once; the mark is dropped once the core answered, by then the
    // published Episode Inspector names the row, unless a later pick is still pending.
    let send = intents.clone();
    app.on_inspect(move |id| {
        let Some(app) = send.window.upgrade() else {
            return;
        };
        let (window, mode, picked) = (send.window.clone(), app.get_mode(), id.clone());
        let intent = Intent::Inspect {
            mode: pipeline(mode),
            id: Some(id.into()),
        };
        send.send("inspect", intent, move || {
            let _ = window.upgrade_in_event_loop(move |app| inspect_answered(&app, mode, &picked));
        });
    });

    // The Library: the input folder scrolls its list to the top; a release shows every row again
    // and scrolls to the release's first (queue-view.js `revealRelease`).
    let (runtime, shown, revealing) = (intents.runtime.clone(), window.clone(), publisher.clone());
    app.on_reveal_release(move |release, root| {
        let Some(app) = shown.upgrade() else {
            return;
        };
        let mode = app.get_mode();
        if root {
            revealing.cancel_reveal(mode);
            app.invoke_reveal(mode, 0);
            return;
        }
        revealing.reveal(mode, release.into());
        let publisher = revealing.clone();
        runtime.spawn_blocking(move || publisher.remap(mode));
    });

    // The subtitle select (app.js `onAssignSelect`): a published choice is assigned, Automatic
    // clears it, "Choose a file…" opens the subtitle file dialog, one at a time.
    let (send, shown) = (intents.clone(), window.clone());
    let picking_subtitle = Arc::new(AtomicBool::new(false));
    app.on_choose_subtitle(move |index| {
        let Some(app) = shown.upgrade() else {
            return;
        };
        let value = usize::try_from(index)
            .ok()
            .and_then(|at| app.get_sub_values().row_data(at));
        if let Some(value) = value {
            // Picked again: a select sends no change for the value it holds.
            if index != app.get_remux_inspector().choice {
                assign_subtitle(&send, &app, &value);
            }
            return;
        }
        if app.get_locked() || picking_subtitle.swap(true, Ordering::Relaxed) {
            restore_form(&app);
            return;
        }
        let (send, picking, window, target) = (
            send.clone(),
            Picking(Arc::clone(&picking_subtitle)),
            shown.clone(),
            inspected(&app),
        );
        // The dialog blocks until it is answered: never on the event loop.
        send.runtime.clone().spawn_blocking(move || {
            let (name, extensions) = SUBTITLE_FILTER;
            let picked = rfd::FileDialog::new()
                .add_filter(name, &extensions)
                .pick_file();
            // Back on the event loop, so the Intents keep their order with the other callbacks'.
            let _ = window.upgrade_in_event_loop(move |app| {
                drop(picking);
                let picked = picked.map(|path| path.to_string_lossy().into_owned());
                subtitle_picked(&send, &app, &target, picked.as_deref());
            });
        });
    });

    let (send, shown) = (intents.clone(), window.clone());
    app.on_reset_subtitle(move || {
        if let Some(app) = shown.upgrade() {
            assign_subtitle(&send, &app, "");
        }
    });

    // A tag, title or offset committed: the assignment again, with the new fields.
    let (send, shown) = (intents.clone(), window.clone());
    app.on_subtitle_options_changed(move || {
        if let Some(app) = shown.upgrade() {
            resend_assignment(&send, &app);
        }
    });

    // A language picked: the same, unless it is the one shown (a select sends no change for the
    // value it holds).
    let (send, shown) = (intents.clone(), window.clone());
    app.on_choose_language(move |index| {
        if let Some(app) = shown.upgrade() {
            if index != app.get_remux_inspector().language {
                resend_assignment(&send, &app);
            }
        }
    });

    // The tag field holds an ISO 639-2 code at most, as the webview's `maxlength` keeps it.
    let shown = window.clone();
    app.on_language_tag_edited(move |text| {
        if let (Some(app), Some(capped)) = (shown.upgrade(), capped_tag(&text)) {
            app.set_sub_custom(capped.into());
        }
    });

    let (runtime, shown) = (intents.runtime.clone(), window.clone());
    app.on_set_filter(move |filter| {
        if let Some(mode) = shown.upgrade().map(|app| app.get_mode()) {
            // Kept now, so the last click wins; the rows are mapped again off the event loop, as
            // every publication's are, with whatever filter is newest by then.
            publisher.set_filter(mode, filter);
            let publisher = publisher.clone();
            runtime.spawn_blocking(move || publisher.remap(mode));
        }
    });

    // Run, Stop and the cache clear; the settings sheet, the Preset Profiles and Open folder.
    crate::jobs::wire(app, intents.clone());
    crate::settings::wire(app, intents, edits);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mkv_anime_batch_slint_ui::Inspector as InspectorPane;
    use mkv_core::item_id::ItemId;
    use mkv_core::manifest::{InspectionManifest, ManifestItem};
    use mkv_core::request::{BatchRequest, EffectiveSettings};
    use mkv_desktop_core::actor::{ActorSink, Core, Ports};
    use mkv_desktop_core::control::Launcher;
    use mkv_desktop_core::inspection::Inspector;
    use mkv_desktop_core::telemetry::{Sampler, TelemetryData};

    /// Every directory holds episodes `a`, `b` and `c`.
    pub(crate) struct Episodes;

    impl Inspector for Episodes {
        async fn identity(&self, path: String) -> Result<String, String> {
            Ok(path)
        }

        async fn inspect(&self, _: String, _: bool) -> Result<InspectionManifest, String> {
            let item = |id: &str| ManifestItem {
                id: ItemId::parse(id),
                name: id.into(),
                release: ".".into(),
                metadata: Default::default(),
                inspection_error: None,
                extra: Default::default(),
            };
            Ok(InspectionManifest {
                version: 2,
                subtitle_candidates: Default::default(),
                items: ["a", "b", "c"].map(item).into(),
                extra: Default::default(),
            })
        }

        async fn clear_cache(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// Runs nothing; the test never asks it to.
    pub(crate) struct NoJobs;

    impl Launcher for NoJobs {
        async fn resolve(&self, _: BatchRequest) -> Result<EffectiveSettings, String> {
            Err("no jobs".into())
        }

        async fn launch(&self, _: BatchRequest, _: ActorSink) -> Result<u64, String> {
            Err("no jobs".into())
        }

        async fn stop(&self, _: Option<u64>) -> Result<(), String> {
            Ok(())
        }
    }

    pub(crate) struct NoSamples;

    impl Sampler for NoSamples {
        async fn sample(&self) -> Result<TelemetryData, String> {
            std::future::pending().await
        }
    }

    /// Three callbacks fired in a row reach the core in that order: deselect `a`, select all,
    /// deselect `b` leaves only `b` out, and every other order leaves a different selection.
    #[test]
    fn callbacks_reach_the_core_in_call_order() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app = App::new().unwrap();
        let publisher = SlintPublisher::new(&app);
        let ports = Ports {
            inspector: Episodes,
            launcher: NoJobs,
            sampler: NoSamples,
        };
        let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
        wire(&app, core.clone(), runtime.handle().clone(), publisher);
        let scan = Intent::ScanDirectory {
            mode: PipelineMode::Remux,
            path: "/show".into(),
            force: false,
            no_cache: false,
        };
        assert_eq!(runtime.block_on(core.dispatch(scan)), Ok(true));

        app.invoke_set_selected("a".into(), false);
        app.invoke_set_all_selected(true);
        app.invoke_set_selected("b".into(), false);

        // Asked after the three, so it sees all of them.
        let state = runtime.block_on(core.state()).unwrap();
        let selected: Vec<_> = state
            .remux
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.selected))
            .collect();
        assert_eq!(selected, [("a", true), ("b", false), ("c", true)]);
    }

    /// The subtitle callbacks send the remux Inspector's row's AssignSubtitle with the fields as
    /// typed, and ClearTrack; a choice or a language picked again, or the file dialog while a
    /// batch runs, sends nothing.
    #[test]
    fn subtitle_callbacks_send_the_fields_as_typed() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app = App::new().unwrap();
        crate::publisher::install_models(&app);
        let publisher = SlintPublisher::new(&app);
        let ports = Ports {
            inspector: Episodes,
            launcher: NoJobs,
            sampler: NoSamples,
        };
        let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
        wire(&app, core.clone(), runtime.handle().clone(), publisher);
        let scan = Intent::ScanDirectory {
            mode: PipelineMode::Remux,
            path: "/show".into(),
            force: false,
            no_cache: false,
        };
        assert_eq!(runtime.block_on(core.dispatch(scan)), Ok(true));
        let assignment = || {
            let state = runtime.block_on(core.state()).unwrap();
            state.remux.assignments.get("b").map(|assignment| {
                (
                    assignment.asset_path.to_string_lossy().into_owned(),
                    assignment.language.clone(),
                    assignment.title.clone(),
                    assignment.offset_ms,
                )
            })
        };
        // What the window shows: row `b` inspected, Automatic, with a second choice.
        let values = ["", "/subs/b.ass"].map(slint::SharedString::from);
        app.set_sub_values(slint::ModelRc::new(slint::VecModel::from(values.to_vec())));
        app.set_remux_inspector(InspectorPane {
            shown: true,
            id: "b".into(),
            ..InspectorPane::default()
        });
        app.set_sub_language(3);
        app.set_sub_custom(" may ".into());
        app.set_sub_title("A".into());
        app.set_sub_offset("1e3".into());

        // A choice picked: assigned with the fields as typed, which leaves nothing to commit.
        app.set_sub_dirty(true);
        app.invoke_choose_subtitle(1);
        assert!(!app.get_sub_dirty());
        let typed = |title: &str| {
            Some((
                "/subs/b.ass".to_string(),
                Some("may".to_string()),
                Some(title.to_string()),
                Some(1000),
            ))
        };
        assert_eq!(assignment(), typed("A"));

        // The Inspector now shows the assignment, with the custom tag.
        app.set_remux_inspector(InspectorPane {
            shown: true,
            id: "b".into(),
            assigned: true,
            asset_path: "/subs/b.ass".into(),
            choice: 1,
            language: 3,
            ..InspectorPane::default()
        });
        app.set_sub_title("Z".into());
        // The choice and the language shown, picked again: nothing is sent.
        app.invoke_choose_subtitle(1);
        app.invoke_choose_language(3);
        assert_eq!(assignment(), typed("A"));
        // A committed title sends the assignment again.
        app.set_sub_title("B".into());
        app.invoke_subtitle_options_changed();
        assert_eq!(assignment(), typed("B"));
        // Each preset language is sent as its code.
        for (index, code) in [(0, "ind"), (1, "eng"), (2, "jpn")] {
            app.set_sub_language(index);
            app.invoke_choose_language(index);
            let language = assignment().and_then(|(_, language, _, _)| language);
            assert_eq!(language.as_deref(), Some(code));
        }

        // The tag field is cut to an ISO 639-2 code.
        app.invoke_language_tag_edited("mayxx".into());
        assert_eq!(app.get_sub_custom(), "may");

        app.invoke_reset_subtitle();
        assert_eq!(assignment(), None);

        // A file picked once the Inspector moved on to another row, or to the same relative id in
        // another folder, is not that row's.
        let send = Intents {
            core: core.clone(),
            runtime: runtime.handle().clone(),
            window: app.as_weak(),
        };
        let (directory, _) = inspected(&app);
        subtitle_picked(&send, &app, &(directory, "a".into()), Some("/subs/b.ass"));
        assert_eq!(assignment(), None);
        let other_folder = ("/other".into(), "b".into());
        subtitle_picked(&send, &app, &other_folder, Some("/subs/b.ass"));
        assert_eq!(assignment(), None);
        subtitle_picked(&send, &app, &inspected(&app), Some("/subs/b.ass"));
        let path = assignment().map(|(path, ..)| path);
        assert_eq!(path.as_deref(), Some("/subs/b.ass"));

        // Locked while a batch runs: neither Automatic nor a committed title is sent, and the
        // fields go back on what is shown.
        app.set_busy(true);
        let before = assignment();
        app.set_sub_title("C".into());
        app.invoke_choose_subtitle(0);
        app.invoke_subtitle_options_changed();
        assert_eq!(assignment(), before);
        assert_eq!(app.get_sub_title(), "", "put back on what is shown");

        // "Choose a file…" while a batch runs: no dialog, the select goes back on what is shown.
        app.set_sub_choice(2);
        app.invoke_choose_subtitle(2);
        assert_eq!(app.get_sub_choice(), 1);
    }

    #[test]
    fn boot_shows_the_extract_view_while_extract_holds_the_batch() {
        i_slint_backend_testing::init_no_event_loop();
        let app = App::new().unwrap();
        let mut state = ViewState::new(None);
        show_boot_mode(&app, &state);
        assert_eq!(app.get_mode(), Mode::Remux);
        // A click on Extract before the answer stays.
        app.set_mode(Mode::Extract);
        show_boot_mode(&app, &state);
        assert_eq!(app.get_mode(), Mode::Extract);
        app.set_mode(Mode::Remux);
        state.run.mode = Some(PipelineMode::Extract);
        show_boot_mode(&app, &state);
        assert_eq!(app.get_mode(), Mode::Extract);
    }
}
