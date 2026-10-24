//! The queue's Intents (P2). Each window callback sends its Intent to the Desktop Core at once, on
//! the UI thread (`CoreHandle::dispatch` sends at call time), so Intents reach the core in the
//! order the user made them; only their answers are awaited, on the host's runtime.

use crate::publisher::SlintPublisher;
use mkv_anime_batch_slint_ui::{App, Mode};
use mkv_core::pipeline::PipelineMode;
use mkv_desktop_core::actor::{CoreHandle, Intent, ViewState};
use mkv_desktop_core::draft::DraftChanges;
use mkv_desktop_core::queue::Direction;
use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;

fn pipeline(mode: Mode) -> PipelineMode {
    match mode {
        Mode::Remux => PipelineMode::Remux,
        Mode::Extract => PipelineMode::Extract,
    }
}

/// The mode the window shows at boot: the extract view when a start or a job is in extract.
pub(crate) fn boot_mode(state: &ViewState) -> Mode {
    let extract = match state.run.mode {
        Some(mode) => mode == PipelineMode::Extract,
        None => state.job.as_ref().is_some_and(|job| job.mode == "extract"),
    };
    if extract {
        Mode::Extract
    } else {
        Mode::Remux
    }
}

#[derive(Clone)]
struct Intents {
    core: CoreHandle,
    runtime: Handle,
    window: slint::Weak<App>,
}

impl Intents {
    /// Sends `intent` now; awaits its answer on the runtime, logs a refusal, then runs `answered`.
    fn send(&self, name: &'static str, intent: Intent, answered: impl FnOnce() + Send + 'static) {
        let answer = self.core.dispatch(intent);
        self.runtime.spawn(async move {
            if let Err(error) = answer.await {
                tracing::warn!("[{name}] refused: {error}");
            }
            answered();
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

/// Clears the one-dialog flag when dropped, so a dialog that panics cannot leave Open folder dead.
struct Picking(Arc<AtomicBool>);

impl Drop for Picking {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Wires the window's queue callbacks to the core. Each acts on the mode the window shows.
pub(crate) fn wire(app: &App, core: CoreHandle, runtime: Handle, publisher: SlintPublisher) {
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
        let (window, picked) = (send.window.clone(), id.clone());
        let intent = |mode| Intent::Inspect {
            mode,
            id: Some(id.into()),
        };
        send.send_shown("inspect", intent, move || {
            let _ = window.upgrade_in_event_loop(move |app| {
                if app.get_pending_inspect() == picked {
                    app.set_pending_inspect("".into());
                }
            });
        });
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

    // One folder dialog at a time: it has no parent window, so it does not block a second click.
    let picking = Arc::new(AtomicBool::new(false));
    app.on_open_folder(move || {
        let Some(app) = window.upgrade() else {
            return;
        };
        if app.get_busy() || picking.swap(true, Ordering::Relaxed) {
            return;
        }
        let mode = pipeline(app.get_mode());
        let (send, picking, window) = (
            intents.clone(),
            Picking(Arc::clone(&picking)),
            window.clone(),
        );
        // The dialog blocks until it is answered: never on the event loop.
        intents.runtime.spawn_blocking(move || {
            let picked = rfd::FileDialog::new().pick_folder();
            // Back on the event loop, so the Intents keep their order with the other callbacks'.
            let _ = window.upgrade_in_event_loop(move |app| {
                drop(picking);
                // A batch may have started while the dialog was open (app.js re-checks too).
                let Some(path) = picked.filter(|_| !app.get_busy()) else {
                    return;
                };
                let path = path.to_string_lossy().into_owned();
                // The picked folder is the Settings Draft's input folder, which Run reads.
                let changes = DraftChanges {
                    input_dir: Some(path.clone()),
                    ..DraftChanges::default()
                };
                send.send("edit_draft", Intent::EditDraft { mode, changes }, || {});
                let intent = Intent::ScanDirectory {
                    mode,
                    path,
                    force: false,
                    no_cache: false,
                };
                send.send("scan_directory", intent, || {});
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_core::item_id::ItemId;
    use mkv_core::manifest::{InspectionManifest, ManifestItem};
    use mkv_core::request::{BatchRequest, EffectiveSettings};
    use mkv_desktop_core::actor::{ActorSink, Core, Ports};
    use mkv_desktop_core::control::Launcher;
    use mkv_desktop_core::inspection::Inspector;
    use mkv_desktop_core::telemetry::{Sampler, TelemetryData};

    /// Every directory holds episodes `a`, `b` and `c`.
    struct Episodes;

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
    struct NoJobs;

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

    struct NoSamples;

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

    #[test]
    fn boot_shows_the_extract_view_while_extract_holds_the_batch() {
        let mut state = ViewState::new(None);
        assert_eq!(boot_mode(&state), Mode::Remux);
        state.run.mode = Some(PipelineMode::Extract);
        assert_eq!(boot_mode(&state), Mode::Extract);
    }
}
