use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[allow(dead_code)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ─── Sidecar Path Resolution ─────────────────────────────────────────────────

fn target_triple() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return "x86_64-pc-windows-msvc";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "x86_64-unknown-linux-gnu";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "aarch64-unknown-linux-gnu";
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    return "";
}

fn sidecar_cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_sidecar(name: &str, path: PathBuf) -> Result<PathBuf, String> {
    if let Ok(mut cache) = sidecar_cache().lock() {
        cache.insert(name.to_string(), path.clone());
    }
    Ok(path)
}

pub(crate) fn get_sidecar_path(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let triple = target_triple();

    if let Ok(cache) = sidecar_cache().lock() {
        if let Some(cached) = cache.get(name) {
            return Ok(cached.clone());
        }
    }

    let mut candidate_filenames = Vec::new();
    if !triple.is_empty() {
        #[cfg(windows)]
        candidate_filenames.push(format!("{}-{}.exe", name, triple));
        #[cfg(not(windows))]
        candidate_filenames.push(format!("{}-{}", name, triple));
    }
    #[cfg(windows)]
    candidate_filenames.push(format!("{}.exe", name));
    #[cfg(not(windows))]
    candidate_filenames.push(name.to_string());

    let mut candidate_dirs = Vec::new();

    if let Ok(resource_dir) = app.path().resource_dir() {
        candidate_dirs.push(resource_dir.clone());
        candidate_dirs.push(resource_dir.join("binaries"));
        candidate_dirs.push(resource_dir.join("bin"));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidate_dirs.push(exe_dir.to_path_buf());
            candidate_dirs.push(exe_dir.join("binaries"));
            candidate_dirs.push(exe_dir.join("bin"));
        }
    }

    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        candidate_dirs.push(PathBuf::from(manifest).join("binaries"));
    }

    // 1. Search disk for existing sidecar binary
    for dir in &candidate_dirs {
        for filename in &candidate_filenames {
            let candidate = dir.join(filename);
            if candidate.is_file() {
                return cache_sidecar(name, candidate);
            }
        }
    }

    // 2. Embedded sidecar fallback (extract from binary payload to app local data directory)
    let embedded_bytes: Option<&[u8]> = get_embedded_bytes(name);

    if let Some(bytes) = embedded_bytes {
        let sidecar_dir = app
            .path()
            .app_local_data_dir()
            .map_err(|e| format!("Failed to resolve app local data directory: {}", e))?
            .join("binaries");
        std::fs::create_dir_all(&sidecar_dir)
            .map_err(|e| format!("Failed to create sidecar directory: {}", e))?;

        let target_filename = if triple.is_empty() {
            #[cfg(windows)]
            {
                format!("{}.exe", name)
            }
            #[cfg(not(windows))]
            {
                name.to_string()
            }
        } else {
            #[cfg(windows)]
            {
                format!("{}-{}.exe", name, triple)
            }
            #[cfg(not(windows))]
            {
                format!("{}-{}", name, triple)
            }
        };

        let target_path = sidecar_dir.join(&target_filename);

        // Content comparison: read existing file bytes and compare to embedded bytes
        let need_write = match std::fs::read(&target_path) {
            Ok(existing_bytes) => existing_bytes != bytes,
            Err(_) => true,
        };

        if need_write {
            let tmp_path =
                sidecar_dir.join(format!(".{}.tmp-{}", target_filename, std::process::id()));
            std::fs::write(&tmp_path, bytes).map_err(|e| {
                format!("Failed to write embedded sidecar to temporary file: {}", e)
            })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) =
                    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o700))
                {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!(
                        "Failed to set permissions on temporary sidecar: {}",
                        e
                    ));
                }
            }

            if let Err(e) = std::fs::rename(&tmp_path, &target_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!(
                    "Failed to rename temporary sidecar to target: {}",
                    e
                ));
            }
        }

        if target_path.is_file() {
            return cache_sidecar(name, target_path);
        }
    }

    Err(format!(
        "Sidecar binary '{}' not found on disk or embedded payload.",
        name
    ))
}

fn get_embedded_bytes(name: &str) -> Option<&'static [u8]> {
    match name {
        "mkv-remux" => {
            #[cfg(all(windows, target_arch = "x86_64"))]
            {
                return Some(include_bytes!(
                    "../binaries/mkv-remux-x86_64-pc-windows-msvc.exe"
                ));
            }
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            {
                return Some(include_bytes!(
                    "../binaries/mkv-remux-x86_64-unknown-linux-gnu"
                ));
            }
            #[allow(unreachable_code)]
            None
        }
        "mkv-extract" => {
            #[cfg(all(windows, target_arch = "x86_64"))]
            {
                return Some(include_bytes!(
                    "../binaries/mkv-extract-x86_64-pc-windows-msvc.exe"
                ));
            }
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            {
                return Some(include_bytes!(
                    "../binaries/mkv-extract-x86_64-unknown-linux-gnu"
                ));
            }
            #[allow(unreachable_code)]
            None
        }
        _ => None,
    }
}

pub(crate) fn sidecar_name(mode: &str) -> Result<&'static str, String> {
    match mode {
        "remux" => Ok("mkv-remux"),
        "extract" => Ok("mkv-extract"),
        _ => Err(format!("Unknown mode: {}", mode)),
    }
}

pub(crate) fn run_sidecar_once(
    app: &AppHandle,
    name: &str,
    args: &[&str],
) -> Result<std::process::Output, String> {
    let binary = get_sidecar_path(app, name)?;
    let mut cmd = Command::new(&binary);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output().map_err(|e| e.to_string())
}
