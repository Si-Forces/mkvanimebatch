use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use sysinfo::System;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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
    pub io_mb_s: f32,
}

// ─── Sidecar Path Resolution ─────────────────────────────────────────────────

fn get_sidecar_path(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let triple = "x86_64-pc-windows-msvc";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let triple = "x86_64-unknown-linux-gnu";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let triple = "aarch64-unknown-linux-gnu";
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    let triple = "";

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
                return Ok(candidate);
            }
        }
    }

    // 2. Embedded sidecar fallback (extract from binary payload to app local data directory)
    let embedded_bytes: Option<&[u8]> = match name {
        "mkv-remux" => get_embedded_remux_bytes(),
        "mkv-extract" => get_embedded_extract_bytes(),
        _ => None,
    };

    if let Some(bytes) = embedded_bytes {
        let sidecar_dir = app
            .path()
            .app_local_data_dir()
            .map_err(|e| format!("Failed to resolve app local data directory: {}", e))?
            .join("binaries");
        std::fs::create_dir_all(&sidecar_dir)
            .map_err(|e| format!("Failed to create sidecar directory: {}", e))?;

        #[cfg(windows)]
        let target_filename = format!("{}-{}.exe", name, if triple.is_empty() { "x86_64-pc-windows-msvc" } else { triple });
        #[cfg(not(windows))]
        let target_filename = format!("{}-{}", name, if triple.is_empty() { "x86_64-unknown-linux-gnu" } else { triple });

        let target_path = sidecar_dir.join(&target_filename);

        // Content comparison: read existing file bytes and compare to embedded bytes
        let need_write = match std::fs::read(&target_path) {
            Ok(existing_bytes) => existing_bytes != bytes,
            Err(_) => true,
        };

        if need_write {
            let tmp_path = sidecar_dir.join(format!(".{}.tmp-{}", target_filename, std::process::id()));
            std::fs::write(&tmp_path, bytes)
                .map_err(|e| format!("Failed to write embedded sidecar to temporary file: {}", e))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o700)) {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("Failed to set permissions on temporary sidecar: {}", e));
                }
            }

            if let Err(e) = std::fs::rename(&tmp_path, &target_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!("Failed to rename temporary sidecar to target: {}", e));
            }
        }

        if target_path.is_file() {
            return Ok(target_path);
        }
    }

    Err(format!(
        "Sidecar binary '{}' not found on disk or embedded payload.",
        name
    ))
}

fn get_embedded_remux_bytes() -> Option<&'static [u8]> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        return Some(include_bytes!("../binaries/mkv-remux-x86_64-pc-windows-msvc.exe"));
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(include_bytes!("../binaries/mkv-remux-x86_64-unknown-linux-gnu"));
    }
    #[allow(unreachable_code)]
    None
}

fn get_embedded_extract_bytes() -> Option<&'static [u8]> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        return Some(include_bytes!("../binaries/mkv-extract-x86_64-pc-windows-msvc.exe"));
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(include_bytes!("../binaries/mkv-extract-x86_64-unknown-linux-gnu"));
    }
    #[allow(unreachable_code)]
    None
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
        io_mb_s: 0.0,
    })
}

#[tauri::command]
pub fn scan_directory(path: String) -> Result<Vec<String>, String> {
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {}", e))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }

    let mut files = Vec::new();
    let mut dirs_to_visit = vec![dir];

    while let Some(current_dir) = dirs_to_visit.pop() {
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let file_name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() && !ft.is_symlink() {
                        // Skip hidden directories, Output, fonts, attachments to match Python scanner logic
                        if !file_name.starts_with('.') && file_name != "output" && file_name != "fonts" && file_name != "attachments" {
                            dirs_to_visit.push(p);
                        }
                    } else if ft.is_file() {
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
    }
    files.sort();
    Ok(files)
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct MkvMetadata {
    pub resolution: String,
    pub video_codec: String,
    pub video_tag: String,
    pub audio_summary: String,
    pub has_flac: bool,
    pub sub_summary: String,
    pub fonts_count: usize,
}

/// Parse the track/attachment data out of `mkvmerge -J` JSON into metadata.
/// Shared by `inspect_mkv_file` (single) and `inspect_directory` (batch).
fn parse_mkv_metadata(json_val: &serde_json::Value) -> MkvMetadata {
    let mut res = "1080p".to_string();
    let mut vcodec = "MKV".to_string();
    let mut audio_parts = Vec::new();
    let mut has_flac = false;
    let mut sub_langs = Vec::new();
    let mut fonts_count = 0;

    if let Some(tracks) = json_val.get("tracks").and_then(|t| t.as_array()) {
        for t in tracks {
            let t_type = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let codec = t.get("codec").and_then(|v| v.as_str()).unwrap_or("");
            let props = t.get("properties");

            if t_type == "video" {
                if codec.to_lowercase().contains("hevc") || codec.to_lowercase().contains("h.265") {
                    vcodec = "HEVC".into();
                } else if codec.to_lowercase().contains("avc") || codec.to_lowercase().contains("h.264") {
                    vcodec = "AVC".into();
                } else if codec.to_lowercase().contains("av1") {
                    vcodec = "AV1".into();
                } else if codec.to_lowercase().contains("vp9") {
                    vcodec = "VP9".into();
                }

                if let Some(dim) = props.and_then(|p| p.get("pixel_dimensions")).and_then(|v| v.as_str()) {
                    let parts: Vec<&str> = dim.split('x').collect();
                    if parts.len() == 2 {
                        let width: u32 = parts[0].trim().parse().unwrap_or(0);
                        let height: u32 = parts[1].trim().parse().unwrap_or(0);
                        if width >= 3800 || height >= 2100 {
                            res = "4K".into();
                        } else if width >= 2500 || height >= 1400 {
                            res = "1440p".into();
                        } else if width >= 1900 || height >= 1000 {
                            res = "1080p".into();
                        } else if width >= 1200 || height >= 700 {
                            res = "720p".into();
                        } else if height >= 480 {
                            res = "480p".into();
                        }
                    } else if dim.contains("3840") || dim.contains("2160") {
                        res = "4K".into();
                    } else if dim.contains("1920") || dim.contains("1080") {
                        res = "1080p".into();
                    } else if dim.contains("1280") || dim.contains("720") {
                        res = "720p".into();
                    }
                }
            } else if t_type == "audio" {
                let is_flac = codec.to_lowercase().contains("flac");
                if is_flac {
                    has_flac = true;
                }
                let channels = props
                    .and_then(|p| p.get("audio_channels"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2);
                let ch_str = match channels {
                    8 => "7.1",
                    6 => "5.1",
                    1 => "1.0",
                    _ => "2.0",
                };
                let aud_name = if is_flac {
                    "FLAC"
                } else if codec.to_lowercase().contains("aac") {
                    "AAC"
                } else if codec.to_lowercase().contains("opus") {
                    "Opus"
                } else if codec.to_lowercase().contains("ac3") || codec.to_lowercase().contains("ac-3") {
                    "AC3"
                } else {
                    "Audio"
                };
                audio_parts.push(format!("{} {}", aud_name, ch_str));
            } else if t_type == "subtitles" {
                let lang = props
                    .and_then(|p| p.get("language"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("sub");
                let ulang = lang.to_uppercase();
                if !sub_langs.contains(&ulang) {
                    sub_langs.push(ulang);
                }
            }
        }
    }

    if let Some(attachments) = json_val.get("attachments").and_then(|a| a.as_array()) {
        fonts_count = attachments.len();
    }

    let video_tag = format!("{} {}", res, vcodec);
    let audio_summary = if audio_parts.is_empty() {
        "No Audio".into()
    } else {
        audio_parts.join(" / ")
    };
    let sub_summary = if sub_langs.is_empty() {
        "ASS Subs".into()
    } else {
        format!("ASS ({})", sub_langs.join(", "))
    };

    MkvMetadata {
        resolution: res,
        video_codec: vcodec,
        video_tag,
        audio_summary,
        has_flac,
        sub_summary,
        fonts_count,
    }
}

/// Probe a single MKV with `mkvmerge -J` and return parsed metadata.
fn probe_mkv(file_path: &std::path::Path) -> Result<MkvMetadata, String> {
    let mut cmd = std::process::Command::new("mkvmerge");
    cmd.args(["-J", &file_path.to_string_lossy()]);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().map_err(|e| format!("Failed to run mkvmerge: {}", e))?;
    if !output.status.success() && output.stdout.is_empty() {
        return Err("mkvmerge failed to inspect file".into());
    }

    let json_val: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse mkvmerge JSON: {}", e))?;

    Ok(parse_mkv_metadata(&json_val))
}

#[tauri::command]
pub fn inspect_mkv_file(dir_path: String, filename: String) -> Result<MkvMetadata, String> {
    let base = std::fs::canonicalize(&dir_path)
        .map_err(|e| format!("Invalid base directory: {}", e))?;
    if !base.is_dir() {
        return Err("Base path is not a directory".into());
    }

    let full_path = base.join(&filename);
    let canonical_file = std::fs::canonicalize(&full_path)
        .map_err(|e| format!("Invalid file path: {}", e))?;

    if !canonical_file.starts_with(&base) || !canonical_file.is_file() {
        return Err("Path traversal detected or invalid file".into());
    }

    probe_mkv(&canonical_file)
}

/// Scan a directory for MKVs and probe each one on a small thread pool,
/// returning (filename, metadata) pairs in a single IPC round-trip. This
/// replaces the old pattern of one `inspect_mkv_file` invoke per file, which
/// spawned one mkvmerge process per IPC call.
#[tauri::command]
pub fn inspect_directory(path: String) -> Result<Vec<(String, MkvMetadata)>, String> {
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {}", e))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }
    let files = scan_directory(path)?;

    let results: Vec<(String, MkvMetadata)> = thread::scope(|s| {
        // Bounded worker pool: probe at most 4 files concurrently to avoid
        // saturating disk I/O with simultaneous mkvmerge processes.
        let (tx, rx) = std::sync::mpsc::channel::<(String, MkvMetadata)>();
        let chunk_size = 4.max((files.len() + 3) / 4);
        let mut handles = Vec::new();

        for chunk in files.chunks(chunk_size) {
            let tx = tx.clone();
            let dir = dir.clone();
            let chunk: Vec<String> = chunk.to_vec();
            handles.push(s.spawn(move || {
                for name in chunk {
                    let full = dir.join(&name);
                    if let Ok(meta) = probe_mkv(&full) {
                        let _ = tx.send((name, meta));
                    }
                }
            }));
        }
        drop(tx); // close sender so rx iteration terminates

        let mut out: Vec<(String, MkvMetadata)> = rx.iter().collect();
        for h in handles {
            let _ = h.join();
        }
        // Restore deterministic (sorted) order to match the scan order.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    });

    Ok(results)
}

#[tauri::command]
pub fn run_job(
    app: AppHandle,
    state: State<'_, JobState>,
    mode: String,
    args: Vec<String>,
) -> Result<(), String> {
    let sidecar_name = match mode.as_str() {
        "remux" => "mkv-remux",
        "extract" => "mkv-extract",
        _ => return Err(format!("Unknown mode: {}", mode)),
    };

    for arg in &args {
        if arg.contains('\0') {
            return Err("Argument contains invalid null character".into());
        }
    }

    let mut lock = state.child.lock().map_err(|e| e.to_string())?;
    if lock.is_some() {
        return Err("A job is already running. Stop it first.".into());
    }

    let binary = get_sidecar_path(&app, sidecar_name)?;

    let mut cmd = Command::new(&binary);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to launch {}: {}", binary.display(), e))?;

    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;

    *lock = Some(child);
    drop(lock);

    let app_out = app.clone();
    let stdout_handle = thread::spawn(move || {
        let mut batch = Vec::with_capacity(30);
        let mut last_flush = std::time::Instant::now();
        for line in BufReader::new(stdout).lines() {
            if let Ok(line) = line {
                batch.push(line);
                if batch.len() >= 25 || last_flush.elapsed().as_millis() >= 25 {
                    let _ = app_out.emit("job-log", &batch);
                    batch.clear();
                    last_flush = std::time::Instant::now();
                }
            }
        }
        if !batch.is_empty() {
            let _ = app_out.emit("job-log", &batch);
        }
    });

    let app_err = app.clone();
    let stderr_handle = thread::spawn(move || {
        let mut batch = Vec::with_capacity(30);
        let mut last_flush = std::time::Instant::now();
        for line in BufReader::new(stderr).lines() {
            if let Ok(line) = line {
                batch.push(format!("[ERR] {}", line));
                if batch.len() >= 25 || last_flush.elapsed().as_millis() >= 25 {
                    let _ = app_err.emit("job-log", &batch);
                    batch.clear();
                    last_flush = std::time::Instant::now();
                }
            }
        }
        if !batch.is_empty() {
            let _ = app_err.emit("job-log", &batch);
        }
    });

    let app_wait = app.clone();
    thread::spawn(move || {
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
        let code = {
            let job = app_wait.state::<JobState>();
            let mut lock = job.child.lock().unwrap();
            if let Some(ref mut c) = *lock {
                let code = c.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                *lock = None;
                Some(code)
            } else {
                None
            }
        };

        if let Some(code) = code {
            let _ = app_wait.emit("job-done", code);
        }
    });

    Ok(())
}

#[tauri::command]
pub fn stop_job(state: State<'_, JobState>) -> Result<(), String> {
    let mut lock = state.child.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut child) = *lock {
        if let Ok(None) = child.try_wait() {
            let pid = child.id();
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .output();
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill")
                    .args(["-s", "KILL", "--", &format!("-{}", pid)])
                    .output();
                let _ = child.kill();
            }
        }
    }
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

    if output.status.success() {
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
pub fn is_maximized(window: tauri::Window) -> bool {
    window.is_maximized().unwrap_or(false)
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

#[tauri::command]
pub fn set_window_theme(app: AppHandle, theme: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let tauri_theme = match theme.as_str() {
            "light" => tauri::Theme::Light,
            _ => tauri::Theme::Dark,
        };
        let _ = window.set_theme(Some(tauri_theme));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_mkv_metadata_4k_hevc_flac() {
        let json_val = json!({
            "tracks": [
                {
                    "type": "video",
                    "codec": "HEVC/H.265",
                    "properties": {
                        "pixel_dimensions": "3840x2160"
                    }
                },
                {
                    "type": "audio",
                    "codec": "FLAC",
                    "properties": {
                        "audio_channels": 6
                    }
                },
                {
                    "type": "subtitles",
                    "codec": "SubStationAlpha",
                    "properties": {
                        "language": "ind"
                    }
                }
            ],
            "attachments": [
                {"id": 1, "file_name": "font.ttf"}
            ]
        });

        let meta = parse_mkv_metadata(&json_val);
        assert_eq!(meta.resolution, "4K");
        assert_eq!(meta.video_codec, "HEVC");
        assert_eq!(meta.video_tag, "4K HEVC");
        assert!(meta.has_flac);
        assert_eq!(meta.audio_summary, "FLAC 5.1");
        assert_eq!(meta.sub_summary, "ASS (IND)");
        assert_eq!(meta.fonts_count, 1);
    }

    #[test]
    fn test_parse_mkv_metadata_1440p_opus_7_1() {
        let json_val = json!({
            "tracks": [
                {
                    "type": "video",
                    "codec": "VP9",
                    "properties": {
                        "pixel_dimensions": "2560x1440"
                    }
                },
                {
                    "type": "audio",
                    "codec": "Opus",
                    "properties": {
                        "audio_channels": 8
                    }
                }
            ]
        });

        let meta = parse_mkv_metadata(&json_val);
        assert_eq!(meta.resolution, "1440p");
        assert_eq!(meta.video_codec, "VP9");
        assert_eq!(meta.audio_summary, "Opus 7.1");
        assert!(!meta.has_flac);
    }

    #[test]
    fn test_parse_mkv_metadata_empty_and_fallback() {
        let json_val = json!({});
        let meta = parse_mkv_metadata(&json_val);
        assert_eq!(meta.resolution, "1080p");
        assert_eq!(meta.video_codec, "MKV");
        assert_eq!(meta.audio_summary, "No Audio");
        assert_eq!(meta.sub_summary, "ASS Subs");
        assert_eq!(meta.fonts_count, 0);
    }
}
