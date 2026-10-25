//! MKV Anime Batch desktop host on Slint (ADR-0017), beside the Tauri host until the switch.

mod diagnostics;
mod inspector;
mod intents;
mod jobs;
mod publisher;
mod queue;
mod renderer;
mod run_log;
mod settings;
#[cfg(test)]
mod ui_tests;

use mkv_anime_batch_slint_ui::{App, Theme};
use mkv_desktop_core::actor::{Core, Ports};
use mkv_desktop_core::control::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, InspectCacheState, ScanState,
};
use mkv_desktop_core::job::JobState;
use mkv_desktop_core::presentation::{close_failed_text, CLOSE_FAILED_TITLE};
use publisher::SlintPublisher;
use renderer::{Answer, Choice, GpuStart, Renderer};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{CloseRequestResponse, ComponentHandle, RenderingState, Timer};
use std::cell::Cell;
use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn Error>> {
    diagnostics::init(diagnostics::log_dir().as_deref());

    let config = renderer::config_dir();
    let forced = renderer::forced_from_env(std::env::var_os(renderer::RENDERER_ENV).as_deref());
    let relaunched = std::env::var_os(renderer::RELAUNCHED_ENV).is_some();
    let choice = renderer::choose(
        forced,
        config.as_deref().is_some_and(renderer::saved_software),
    );
    // Both names pinned, so neither SLINT_BACKEND nor Slint's or Skia's renderer chains pick
    // another renderer behind the host's back (renderer.rs).
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(choice.renderer.slint_name().into())
        .select()
        .inspect_err(|error| {
            tracing::error!(
                "Cannot select the {} renderer: {error}",
                choice.renderer.slint_name()
            )
        })?;
    if choice.renderer == Renderer::Software {
        let why = match (relaunched, forced) {
            (true, _) => "restarted after a GPU failure".to_string(),
            (false, Some(_)) => format!("{} is software", renderer::RENDERER_ENV),
            (false, None) => format!(
                "saved after a GPU failure; {}=opengl tries the GPU again",
                renderer::RENDERER_ENV
            ),
        };
        tracing::warn!("Running on the software renderer ({why}); the glass is faked");
    }

    // A re-launched child saves the software choice once it has drawn (save_once_drawn).
    let save = match (std::env::var_os(renderer::SAVE_ENV), &config) {
        (Some(_), Some(dir)) if choice.renderer == Renderer::Software => Some(dir.clone()),
        (Some(_), None) => {
            tracing::warn!("No config directory: the software renderer choice is not saved");
            None
        }
        _ => None,
    };
    let start = Rc::new(Cell::new(GpuStart::Setup));
    let result = run(&choice, config.clone(), save, Rc::clone(&start));
    if renderer::gpu_failed_at_start(choice.renderer, start.get(), result.is_err()) {
        let error = match result {
            Err(error) => error.to_string(),
            Ok(()) => "no window was created".to_string(),
        };
        if renderer::may_offer_restart(choice.renderer, relaunched) {
            return gpu_failed(&error);
        }
        tracing::error!("The GPU renderer could not start: {error}");
        return Err(error.into());
    }
    result.inspect_err(|error| tracing::error!("The app stopped with an error: {error}"))
}

/// OpenGL failed before the first frame: ask, then on yes re-launch in software mode; the child
/// saves the choice once it has drawn, so a restart that fails too saves nothing. No, or no dialog
/// at all, exits with the error: there is no silent re-launch.
fn gpu_failed(error: &str) -> Result<(), Box<dyn Error>> {
    tracing::error!("The GPU renderer could not start: {error}");
    match renderer::ask_restart() {
        Answer::No => return Err(format!("The GPU renderer could not start: {error}").into()),
        Answer::Unavailable(reason) => {
            tracing::error!("No dialog could be shown to offer software mode: {reason}");
            return Err(format!("The GPU renderer could not start: {error}").into());
        }
        Answer::Yes => {}
    }
    std::env::current_exe()
        .and_then(|exe| {
            renderer::relaunch_command(
                &exe,
                std::env::var_os("APPIMAGE"),
                std::env::var_os("APPDIR"),
                std::env::var_os("ARGV0"),
                std::env::args_os().skip(1),
            )
            .spawn()
        })
        .map(drop)
        .map_err(|error| {
            tracing::error!("Cannot restart in software mode: {error}");
            error.into()
        })
}

/// Builds the window and the Desktop Core, runs the event loop, then tears down. On OpenGL,
/// `start` says whether a frame was drawn or the window never came up (see [`watch_start`]).
/// `save` names the directory to save the software choice in once a frame is drawn.
fn run(
    choice: &Choice,
    config: Option<PathBuf>,
    save: Option<PathBuf>,
    start: Rc<Cell<GpuStart>>,
) -> Result<(), Box<dyn Error>> {
    // The Desktop Core actor, its scans and its jobs run here; Slint keeps the main thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let job = JobState::new(runtime.handle().clone());
    let scan = Arc::new(ScanState::default());
    let app = App::new()?;
    app.global::<Theme>()
        .set_glass(choice.renderer == Renderer::OpenGl);
    if choice.renderer == Renderer::OpenGl {
        watch_start(
            &app,
            config.filter(|_| choice.forget_saved),
            Rc::clone(&start),
        );
    }
    if let Some(dir) = save {
        save_once_drawn(&app, dir);
    }
    publisher::install_models(&app);
    // Held until the end: the actor stops once every handle is gone.
    let publisher = SlintPublisher::new(&app);
    let core = Core::spawn(
        runtime.handle().clone(),
        publisher.clone(),
        Ports::engine(Arc::clone(&scan), InspectCacheState::default(), job.clone()),
    );
    run_log::wire(&app, &publisher, run_log::system_clipboard());
    runtime.spawn(run_log::load(&core, &publisher));
    intents::wire(&app, core.clone(), runtime.handle().clone(), publisher);
    let (window, boot) = (app.as_weak(), core.state());
    slint::spawn_local(async move {
        if let (Ok(state), Some(app)) = (boot.await, window.upgrade()) {
            app.set_mode(intents::boot_mode(&state));
        }
    })?;

    let close = {
        let (window, job, scan) = (app.as_weak(), job.clone(), Arc::clone(&scan));
        let closing = Arc::new(AtomicBool::new(false));
        move || {
            start_close(
                window.clone(),
                job.clone(),
                Arc::clone(&scan),
                Arc::clone(&closing),
            )
        }
    };
    app.on_close_window(close.clone());
    app.window().on_close_requested(move || {
        close();
        CloseRequestResponse::KeepWindowShown
    });
    let window = app.as_weak();
    app.on_minimize_window(move || {
        if let Some(app) = window.upgrade() {
            minimize(&app);
        }
    });
    let window = app.as_weak();
    slint::spawn_local(async move {
        if let Some(app) = window.upgrade() {
            centre(&app).await;
        }
    })?;

    // Kept for the end: a failed event loop still tears down first. From here an error before
    // the first frame is the GPU's, unless the first frame cannot be watched.
    if start.get() == GpuStart::Setup {
        start.set(GpuStart::Pending);
    }
    let result = app.run();

    // The event loop can end without a close request, or fail; tear down all the same. Idempotent
    // after a Close that already did.
    begin_shutdown(&job, &scan);
    let scan_state = Arc::clone(&scan);
    match std::thread::spawn(move || cleanup_owned_processes(&job, &scan_state)).join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("Teardown after the event loop failed: {error}"),
        Err(_) => tracing::error!("Teardown after the event loop panicked"),
    }
    // Nothing the teardown stopped is left to wait for; a stuck blocking task must not hold exit.
    runtime.shutdown_background();
    result.map_err(Into::into)
}

/// How long the OpenGL window may take to appear, counted from the event loop's first pass. Skia
/// creates its OpenGL surface on the event loop thread, in the same step that creates the window,
/// once Slint's wait of at most 500 ms for the desktop appearance is over; a slow driver delays
/// this timer as much as the window.
const WINDOW_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Watches an OpenGL start. A failed OpenGL surface ends the event loop with an error when
/// Slint creates the window as it resumes, but when it creates it later (after the appearance
/// wait) Slint keeps the error and idles with no window (ADR-0017, renderer fallback amendment).
/// So: the first drawn frame marks `Drew` (and forgets a saved software choice when
/// `forget` names its directory); no window by [`WINDOW_DEADLINE`] marks `NoWindow` and quits
/// the loop, which then returns the error Slint kept.
fn watch_start(app: &App, forget: Option<PathBuf>, start: Rc<Cell<GpuStart>>) {
    let drawn = Rc::clone(&start);
    let forgetting = forget.is_some();
    // Skia answers the notifier; only the software renderer would refuse it.
    let notified = app.window().set_rendering_notifier(move |state, _| {
        if matches!(state, RenderingState::AfterRendering) && drawn.get() == GpuStart::Pending {
            drawn.set(GpuStart::Drew);
            if let Some(dir) = &forget {
                match renderer::forget_saved(dir) {
                    Ok(()) => {
                        tracing::info!("OpenGL drew: the software renderer choice is forgotten")
                    }
                    Err(error) => {
                        tracing::warn!("Cannot forget the software renderer choice: {error}")
                    }
                }
            }
        }
    });
    if let Err(error) = notified {
        // Unwatched, no error counts as the GPU's: no fallback rather than a false one.
        tracing::warn!("Cannot watch for the first frame, so no GPU fallback: {error:?}");
        if forgetting {
            tracing::warn!(
                "The saved software renderer choice is kept: no frame can confirm the GPU"
            );
        }
        start.set(GpuStart::Unwatched);
    }
    let window = app.as_weak();
    // A timer counts from when it is armed, so arm the deadline on the loop's first pass: time
    // spent before the loop (a loaded machine, a slow Wayland connect) must not count.
    Timer::single_shot(std::time::Duration::ZERO, move || {
        Timer::single_shot(WINDOW_DEADLINE, move || {
            let created = window
                .upgrade()
                .is_some_and(|app| app.window().with_winit_window(|_| ()).is_some());
            if !created && start.get() == GpuStart::Pending {
                start.set(GpuStart::NoWindow);
                let _ = slint::quit_event_loop();
            }
        });
    });
}

/// Saves the software choice once the re-launched child has drawn. Slint's software renderer
/// refuses the rendering notifier, so this watches winit's first `RedrawRequested` and saves on a
/// later loop pass, after Slint has drawn: a failed draw ends the loop first, and nothing is saved.
/// A first redraw that drew nothing (the window occluded at start) still saves; the software
/// renderer it saves is the one that does not fail to start.
fn save_once_drawn(app: &App, dir: PathBuf) {
    let mut pending = Some(dir);
    app.window().on_winit_window_event(move |_, event| {
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Some(dir) = pending.take() {
                Timer::single_shot(
                    std::time::Duration::ZERO,
                    move || match renderer::save_software(&dir) {
                        Ok(()) => {
                            tracing::info!("Software drew: the software renderer choice is saved")
                        }
                        Err(error) => tracing::warn!(
                            "Cannot save the software renderer choice in {}: {error}",
                            dir.display()
                        ),
                    },
                );
            }
        }
        EventResult::Propagate
    });
}

/// Close: refuse new starts at once, tear down on a thread of its own (never the UI thread or a
/// tokio worker, ADR-0017 decision 9), then quit; on failure reopen starts and say why. The
/// window stays up during the teardown, so a second Close or Alt+F4 meanwhile is ignored rather
/// than racing a second teardown; `closing` clears only when the teardown failed.
fn start_close(
    window: slint::Weak<App>,
    job: JobState,
    scan: Arc<ScanState>,
    closing: Arc<AtomicBool>,
) {
    if closing.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(app) = window.upgrade() {
        app.set_notice_text("".into());
    }
    begin_shutdown(&job, &scan);
    std::thread::spawn(move || {
        // A panicking teardown takes the failure path too: left set, `closing` would refuse every
        // later Close and the window could not be closed at all.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_owned_processes(&job, &scan)
        }))
        .unwrap_or_else(|_| Err("the teardown panicked".to_string()));
        match result {
            Ok(()) => {
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                cancel_shutdown(&job, &scan);
                closing.store(false, Ordering::SeqCst);
                tracing::error!("Close failed: {error}");
                let _ = window.upgrade_in_event_loop(move |app| {
                    app.set_notice_title(CLOSE_FAILED_TITLE.into());
                    app.set_notice_text(close_failed_text(&error).into());
                });
            }
        }
    });
}

/// Minimizes through winit. Slint caches `minimized` and only calls winit when it changes, but on
/// Wayland winit cannot report the minimized state, so after a restore the cache stays set and
/// every later minimize is dropped (ADR-0017 decision 4).
fn minimize(app: &App) {
    app.window()
        .with_winit_window(|window| window.set_minimized(true));
}

/// Centres the window on its monitor once winit has created it (`center` in tauri.conf.json).
/// Wayland lets no client place its window, so there the compositor decides.
async fn centre(app: &App) {
    let Ok(window) = app.window().winit_window().await else {
        return;
    };
    if let Some(monitor) = window.current_monitor() {
        let (area, size) = (monitor.size(), window.outer_size());
        let origin = monitor.position();
        let offset =
            |area: u32, size: u32| i32::try_from(area.saturating_sub(size) / 2).unwrap_or(0);
        window.set_outer_position(slint::winit_030::winit::dpi::PhysicalPosition::new(
            origin.x + offset(area.width, size.width),
            origin.y + offset(area.height, size.height),
        ));
    }
}
