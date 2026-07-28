mod commands;

pub use commands::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(JobState::default())
        .invoke_handler(tauri::generate_handler![
            commands::run_job,
            commands::stop_job,
            commands::clear_cache,
            commands::pick_folder,
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

            // On Linux there is no reliable compositor blur support.
            // Disable transparency so the window is fully opaque and readable.
            #[cfg(target_os = "linux")]
            {
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    // Make window opaque — transparent: true in tauri.conf.json
                    // causes a see-through window on Linux without a compositor.
                    let _ = window.set_transparent(false);
                    // Restore native decorations so the user gets a title bar
                    // from their window manager (GTK/KDE/etc).
                    let _ = window.set_decorations(true);
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
