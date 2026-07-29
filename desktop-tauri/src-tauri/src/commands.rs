use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use sysinfo::System;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[allow(dead_code)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ─── App State ──────────────────────────────────────────────────────────────

pub struct JobState {
    pub child: Mutex<Option<Child>>,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }
}

pub struct TelemetryState {
    pub sys: Mutex<System>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self {
            sys: Mutex::new(sys),
        }
    }
}

#[derive(serde::Serialize)]
pub struct TelemetryData {
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub ram_pct: f32,
}

// ─── Sidecar Path Resolution ─────────────────────────────────────────────────

fn get_sidecar_path(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let triple = "x86_64-pc-windows-msvc";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let triple = "x86_64-unknown-linux-gnu";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let triple = "aarch64-unknown-linux-gnu";

    #[cfg(windows)]
    let filename = format!("{}-{}.exe", name, triple);
    #[cfg(not(windows))]
    let filename = format!("{}-{}", name, triple);

    if let Ok(resource_dir) = app.path().resource_dir() {
        let candidate = resource_dir.join(&filename);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join(&filename);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    let manifest = env!("CARGO_MANIFEST_DIR");
    let candidate = PathBuf::from(manifest).join("binaries").join(&filename);
    if candidate.exists() {
        return Ok(candidate);
    }

    Err(format!(
        "Sidecar binary '{}' not found. Run build-sidecars first.",
        filename
    ))
}

// ─── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_system_telemetry(state: State<'_, TelemetryState>) -> Result<TelemetryData, String> {
    let mut sys = state.sys.lock().map_err(|e| e.to_string())?;
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cpus = sys.cpus();
    let cpu_pct = if !cpus.is_empty() {
        let total: f32 = cpus.iter().map(|c| c.cpu_usage()).sum();
        (total / cpus.len() as f32).round()
    } else {
        sys.global_cpu_usage().round()
    };

    let ram_used = sys.used_memory() / 1024 / 1024;
    let ram_total = sys.total_memory() / 1024 / 1024;
    let ram_pct = if ram_total > 0 {
        ((ram_used as f32 / ram_total as f32) * 100.0).round()
    } else {
        0.0
    };

    Ok(TelemetryData {
        cpu_pct,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        ram_pct,
    })
}

#[tauri::command]
pub fn scan_directory(path: String) -> Result<Vec<String>, String> {
    let dir = PathBuf::from(&path);
    if !dir.exists() || !dir.is_dir() {
        return Err("Directory does not exist".into());
    }

    let mut files = Vec::new();
    let mut dirs_to_visit = vec![dir];

    while let Some(current_dir) = dirs_to_visit.pop() {
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let file_name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                
                if p.is_dir() {
                    // Skip hidden directories, Output, fonts, attachments to match Python scanner logic
                    if !file_name.starts_with('.') && file_name != "output" && file_name != "fonts" && file_name != "attachments" {
                        dirs_to_visit.push(p);
                    }
                } else if p.is_file() {
                    if let Some(ext) = p.extension() {
                        if ext.to_string_lossy().to_lowercase() == "mkv" {
                            if let Some(name) = p.file_name() {
                                files.push(name.to_string_lossy().into_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

#[tauri::command]
pub fn run_job(
    app: AppHandle,
    state: State<'_, JobState>,
    mode: String,
    args: Vec<String>,
) -> Result<(), String> {
    {
        let lock = state.child.lock().map_err(|e| e.to_string())?;
        if lock.is_some() {
            return Err("A job is already running. Stop it first.".into());
        }
    }

    let sidecar_name = match mode.as_str() {
        "remux" => "mkv-remux",
        "extract" => "mkv-extract",
        _ => return Err(format!("Unknown mode: {}", mode)),
    };

    let binary = get_sidecar_path(&app, sidecar_name)?;

    let mut cmd = Command::new(&binary);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to launch {}: {}", binary.display(), e))?;

    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;

    {
        let mut lock = state.child.lock().map_err(|e| e.to_string())?;
        *lock = Some(child);
    }

    let app_out = app.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if let Ok(line) = line {
                let _ = app_out.emit("job-log", line);
            }
        }
    });

    let app_err = app.clone();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if let Ok(line) = line {
                let _ = app_err.emit("job-log", format!("[ERR] {}", line));
            }
        }
    });

    let app_wait = app.clone();
    thread::spawn(move || loop {
        let status = {
            let job = app_wait.state::<JobState>();
            let mut lock = job.child.lock().unwrap();
            if let Some(ref mut c) = *lock {
                c.try_wait().ok().flatten()
            } else {
                return;
            }
        };

        if let Some(status) = status {
            let code = status.code().unwrap_or(-1);
            let _ = app_wait.emit("job-done", code);
            let job = app_wait.state::<JobState>();
            let mut lock = job.child.lock().unwrap();
            *lock = None;
            return;
        }

        thread::sleep(std::time::Duration::from_millis(100));
    });

    Ok(())
}

#[tauri::command]
pub fn stop_job(state: State<'_, JobState>) -> Result<(), String> {
    let mut lock = state.child.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut child) = *lock {
        #[cfg(windows)]
        {
            let pid = child.id();
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
        }
        #[cfg(not(windows))]
        {
            let _ = child.kill();
        }
    }
    *lock = None;
    Ok(())
}

#[tauri::command]
pub fn clear_cache(app: AppHandle, mode: String) -> Result<String, String> {
    let sidecar_name = match mode.as_str() {
        "remux" => "mkv-remux",
        "extract" => "mkv-extract",
        _ => return Err(format!("Unknown mode: {}", mode)),
    };

    let binary = get_sidecar_path(&app, sidecar_name)?;

    let mut cmd = Command::new(&binary);
    cmd.arg("--clear-cache");

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}{}", stdout, stderr).trim().to_string();

    if output.status.success() || output.status.code() == Some(0) {
        Ok(if combined.is_empty() {
            "Cache cleared.".into()
        } else {
            combined
        })
    } else {
        Err(format!("clear-cache failed (exit {}): {}", output.status.code().unwrap_or(-1), combined))
    }
}

#[tauri::command]
pub fn pick_folder() -> Option<String> {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Video Files / Anime Folder", &["mkv", "mp4", "avi", "mka", "ass", "srt", "*"])
        .pick_file()
    {
        if path.is_file() {
            if let Some(parent) = path.parent() {
                return Some(parent.to_string_lossy().into_owned());
            }
        }
        return Some(path.to_string_lossy().into_owned());
    }

    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn close_window(window: tauri::Window) {
    let _ = window.close();
}

#[tauri::command]
pub fn minimize_window(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command]
pub fn toggle_maximize(window: tauri::Window) {
    if let Ok(maximized) = window.is_maximized() {
        if maximized {
            let _ = window.unmaximize();
        } else {
            let _ = window.maximize();
        }
    }
}

#[tauri::command]
pub fn start_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

#[tauri::command]
pub fn start_resize(window: tauri::Window, direction: String) {
    use tauri_runtime::ResizeDirection;
    let dir = match direction.as_str() {
        "n" => ResizeDirection::North,
        "s" => ResizeDirection::South,
        "e" => ResizeDirection::East,
        "w" => ResizeDirection::West,
        "ne" => ResizeDirection::NorthEast,
        "nw" => ResizeDirection::NorthWest,
        "se" => ResizeDirection::SouthEast,
        "sw" => ResizeDirection::SouthWest,
        _ => return,
    };
    let _ = window.start_resize_dragging(dir);
}
