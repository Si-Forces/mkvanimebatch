use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[allow(dead_code)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ─── App State ──────────────────────────────────────────────────────────────

pub struct JobState {
    pub child: Mutex<Option<(u64, Child)>>,
    pub next_id: AtomicU64,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }
}

pub struct TelemetryState {
    inner: Mutex<TelemetryInner>,
}

struct TelemetryInner {
    sys: System,
    /// Timestamp of the previous telemetry poll; the window between polls
    /// converts the per-refresh written-bytes delta into a rate.
    last_poll: Option<std::time::Instant>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self {
            inner: Mutex::new(TelemetryInner {
                sys,
                last_poll: None,
            }),
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

fn get_sidecar_path(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
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

fn sidecar_name(mode: &str) -> Result<&'static str, String> {
    match mode {
        "remux" => Ok("mkv-remux"),
        "extract" => Ok("mkv-extract"),
        _ => Err(format!("Unknown mode: {}", mode)),
    }
}

fn run_sidecar_once(
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

// ─── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_system_telemetry(state: State<'_, TelemetryState>) -> Result<TelemetryData, String> {
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    let sys = &mut inner.sys;
    sys.refresh_cpu_all();
    sys.refresh_memory();
    // `written_bytes` is a per-refresh delta, so the poll interval doubles as
    // the measurement window. Scoped to this app and the mkv-* sidecar
    // processes; I/O from their grandchildren (mkvmerge/ffmpeg) is not
    // attributed to them by the OS process accounting used here.
    sys.refresh_processes(ProcessesToUpdate::All, false);

    let self_pid = Pid::from_u32(std::process::id());
    let mut written_window = 0u64;
    for (pid, process) in sys.processes() {
        let lowered = process.name().to_string_lossy().to_lowercase();
        let is_engine =
            *pid == self_pid || lowered.starts_with("mkv-remux") || lowered.starts_with("mkv-extract");
        if is_engine {
            written_window += process.disk_usage().written_bytes;
        }
    }

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
    // `sys` is no longer borrowed past this point, so the poll bookkeeping
    // can read `last_poll` freely.
    let io_mb_s = match inner.last_poll {
        Some(prev) => {
            let elapsed = prev.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                (written_window as f64 / elapsed / (1024.0 * 1024.0)) as f32
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    inner.last_poll = Some(std::time::Instant::now());

    Ok(TelemetryData {
        cpu_pct,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        ram_pct,
        io_mb_s,
    })
}

pub fn parse_manifest_json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if bytes.is_empty() {
        return Err("Sidecar returned empty stdout.".to_string());
    }

    let manifest: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        let prefix = String::from_utf8_lossy(bytes)
            .chars()
            .take(100)
            .collect::<String>();
        format!("Failed to parse JSON: {}. Got: {}", e, prefix)
    })?;

    if manifest.get("version").and_then(|v| v.as_i64()) != Some(1) {
        return Err("Unsupported manifest version. Expected version 1.".to_string());
    }

    if manifest.get("items").and_then(|it| it.as_array()).is_none() {
        return Err("Malformed manifest: 'items' array missing.".to_string());
    }

    Ok(manifest)
}

#[tauri::command]
pub async fn inspect_directory(app: AppHandle, path: String) -> Result<serde_json::Value, String> {
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {}", e))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }
    let path = dir.to_string_lossy().into_owned();

    tauri::async_runtime::spawn_blocking(move || {
        get_sidecar_path(&app, "mkv-remux")
            .map_err(|_| "Sidecar 'mkv-remux' not found.".to_string())?;
        let output = run_sidecar_once(&app, "mkv-remux", &["--plan", "--input-dir", &path])
            .map_err(|e| format!("Failed to spawn sidecar: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!(
                "Sidecar exit code {:?}: {}",
                output.status.code(),
                stderr
            ));
        }

        parse_manifest_json(&output.stdout)
    })
    .await
    .unwrap_or_else(|e| Err(format!("Async runtime error: {}", e)))
}

fn spawn_log_stream_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    app: AppHandle,
    is_stderr: bool,
    event_mode: Option<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut log_batch = Vec::with_capacity(30);
        let mut last_flush = std::time::Instant::now();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if !is_stderr {
                if let Some(event_str) = line.strip_prefix("@event:") {
                    let trimmed = event_str.trim();
                    if let Ok(mut event_json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if let (Some(event), Some(ref mode)) =
                            (event_json.as_object_mut(), &event_mode)
                        {
                            event.insert("mode".to_string(), mode.clone().into());
                        }
                        let _ = app.emit("job-event", event_json);
                        continue;
                    }
                }
            }
            let formatted_line = if is_stderr {
                format!("[ERR] {}", line)
            } else {
                line
            };
            log_batch.push(formatted_line);
            if log_batch.len() >= 25 || last_flush.elapsed().as_millis() >= 25 {
                let _ = app.emit("job-log", &log_batch);
                log_batch.clear();
                last_flush = std::time::Instant::now();
            }
        }
        if !log_batch.is_empty() {
            let _ = app.emit("job-log", &log_batch);
        }
    })
}

/// Owns the temporary assignments file until it is handed to the wait thread.
/// Dropping the guard (any early return before spawn succeeds) removes the
/// file; after `defuse()`, removal becomes the wait thread's responsibility.
struct TempAssignmentFile(Option<PathBuf>);

impl TempAssignmentFile {
    fn defuse(&mut self) -> Option<PathBuf> {
        self.0.take()
    }
}

impl Drop for TempAssignmentFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[tauri::command]
pub fn run_job(
    app: AppHandle,
    state: State<'_, JobState>,
    mode: String,
    args: Vec<String>,
    assignments_json: Option<String>,
) -> Result<(), String> {
    let sidecar_name = sidecar_name(&mode)?;

    for arg in &args {
        if arg.contains('\0') {
            return Err("Argument contains invalid null character".into());
        }
    }

    // GUI manual subtitle selections are persisted verbatim to a temp file and
    // handed to the sidecar by path; this adapter never interprets the payload.
    let mut full_args = args;
    let mut temp_assignments = TempAssignmentFile(None);
    if let Some(json) = assignments_json.filter(|json| !json.is_empty()) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "mkvanimebatch-assignments-{}-{}.json",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, json.as_bytes())
            .map_err(|e| format!("Failed to write assignments file: {}", e))?;
        full_args.push("--sub-assignments".into());
        full_args.push(path.to_string_lossy().into_owned());
        temp_assignments.0 = Some(path);
    }

    let mut lock = state.child.lock().map_err(|e| e.to_string())?;
    if let Some((_, ref mut running_child)) = *lock {
        match running_child.try_wait() {
            Ok(Some(_)) => {
                // Previous child process has already exited; eagerly clean up slot
                *lock = None;
            }
            Ok(None) => {
                return Err("A job is already running. Stop it first.".into());
            }
            Err(e) => {
                // Cannot confirm the previous child has been reaped; refuse to
                // overwrite the slot, which would leak/zombie the old process.
                return Err(format!("Failed to check status of running job: {e}"));
            }
        }
    }

    let binary = get_sidecar_path(&app, sidecar_name)?;

    let mut cmd = Command::new(&binary);
    cmd.args(&full_args);
    if !full_args.iter().any(|a| a == "--events") {
        cmd.arg("--events");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to launch {}: {}", binary.display(), e))?;

    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;

    // Spawn fully succeeded; the sidecar has (or will) read its argv, so the
    // wait thread now owns temp-file removal for after the process exits.
    let temp_assignments_path = temp_assignments.defuse();

    let job_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    *lock = Some((job_id, child));
    drop(lock);

    let stdout_handle = spawn_log_stream_reader(stdout, app.clone(), false, Some(mode.clone()));
    let stderr_handle = spawn_log_stream_reader(stderr, app.clone(), true, None);

    let app_wait = app.clone();
    let done_mode = mode;
    thread::spawn(move || {
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
        let code = {
            let job = app_wait.state::<JobState>();
            let mut lock = job.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((id, ref mut c)) = *lock {
                if id == job_id {
                    let code = c.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                    *lock = None;
                    Some(code)
                } else {
                    None
                }
            } else {
                None
            }
        };

        // The sidecar has fully exited, so its argv (including the assignments
        // file path) is no longer needed. Safe on the id-mismatch branch too:
        // takeover implies this job's process had already exited.
        if let Some(path) = temp_assignments_path {
            let _ = std::fs::remove_file(path);
        }

        if let Some(code) = code {
            let _ = app_wait.emit(
                "job-done",
                serde_json::json!({"mode": done_mode, "code": code}),
            );
        }
    });

    Ok(())
}

#[tauri::command]
pub fn stop_job(state: State<'_, JobState>) -> Result<(), String> {
    {
        // Signal phase: hold the lock only long enough to inspect the child
        // and deliver the stop signal.
        let mut lock = state.child.lock().map_err(|e| e.to_string())?;
        if let Some((_, ref mut child)) = *lock {
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
                    // Ask nicely first: SIGTERM lets the Python sidecar run
                    // its cancellation handlers (terminate ffmpeg/mkvmerge
                    // children, clean temp dirs, emit final events).
                    let _ = Command::new("kill")
                        .args(["-s", "TERM", "--", &format!("-{}", pid)])
                        .output();
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        // Grace phase: wait for a cooperative exit WITHOUT holding the mutex,
        // so the job-wait thread can reap the process and concurrent commands
        // stay responsive. Re-acquire briefly per tick just to poll status.
        let grace = std::time::Duration::from_secs(8);
        let start = std::time::Instant::now();
        let mut exited = false;
        while start.elapsed() < grace {
            {
                let mut lock = state.child.lock().map_err(|e| e.to_string())?;
                match *lock {
                    Some((_, ref mut child)) => match child.try_wait() {
                        Ok(Some(_)) | Err(_) => {
                            exited = true;
                        }
                        Ok(None) => {}
                    },
                    None => {
                        exited = true;
                    }
                }
            }
            if exited {
                return Ok(());
            }
            thread::sleep(std::time::Duration::from_millis(200));
        }

        // Escalation phase: the sidecar ignored the grace period; hard kill.
        let mut lock = state.child.lock().map_err(|e| e.to_string())?;
        if let Some((_, ref mut child)) = *lock {
            if child
                .try_wait()
                .map(|status| status.is_none())
                .unwrap_or(false)
            {
                let pid = child.id();
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
pub async fn clear_cache(app: AppHandle, mode: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let sidecar_name = sidecar_name(&mode)?;
        let output = run_sidecar_once(&app, sidecar_name, &["--clear-cache"])?;

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
            Err(format!(
                "clear-cache failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                combined
            ))
        }
    })
    .await
    .unwrap_or_else(|e| Err(format!("Async runtime error: {}", e)))
}

#[tauri::command]
pub fn pick_folder() -> Option<String> {
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

    #[test]
    fn test_parse_manifest_json_valid_v1() {
        let json_bytes = br#"{
            "version": 1,
            "items": [
                {
                    "id": "Season 01/Show - 01.mkv",
                    "name": "Show - 01.mkv",
                    "metadata": {
                        "resolution": "1080p",
                        "video_codec": "HEVC",
                        "video_tag": "1080p HEVC",
                        "audio_summary": "FLAC 2.0",
                        "has_flac": true,
                        "sub_summary": "ASS (IND)",
                        "fonts_count": 5
                    }
                }
            ]
        }"#;

        let res = parse_manifest_json(json_bytes);
        assert!(res.is_ok());
        let manifest = res.unwrap();
        assert_eq!(manifest["version"], 1);
        let items = manifest["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "Season 01/Show - 01.mkv");
        assert_eq!(items[0]["name"], "Show - 01.mkv");
    }

    #[test]
    fn test_parse_manifest_json_rejects_unsupported_version() {
        let json_bytes = br#"{"version": 2, "items": []}"#;
        let res = parse_manifest_json(json_bytes);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Unsupported manifest version"));
    }

    #[test]
    fn test_parse_manifest_json_rejects_empty() {
        let res = parse_manifest_json(b"");
        assert!(res.is_err());
    }

    #[test]
    fn test_event_line_parsing() {
        let line =
            r#"@event: {"version":1,"type":"item_start","id":"Season 01/01.mkv","file":"01.mkv"}"#;
        assert!(line.starts_with("@event:"));
        let event_str = line.strip_prefix("@event:").unwrap().trim();
        let parsed: serde_json::Value = serde_json::from_str(event_str).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["type"], "item_start");
        assert_eq!(parsed["id"], "Season 01/01.mkv");
        assert_eq!(parsed["file"], "01.mkv");
    }

    #[test]
    fn test_job_state_initialization_and_increment() {
        let state = JobState::default();
        assert!(state.child.lock().unwrap().is_none());
        assert_eq!(state.next_id.load(Ordering::SeqCst), 1);
        let id1 = state.next_id.fetch_add(1, Ordering::SeqCst);
        let id2 = state.next_id.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(state.next_id.load(Ordering::SeqCst), 3);
    }
}
