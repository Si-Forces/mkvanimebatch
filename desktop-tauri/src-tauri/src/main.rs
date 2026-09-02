// Prevents additional console window on Windows, DO NOT REMOVE!!
#![windows_subsystem = "windows"]

fn main() {
    #[cfg(target_os = "linux")]
    {
        // Workaround for upstream WebKitGTK / Mesa EGL race condition crash on shutdown.
        // Disabling the DMABUF renderer prevents SkiaGLContext / __eglFini SEGV upon exit.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        // Workaround for WebKitWebProcess crash (EGL_BAD_PARAMETER / SIGABRT) on modern Wayland/Mesa distros (e.g. Fedora)
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    mkv_anime_batch_lib::run()
}
