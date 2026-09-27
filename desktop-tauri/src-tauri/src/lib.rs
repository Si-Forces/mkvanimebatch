mod commands;
pub mod contract;
mod process;
mod telemetry;
mod window;

pub use commands::*;
use tauri::{Emitter, Manager};

/// Host `libwayland-client.so.0` locations to preload inside an AppImage.
#[cfg(target_os = "linux")]
const HOST_WAYLAND_CLIENT_CANDIDATES: &[&str] = &[
    "/usr/lib64/libwayland-client.so.0",
    "/lib64/libwayland-client.so.0",
    "/usr/lib/x86_64-linux-gnu/libwayland-client.so.0",
    "/usr/lib/libwayland-client.so.0",
    "/usr/lib/libwayland-client.so",
    "/lib64/libwayland-client.so",
];

#[cfg(target_os = "linux")]
pub fn init_linux_env() {
    // Prefer X11/XWayland backend by default to prevent WebKitWebProcess EGL_BAD_PARAMETER abort on Wayland
    if std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11,wayland");
    }

    // Fixes for bundled WebKitGTK inside AppImage on modern Linux (Fedora/Arch/Ubuntu 24+):
    if std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some() {
        // Workaround for an upstream WebKitGTK / Mesa EGL race condition crash on shutdown in
        // AppImage. Disabling the DMABUF renderer prevents SkiaGLContext / __eglFini SEGV upon exit.
        // Note: Do NOT set this on native packages (RPM/DEB) with WebKitGTK 2.54+, where disabling
        // DMABUF drops Skia to CPU shared-memory rendering on transparent windows and causes
        // regions outside pointer damage to vanish.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }

        // AppImage bundles an older libwayland-client.so.0 that conflicts with host Mesa EGL drivers.
        // Preloading the host system's native libwayland-client.so.0 resolves the ABI/symbol mismatch.
        let current_preload = std::env::var("LD_PRELOAD").unwrap_or_default();
        if !current_preload.contains("libwayland-client") {
            for candidate in HOST_WAYLAND_CLIENT_CANDIDATES {
                if std::path::Path::new(candidate).exists() {
                    let new_preload = if current_preload.is_empty() {
                        candidate.to_string()
                    } else {
                        format!("{}:{}", candidate, current_preload)
                    };
                    std::env::set_var("LD_PRELOAD", new_preload);
                    break;
                }
            }
        }
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
            pick_directory,
            pick_subtitle_file,
            inspect_directory,
            get_system_telemetry,
            close_window,
            minimize_window,
            toggle_maximize,
            is_maximized,
            start_drag,
            start_resize,
            set_window_theme,
            get_preset_profiles,
            get_job_snapshot,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::ffi::OsString;

    // Every env var init_linux_env() reads or writes.
    const TOUCHED: [&str; 5] = [
        "APPIMAGE",
        "APPDIR",
        "GDK_BACKEND",
        "LD_PRELOAD",
        "WEBKIT_DISABLE_DMABUF_RENDERER",
    ];

    // This lives in one test on purpose: init_linux_env() mutates
    // process-global env while cargo runs tests on parallel threads, so
    // splitting the cases into separate tests made them clobber each other's
    // APPIMAGE/var state and fail at random.
    #[test]
    fn test_init_linux_env_scopes_dmabuf_workaround_to_appimage() {
        let saved: Vec<(&str, Option<OsString>)> = TOUCHED
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();

        // Reset to a known-clean slate before each case.
        let reset = || {
            for key in TOUCHED {
                std::env::remove_var(key);
            }
        };

        // Native packages (RPM/DEB): leave DMABUF enabled so transparent
        // windows keep hardware-accelerated damage tracking.
        reset();
        super::init_linux_env();
        let native = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER");

        // AppImage: keep the shutdown-SEGV workaround. Both env markers the
        // runtime sets are exercised, so neither arm of the detection can be
        // dropped without this failing.
        for marker in ["APPIMAGE", "APPDIR"] {
            reset();
            std::env::set_var(marker, "1");
            super::init_linux_env();
            assert_eq!(
                std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER")
                    .ok()
                    .as_deref(),
                Some("1"),
                "{marker} must keep DMABUF disabled"
            );
        }

        // The libwayland-client preload must survive alongside the DMABUF
        // workaround; only assert when this host actually has a candidate.
        let has_candidate = super::HOST_WAYLAND_CLIENT_CANDIDATES
            .iter()
            .any(|path| std::path::Path::new(path).exists());
        let preload = std::env::var("LD_PRELOAD").ok();

        for (key, value) in &saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }

        assert_eq!(native, None, "native packages must keep DMABUF enabled");
        if has_candidate {
            assert!(
                preload.is_some_and(|value| value.contains("libwayland-client")),
                "AppImage must preload the host libwayland-client"
            );
        }
    }
}
