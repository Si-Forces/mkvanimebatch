mod commands;

pub use commands::*;

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
        .setup(|_app| {
            use tauri::Manager;
            if let Some(window) = _app.get_webview_window("main") {
                let _ = window.set_theme(Some(tauri::Theme::Dark));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
