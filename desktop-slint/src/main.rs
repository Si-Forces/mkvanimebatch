//! MKV Anime Batch desktop host on Slint (ADR-0017), beside the Tauri host until the switch.

mod diagnostics;
mod publisher;
#[cfg(test)]
mod ui_tests;

use mkv_anime_batch_slint_ui::App;
use mkv_desktop_core::actor::{Core, Ports};
use mkv_desktop_core::control::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, InspectCacheState, ScanState,
};
use mkv_desktop_core::job::JobState;
use mkv_desktop_core::presentation::{close_failed_text, CLOSE_FAILED_TITLE};
use publisher::SlintPublisher;
use slint::winit_030::WinitWindowAccessor;
use slint::{CloseRequestResponse, ComponentHandle};
use std::error::Error;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn Error>> {
    diagnostics::init(diagnostics::log_dir().as_deref());

    // The Desktop Core actor, its scans and its jobs run here; Slint keeps the main thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let job = JobState::new(runtime.handle().clone());
    let scan = Arc::new(ScanState::default());
    let app = App::new()?;
    // Held until the end: the actor stops once every handle is gone.
    let _core = Core::spawn(
        runtime.handle().clone(),
        SlintPublisher::new(app.as_weak()),
        Ports::engine(Arc::clone(&scan), InspectCacheState::default(), job.clone()),
    );

    let close = {
        let (window, job, scan) = (app.as_weak(), job.clone(), Arc::clone(&scan));
        move || start_close(window.clone(), job.clone(), Arc::clone(&scan))
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

    app.run()?;

    // The event loop can end without a close request; tear down all the same. Idempotent after
    // a Close that already did.
    begin_shutdown(&job, &scan);
    let scan_state = Arc::clone(&scan);
    match std::thread::spawn(move || cleanup_owned_processes(&job, &scan_state)).join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("Teardown after the event loop failed: {error}"),
        Err(_) => tracing::error!("Teardown after the event loop panicked"),
    }
    // Nothing the teardown stopped is left to wait for; a stuck blocking task must not hold exit.
    runtime.shutdown_background();
    Ok(())
}

/// Close: refuse new starts at once, tear down on a thread of its own (never the UI thread or a
/// tokio worker, ADR-0017 decision 9), then quit; on failure reopen starts and say why.
fn start_close(window: slint::Weak<App>, job: JobState, scan: Arc<ScanState>) {
    if let Some(app) = window.upgrade() {
        app.set_notice_text("".into());
    }
    begin_shutdown(&job, &scan);
    std::thread::spawn(move || match cleanup_owned_processes(&job, &scan) {
        Ok(()) => {
            let _ = slint::quit_event_loop();
        }
        Err(error) => {
            cancel_shutdown(&job, &scan);
            tracing::error!("Close failed: {error}");
            let _ = window.upgrade_in_event_loop(move |app| {
                app.set_notice_title(CLOSE_FAILED_TITLE.into());
                app.set_notice_text(close_failed_text(&error).into());
            });
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
