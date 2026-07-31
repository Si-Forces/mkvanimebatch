mod commands;

pub use commands::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            commands::get_system_telemetry,
            commands::close_window,
            commands::minimize_window,
            commands::toggle_maximize,
            commands::start_drag,
            commands::start_resize,
        ])
        .setup(|_app| {
            #[cfg(target_os = "macos")]
            {
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    let _ = window_vibrancy::apply_vibrancy(&window, window_vibrancy::NSVisualEffectMaterial::HudWindow, None, None);
                }
            }

            #[cfg(target_os = "windows")]
            {
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    let _ = window_vibrancy::apply_acrylic(&window, Some((0, 0, 0, 0)));
                }
            }

            #[cfg(target_os = "linux")]
            {
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    let _ = window.set_shadow(false);
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
