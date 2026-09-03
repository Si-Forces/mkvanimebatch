mod commands;
pub mod contract;
mod process;
mod sidecar;
mod telemetry;
mod window;

pub use commands::*;
use tauri::{Emitter, Manager};

#[cfg(target_os = "linux")]
pub fn init_linux_env() {
    // Workaround for upstream WebKitGTK / Mesa EGL race condition crash on shutdown.
    // Disabling the DMABUF renderer prevents SkiaGLContext / __eglFini SEGV upon exit.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    // Workaround for WebKitWebProcess crash (EGL_BAD_PARAMETER / SIGABRT) on modern Wayland/Mesa distros (e.g. Fedora)
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }
    // Prefer X11/XWayland backend by default to prevent WebKitWebProcess EGL_BAD_PARAMETER abort on Wayland
    if std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11,wayland");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    init_linux_env();

    tauri::Builder::default()
        .manage(JobState::default())
        .manage(ScanState::default())
        .manage(TelemetryState::default())
        .invoke_handler(tauri::generate_handler![
            run_job,
            stop_job,
            clear_cache,
            pick_folder,
            inspect_directory,
            get_system_telemetry,
            close_window,
            minimize_window,
            toggle_maximize,
            is_maximized,
            start_drag,
            start_resize,
            set_window_theme,
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_theme(Some(tauri::Theme::Dark));
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<JobState>();
                let scan_state = app.state::<ScanState>();
                if begin_shutdown(&state, &scan_state) {
                    api.prevent_exit();
                    let app = app.clone();
                    std::thread::spawn(move || {
                        let state = app.state::<JobState>();
                        let scan_state = app.state::<ScanState>();
                        match cleanup_owned_processes(&state, &scan_state) {
                            Ok(()) => app.exit(0),
                            Err(error) => {
                                cancel_shutdown(&state, &scan_state);
                                let _ = app.emit("cleanup-error", error);
                            }
                        }
                    });
                }
            }
        });
}
