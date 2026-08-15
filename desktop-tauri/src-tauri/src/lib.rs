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
            commands::run_job,
            commands::stop_job,
            commands::clear_cache,
            commands::pick_folder,
            commands::scan_directory,
            commands::inspect_mkv_file,
            commands::inspect_directory,
            commands::get_system_telemetry,
            commands::close_window,
            commands::minimize_window,
            commands::toggle_maximize,
            commands::is_maximized,
            commands::start_drag,
            commands::start_resize,
            commands::set_window_theme,
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
