mod commands;
mod diagnostics;
mod process;
mod publisher;
mod telemetry;
mod window;

use mkv_desktop_core::actor::Core;
use mkv_desktop_core::control::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, InspectCacheState, ScanState,
};
use mkv_desktop_core::inspection::EngineInspector;
use mkv_desktop_core::job::JobState;
use mkv_desktop_core::telemetry::TelemetryState;
use publisher::WebviewPublisher;
use std::sync::Arc;
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
fn init_linux_env() {
    // Both the proprietary and the open NVIDIA kernel modules register as `nvidia`.
    apply_linux_env(std::path::Path::new("/sys/module/nvidia").exists());
}

#[cfg(target_os = "linux")]
fn apply_linux_env(nvidia_driver_loaded: bool) {
    // Prefer X11/XWayland: on native Wayland, WebKitWebProcess aborts with EGL_BAD_PARAMETER.
    if std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11,wayland");
    }

    // Workarounds for the WebKitGTK bundled in the AppImage (ADR-0015).
    if std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some() {
        // Disable the DMABUF renderer only on the NVIDIA driver, untested here and the most
        // reported DMABUF breaker. On the one Intel host retested, the shutdown SEGV
        // (SkiaGLContext / __eglFini) did not reproduce, and the CPU path ran at single-digit fps.
        // Widen the check if another GPU reports DMABUF crashes. Never set this on native packages
        // (RPM/DEB): with WebKitGTK 2.54+, disabling DMABUF drops Skia to CPU shared-memory
        // rendering on transparent windows, and regions outside pointer damage vanish.
        if nvidia_driver_loaded && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }

        // The bundled libwayland-client.so.0 is older than the host Mesa EGL drivers expect.
        // Preloading the host copy resolves the ABI/symbol mismatch.
        let current_preload = std::env::var("LD_PRELOAD").unwrap_or_default();
        if !current_preload.contains("libwayland-client") {
            for candidate in HOST_WAYLAND_CLIENT_CANDIDATES {
                if std::path::Path::new(candidate).exists() {
                    let new_preload = if current_preload.is_empty() {
                        candidate.to_string()
                    } else {
                        format!("{candidate}:{current_preload}")
                    };
                    std::env::set_var("LD_PRELOAD", new_preload);
                    break;
                }
            }
        }
    }
}

pub fn run() {
    #[cfg(target_os = "linux")]
    init_linux_env();

    tauri::Builder::default()
        .setup(|app| {
            // As early as the log directory is known; a failed lookup leaves stderr only.
            diagnostics::init(app.path().app_log_dir().ok().as_deref());
            // The Desktop Core actor runs on the tokio runtime Tauri runs its async commands on,
            // publishes to the webview, and inspects through the Directory Inspection registry
            // and the session cache that Stop, Close and Run share.
            let inspector = EngineInspector {
                scan: Arc::clone(&app.state::<Arc<ScanState>>()),
                cache: app.state::<InspectCacheState>().inner().clone(),
            };
            app.manage(Core::spawn(
                tauri::async_runtime::handle().inner().clone(),
                WebviewPublisher(app.handle().clone()),
                inspector,
            ));
            Ok(())
        })
        // Jobs run on the tokio runtime Tauri runs its async commands on.
        .manage(JobState::new(
            tauri::async_runtime::handle().inner().clone(),
        ))
        .manage(Arc::new(ScanState::default()))
        .manage(InspectCacheState::default())
        .manage(TelemetryState::default())
        .invoke_handler(tauri::generate_handler![
            commands::run_job,
            commands::resolve_batch_request,
            process::stop_job,
            commands::clear_cache,
            commands::pick_directory,
            commands::pick_subtitle_file,
            telemetry::get_system_telemetry,
            process::close_window,
            window::minimize_window,
            window::toggle_maximize,
            window::is_maximized,
            window::start_drag,
            window::start_resize,
            window::set_window_theme,
            commands::get_state,
            commands::dispatch,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<JobState>();
                let scan_state = app.state::<Arc<ScanState>>();
                if begin_shutdown(&state, &scan_state) {
                    api.prevent_exit();
                    let app = app.clone();
                    std::thread::spawn(move || {
                        let state = app.state::<JobState>();
                        let scan_state = app.state::<Arc<ScanState>>();
                        match cleanup_owned_processes(&state, &scan_state) {
                            Ok(()) => app.exit(0),
                            Err(error) => {
                                cancel_shutdown(&state, &scan_state);
                                let _ = app.emit(commands::CLEANUP_ERROR, error);
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

    // Every env var apply_linux_env() reads or writes.
    const TOUCHED: [&str; 5] = [
        "APPIMAGE",
        "APPDIR",
        "GDK_BACKEND",
        "LD_PRELOAD",
        "WEBKIT_DISABLE_DMABUF_RENDERER",
    ];

    // One test on purpose: apply_linux_env() mutates process-global env, and
    // cargo runs tests on parallel threads, so split cases would race.
    #[test]
    fn test_apply_linux_env_scopes_dmabuf_workaround_to_nvidia_appimage() {
        let saved: Vec<(&str, Option<OsString>)> = TOUCHED
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();

        let reset = || {
            for key in TOUCHED {
                std::env::remove_var(key);
            }
        };

        // Native packages (RPM/DEB): leave DMABUF enabled so transparent
        // windows keep hardware-accelerated damage tracking, even on NVIDIA.
        reset();
        super::apply_linux_env(true);
        let native = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER");

        // AppImage: disable DMABUF only on the NVIDIA driver. Both env markers
        // the runtime sets are exercised, so neither arm of the detection can
        // be dropped without this failing. Results are asserted only after the
        // restore below, so a failing assertion cannot leak the vars into
        // later tests.
        let mut markers: Vec<(&str, bool, Option<String>)> = Vec::new();
        for marker in ["APPIMAGE", "APPDIR"] {
            for nvidia in [false, true] {
                reset();
                std::env::set_var(marker, "1");
                super::apply_linux_env(nvidia);
                let value = std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok();
                markers.push((marker, nvidia, value));
            }
        }

        // An operator's own value always wins, even where the default would apply.
        reset();
        std::env::set_var("APPIMAGE", "1");
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "0");
        super::apply_linux_env(true);
        let overridden = std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok();

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
        for (marker, nvidia, value) in &markers {
            assert_eq!(
                value.as_deref(),
                nvidia.then_some("1"),
                "{marker} with nvidia={nvidia} must disable DMABUF only on NVIDIA"
            );
        }
        assert_eq!(
            overridden.as_deref(),
            Some("0"),
            "an exported WEBKIT_DISABLE_DMABUF_RENDERER must win"
        );
        if has_candidate {
            assert!(
                preload.is_some_and(|value| value.contains("libwayland-client")),
                "AppImage must preload the host libwayland-client"
            );
        }
    }
}
