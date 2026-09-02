mod commands;
pub mod contract;
mod process;
mod sidecar;
mod telemetry;
mod window;

pub use commands::*;
use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        // Workaround for upstream WebKitGTK / Mesa EGL race condition crash on shutdown
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

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
