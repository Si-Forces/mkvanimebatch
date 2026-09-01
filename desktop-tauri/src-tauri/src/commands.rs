use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: i32, action: &str) -> Result<(), String> {
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("Failed to {action} process group"))
    }
}

#[cfg(unix)]
fn process_group_exists(pid: u32) -> bool {
    (unsafe { libc::kill(-(pid as i32), 0) }) == 0
}

#[cfg(windows)]
fn taskkill_process_tree(pid: u32) -> Result<std::process::Output, String> {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to launch taskkill: {e}"))
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some()
}

// ─── App State ──────────────────────────────────────────────────────────────

pub struct JobSlot {
    id: u64,
    child: Child,
    stopping: bool,
}

fn reserve_job_slot(
    state: &JobState,
) -> Result<std::sync::MutexGuard<'_, Option<JobSlot>>, String> {
    let lock = state.slot.lock().map_err(|e| e.to_string())?;
    if !state.accepting_starts.load(Ordering::SeqCst) {
        return Err("Application is closing; no new job can start.".into());
    }
    if lock.is_some() {
        return Err("A job is already running or stopping. Wait for it to finish.".into());
    }
    Ok(lock)
}

pub struct JobState {
    pub slot: Mutex<Option<JobSlot>>,
    pub next_id: AtomicU64,
    accepting_starts: AtomicBool,
}

pub struct ScanState {
    active: Mutex<Option<(u64, u32)>>,
    next_id: AtomicU64,
    start_lock: Mutex<()>,
    accepting_starts: AtomicBool,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            next_id: AtomicU64::new(1),
            start_lock: Mutex::new(()),
            accepting_starts: AtomicBool::new(true),
        }
    }
}

impl ScanState {
    pub fn is_active(&self) -> bool {
        self.active.lock().is_ok_and(|active| active.is_some())
    }

    fn ensure_accepting_starts(&self) -> Result<(), String> {
        if self.accepting_starts.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Application is closing; no new directory inspection can start.".into())
        }
    }

    fn wait_until_released(
        &self,
        scan_id: u64,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        wait_until(timeout, "directory inspection", || {
            Ok(!self
                .active
                .lock()
                .map_err(|e| e.to_string())?
                .is_some_and(|(id, _)| id == scan_id))
        })
    }

    fn wait_until_empty(&self, timeout: std::time::Duration) -> Result<(), String> {
        wait_until(timeout, "directory inspection", || {
            Ok(self.active.lock().map_err(|e| e.to_string())?.is_none())
        })
    }
}

struct ScanRegistration<'a> {
    state: &'a ScanState,
    id: u64,
}

impl Drop for ScanRegistration<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.state.active.lock() {
            if active.is_some_and(|(id, _)| id == self.id) {
                *active = None;
            }
        }
    }
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
            next_id: AtomicU64::new(1),
            accepting_starts: AtomicBool::new(true),
        }
    }
}

impl JobState {
    fn is_active(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.is_some())
    }

    fn wait_until_released(
        &self,
        target_id: u64,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        wait_until(timeout, "job", || {
            Ok(!self
                .slot
                .lock()
                .map_err(|e| e.to_string())?
                .as_ref()
                .is_some_and(|slot| slot.id == target_id))
        })
    }

    fn wait_until_empty(&self, timeout: std::time::Duration) -> Result<(), String> {
        wait_until(timeout, "job", || {
            Ok(self.slot.lock().map_err(|e| e.to_string())?.is_none())
        })
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
        let is_engine = *pid == self_pid
            || lowered.starts_with("mkv-remux")
            || lowered.starts_with("mkv-extract");
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
pub async fn inspect_directory(
    app: AppHandle,
    state: State<'_, ScanState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    state.ensure_accepting_starts()?;
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {}", e))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }
    let path = dir.to_string_lossy().into_owned();
    stop_registered_scan(&state)?;

    let binary = get_sidecar_path(&app, "mkv-remux")
        .map_err(|_| "Sidecar 'mkv-remux' not found.".to_string())?;
    let mut command = Command::new(binary);
    command.args(["--plan", "--input-dir", &path]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    command.process_group(0);

    let child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar: {e}"))?;
    let scan_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let pid = child.id();
    *state.active.lock().map_err(|e| e.to_string())? = Some((scan_id, pid));
    drop(start_lock);
    let registration = ScanRegistration {
        state: &state,
        id: scan_id,
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        let output = child
            .wait_with_output()
            .map_err(|e| format!("Failed to wait for sidecar: {e}"))?;

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
    .unwrap_or_else(|e| Err(format!("Async runtime error: {}", e)));
    drop(registration);
    result
}

fn spawn_log_stream_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    app: AppHandle,
    is_stderr: bool,
    event_mode: Option<String>,
    job_id: u64,
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
                            event.insert("job_id".to_string(), job_id.into());
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
struct TempAssignmentFile(Option<tempfile::TempPath>);

impl TempAssignmentFile {
    fn defuse(&mut self) -> Option<tempfile::TempPath> {
        self.0.take()
    }
}

#[tauri::command]
pub fn run_job(
    app: AppHandle,
    state: State<'_, JobState>,
    mode: String,
    args: Vec<String>,
    assignments_json: Option<String>,
) -> Result<u64, String> {
    let sidecar_name = sidecar_name(&mode)?;

    for arg in &args {
        if arg.contains('\0') {
            return Err("Argument contains invalid null character".into());
        }
    }

    let mut lock = reserve_job_slot(&state)?;

    let mut full_args = args;
    let mut temp_assignments = TempAssignmentFile(None);
    if let Some(json) = assignments_json.filter(|json| !json.is_empty()) {
        use std::io::Write;
        let mut temp_file = tempfile::Builder::new()
            .prefix("mkvanimebatch-assignments-")
            .suffix(".json")
            .tempfile()
            .map_err(|e| format!("Failed to create secure temp file: {}", e))?;
        temp_file
            .write_all(json.as_bytes())
            .map_err(|e| format!("Failed to write assignments file: {}", e))?;
        temp_file
            .flush()
            .map_err(|e| format!("Failed to flush assignments file: {}", e))?;
        let path = temp_file.into_temp_path();
        full_args.push("--sub-assignments".into());
        full_args.push(path.to_path_buf().to_string_lossy().into_owned());
        temp_assignments.0 = Some(path);
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
    *lock = Some(JobSlot {
        id: job_id,
        child,
        stopping: false,
    });
    drop(lock);

    let stdout_handle =
        spawn_log_stream_reader(stdout, app.clone(), false, Some(mode.clone()), job_id);
    let stderr_handle = spawn_log_stream_reader(stderr, app.clone(), true, None, job_id);

    let app_wait = app.clone();
    let done_mode = mode.clone();
    thread::spawn(move || {
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        let code = {
            let job = app_wait.state::<JobState>();
            // We loop wait in case it hasn't exited, without holding the lock
            let mut exit_code = None;
            loop {
                let mut lock = job.slot.lock().unwrap_or_else(|e| e.into_inner());
                let mut should_wait = false;

                if let Some(slot) = &mut *lock {
                    if slot.id == job_id {
                        match slot.child.try_wait() {
                            Ok(Some(status)) => {
                                exit_code = Some(status.code().unwrap_or(-1));
                            }
                            Ok(None) => {
                                should_wait = true;
                            }
                            Err(_) => {
                                exit_code = Some(-1);
                            }
                        }
                    }
                }
                drop(lock);

                if should_wait {
                    thread::sleep(std::time::Duration::from_millis(100));
                } else {
                    break;
                }
            }
            exit_code
        };

        drop(temp_assignments_path);

        {
            let job = app_wait.state::<JobState>();
            let mut lock = job.slot.lock().unwrap_or_else(|e| e.into_inner());
            if lock.as_ref().is_some_and(|slot| slot.id == job_id) {
                *lock = None;
            }
        }

        if let Some(code) = code {
            let _ = app_wait.emit(
                "job-done",
                serde_json::json!({"mode": done_mode, "job_id": job_id, "code": code}),
            );
        }
    });

    Ok(job_id)
}

/// Default bound on waiting for owned-process teardown to finish.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn combine_errors(errors: impl IntoIterator<Item = Option<String>>) -> Result<(), String> {
    let errors: Vec<String> = errors.into_iter().flatten().collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(" "))
    }
}

pub(crate) fn stop_job_inner(
    state: &JobState,
    scan_state: &ScanState,
    job_id: Option<u64>,
) -> Result<(), String> {
    if let Some(expected_id) = job_id {
        let lock = state.slot.lock().map_err(|e| e.to_string())?;
        match &*lock {
            Some(slot) if slot.id == expected_id => {}
            _ => return Ok(()),
        }
    }
    // Stopping the job must proceed even if scan teardown fails; report both.
    let scan_error = stop_scan_internal(scan_state).err();
    let job_error = stop_job_internal(state, job_id).err();
    combine_errors([scan_error, job_error])
}

#[tauri::command]
pub fn stop_job(
    state: State<'_, JobState>,
    scan_state: State<'_, ScanState>,
    job_id: Option<u64>,
) -> Result<(), String> {
    stop_job_inner(&state, &scan_state, job_id)
}

pub fn stop_scan_internal(state: &ScanState) -> Result<(), String> {
    stop_scan_internal_with_timeout(state, STOP_TIMEOUT)
}

fn stop_scan_internal_with_timeout(
    state: &ScanState,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let _start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    stop_registered_scan_with_timeout(state, timeout)
}

fn stop_registered_scan(state: &ScanState) -> Result<(), String> {
    stop_registered_scan_with_timeout(state, STOP_TIMEOUT)
}

fn stop_registered_scan_with_timeout(
    state: &ScanState,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let target = *state.active.lock().map_err(|e| e.to_string())?;
    let Some((scan_id, pid)) = target else {
        return Ok(());
    };

    #[cfg(unix)]
    signal_process_group(pid, libc::SIGTERM, "stop directory inspection")?;
    #[cfg(windows)]
    {
        let failure = match taskkill_process_tree(pid) {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(format!(
                "Failed to stop directory inspection: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(error) => Some(error),
        };
        if let Some(error) = failure {
            let mut active = state.active.lock().map_err(|e| e.to_string())?;
            if active
                .as_ref()
                .is_some_and(|target| *target != (scan_id, pid))
            {
                return Ok(());
            }
            if !process_exists(pid) {
                *active = None;
                return Ok(());
            }
            return Err(error);
        }
    }

    #[cfg(unix)]
    {
        for _ in 0..40 {
            if !process_group_exists(pid) {
                // UI stop path must not misreport a delivered kill as failure; exit path re-verifies strictly.
                let _ = state.wait_until_released(scan_id, timeout);
                return Ok(());
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        if state
            .active
            .lock()
            .map_err(|e| e.to_string())?
            .is_some_and(|(id, _)| id == scan_id)
        {
            signal_process_group(pid, libc::SIGKILL, "force-stop directory inspection")?;
        }
    }
    // UI stop path must not misreport a delivered kill as failure; exit path re-verifies strictly.
    let _ = state.wait_until_released(scan_id, timeout);
    Ok(())
}

pub fn stop_job_internal(state: &JobState, expected_id: Option<u64>) -> Result<(), String> {
    stop_job_internal_with_timeout(state, expected_id, STOP_TIMEOUT)
}

fn stop_job_internal_with_timeout(
    state: &JobState,
    expected_id: Option<u64>,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let target_id = {
        let mut lock = state.slot.lock().map_err(|e| e.to_string())?;
        match &mut *lock {
            Some(slot) if expected_id.is_some_and(|id| id != slot.id) => return Ok(()),
            Some(slot) if !slot.stopping => {
                let child = &mut slot.child;
                let pid = child.id();
                let current_id = slot.id;

                #[cfg(windows)]
                {
                    let output = taskkill_process_tree(pid)?;
                    if !output.status.success() && child.try_wait().ok().flatten().is_none() {
                        return Err(format!(
                            "Failed to terminate job process tree: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                }
                #[cfg(not(windows))]
                signal_process_group(pid, libc::SIGTERM, "signal job")?;

                lock.as_mut().expect("slot exists").stopping = true;

                current_id
            }
            Some(slot) => slot.id,
            None => {
                return Ok(());
            }
        }
    };

    {
        #[cfg(unix)]
        let grace = std::time::Duration::from_secs(8);
        #[cfg(windows)]
        let grace = std::time::Duration::from_secs(2);
        let start = std::time::Instant::now();
        let mut exited = false;
        while start.elapsed() < grace {
            {
                let mut lock = state.slot.lock().map_err(|e| e.to_string())?;
                if let Some(slot) = &mut *lock {
                    if slot.id == target_id {
                        match slot.child.try_wait() {
                            Ok(Some(_)) | Err(_) => {
                                exited = true;
                            }
                            Ok(None) => {}
                        }
                    } else {
                        exited = true;
                    }
                } else {
                    exited = true;
                }
            }
            if exited {
                #[cfg(unix)]
                if let Some(slot) = state
                    .slot
                    .lock()
                    .map_err(|e| e.to_string())?
                    .as_ref()
                    .filter(|slot| slot.id == target_id)
                {
                    let pid = slot.child.id();
                    if process_group_exists(pid) {
                        let _ = signal_process_group(pid, libc::SIGKILL, "force-kill job");
                    }
                }
                break;
            }
            thread::sleep(std::time::Duration::from_millis(200));
        }

        #[cfg(unix)]
        let mut group_pid = None;
        let mut lock = state.slot.lock().map_err(|e| e.to_string())?;
        if let Some(slot) = &mut *lock {
            if slot.id == target_id {
                #[cfg(unix)]
                {
                    group_pid = Some(slot.child.id());
                }
                if let Ok(None) = slot.child.try_wait() {
                    let pid = slot.child.id();
                    #[cfg(unix)]
                    signal_process_group(pid, libc::SIGKILL, "force-kill job")?;
                    #[cfg(windows)]
                    slot.child
                        .kill()
                        .map_err(|e| format!("Failed to force-kill job: {e}"))?;
                }
            }
        }
        drop(lock);
        #[cfg(unix)]
        if let Some(pid) = group_pid {
            for _ in 0..20 {
                if !process_group_exists(pid) {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(100));
            }
            if process_group_exists(pid) {
                signal_process_group(pid, libc::SIGKILL, "force-kill job")?;
            }
        }
    }

    // UI stop path must not misreport a delivered kill as failure; exit path re-verifies strictly.
    let _ = state.wait_until_released(target_id, timeout);
    Ok(())
}

fn wait_until(
    timeout: std::time::Duration,
    operation: &str,
    released: impl Fn() -> Result<bool, String>,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if released()? {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "Timed out waiting for {operation} cleanup to finish."
            ));
        }
        thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub fn cleanup_owned_processes(job_state: &JobState, scan_state: &ScanState) -> Result<(), String> {
    cleanup_owned_processes_with_timeout(job_state, scan_state, STOP_TIMEOUT)
}

fn cleanup_owned_processes_with_timeout(
    job_state: &JobState,
    scan_state: &ScanState,
    timeout: std::time::Duration,
) -> Result<(), String> {
    // Both stops run before either wait; a failed stop already reports why, so skip its wait.
    let scan_stop = stop_scan_internal_with_timeout(scan_state, timeout);
    let job_stop = stop_job_internal_with_timeout(job_state, None, timeout);
    let scan_error = scan_stop
        .and_then(|()| scan_state.wait_until_empty(timeout))
        .err();
    let job_error = job_stop
        .and_then(|()| job_state.wait_until_empty(timeout))
        .err();
    combine_errors([
        scan_error.map(|error| format!("Directory inspection cleanup failed: {error}")),
        job_error.map(|error| format!("Job cleanup failed: {error}")),
    ])
}

pub fn begin_shutdown(job_state: &JobState, scan_state: &ScanState) -> bool {
    job_state.accepting_starts.store(false, Ordering::SeqCst);
    scan_state.accepting_starts.store(false, Ordering::SeqCst);
    let scan_starting = scan_state.start_lock.try_lock().is_err();
    scan_starting || scan_state.is_active() || job_state.is_active()
}

pub fn cancel_shutdown(job_state: &JobState, scan_state: &ScanState) {
    job_state.accepting_starts.store(true, Ordering::SeqCst);
    scan_state.accepting_starts.store(true, Ordering::SeqCst);
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
pub fn close_window(
    window: tauri::Window,
    state: State<'_, JobState>,
    scan_state: State<'_, ScanState>,
) -> Result<(), String> {
    begin_shutdown(&state, &scan_state);
    if let Err(error) = cleanup_owned_processes(&state, &scan_state) {
        cancel_shutdown(&state, &scan_state);
        return Err(error);
    }
    window.close().map_err(|error| {
        cancel_shutdown(&state, &scan_state);
        error.to_string()
    })
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
        assert!(state.slot.lock().unwrap().is_none());
        assert_eq!(state.next_id.load(Ordering::SeqCst), 1);
        let id1 = state.next_id.fetch_add(1, Ordering::SeqCst);
        let id2 = state.next_id.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(state.next_id.load(Ordering::SeqCst), 3);
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_job_slot_transitions() {
        let mut slot = JobSlot {
            id: 42,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: false,
        };
        assert_eq!(slot.id, 42);
        assert!(!slot.stopping);
        slot.stopping = true;
        assert!(slot.stopping);
        let _ = slot.child.wait();
    }

    #[test]
    fn test_wait_for_job_slot_release_waits_for_cleanup() {
        let state = Arc::new(JobState::default());
        *state.slot.lock().unwrap() = Some(JobSlot {
            id: 42,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: true,
        });

        let cleanup_state = Arc::clone(&state);
        let cleanup = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(100));
            let mut slot = cleanup_state.slot.lock().unwrap().take().unwrap();
            let _ = slot.child.wait();
        });

        assert!(state
            .wait_until_released(42, std::time::Duration::from_secs(1))
            .is_ok());
        cleanup.join().unwrap();
    }

    #[test]
    fn test_wait_for_job_slot_release_times_out_while_owned() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(JobSlot {
            id: 42,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: true,
        });

        let result = state.wait_until_released(42, std::time::Duration::from_millis(10));
        assert!(result.unwrap_err().contains("Timed out"));
        let mut slot = state.slot.lock().unwrap().take().unwrap();
        let _ = slot.child.wait();
    }

    #[test]
    fn test_stale_stop_job_does_not_stop_active_scan() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        *job_state.slot.lock().unwrap() = Some(JobSlot {
            id: 7,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: false,
        });

        *scan_state.active.lock().unwrap() = Some((1, 99999));

        let result = stop_job_inner(&job_state, &scan_state, Some(99));
        assert!(result.is_ok());
        assert_eq!(*scan_state.active.lock().unwrap(), Some((1, 99999)));

        let empty_job_state = JobState::default();
        let result_empty = stop_job_inner(&empty_job_state, &scan_state, Some(99));
        assert!(result_empty.is_ok());
        assert_eq!(*scan_state.active.lock().unwrap(), Some((1, 99999)));

        let mut slot = job_state.slot.lock().unwrap().take().unwrap();
        let _ = slot.child.wait();
    }

    #[test]
    fn test_stop_job_proceeds_when_scan_teardown_fails() {
        let scan_state = Arc::new(ScanState::default());
        let poison_scan = Arc::clone(&scan_state);
        let _ = thread::spawn(move || {
            let _lock = poison_scan.start_lock.lock().unwrap();
            panic!("poison scan teardown");
        })
        .join();

        let job_state = JobState::default();
        *job_state.slot.lock().unwrap() = Some(JobSlot {
            id: 7,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: false,
        });

        let error = stop_job_inner(&job_state, &scan_state, Some(7)).unwrap_err();
        assert!(error.contains("poisoned"));
        // The job stop still ran despite the scan failure: the slot is stopping.
        assert!(job_state
            .slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|slot| slot.stopping));

        let mut slot = job_state.slot.lock().unwrap().take().unwrap();
        let _ = slot.child.wait();
    }

    #[test]
    fn test_stop_job_internal_succeeds_even_if_slot_release_times_out() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(JobSlot {
            id: 42,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: true,
        });

        let result =
            stop_job_internal_with_timeout(&state, None, std::time::Duration::from_millis(50));
        assert!(result.is_ok());

        let mut slot = state.slot.lock().unwrap().take().unwrap();
        let _ = slot.child.wait();
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_cleanup_attempts_scan_and_job_teardown() {
        let scan_state = Arc::new(ScanState::default());
        let poison_scan = Arc::clone(&scan_state);
        let _ = thread::spawn(move || {
            let _lock = poison_scan.start_lock.lock().unwrap();
            panic!("poison scan cleanup");
        })
        .join();

        let job_state = Arc::new(JobState::default());
        let poison_job = Arc::clone(&job_state);
        let _ = thread::spawn(move || {
            let _lock = poison_job.slot.lock().unwrap();
            panic!("poison job cleanup");
        })
        .join();

        let error = cleanup_owned_processes(&job_state, &scan_state).unwrap_err();
        assert!(error.contains("Directory inspection cleanup failed"));
        assert!(error.contains("Job cleanup failed"));
    }

    #[test]
    fn test_scan_startup_is_visible_before_process_registration() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        let _startup = scan_state.start_lock.lock().unwrap();
        assert!(begin_shutdown(&job_state, &scan_state));
        assert!(scan_state.ensure_accepting_starts().is_err());
        assert!(reserve_job_slot(&job_state).is_err());
    }

    #[test]
    fn test_cancel_shutdown_restores_accepting_starts() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        begin_shutdown(&job_state, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_err());
        assert!(reserve_job_slot(&job_state).is_err());

        cancel_shutdown(&job_state, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_ok());
        assert!(reserve_job_slot(&job_state).is_ok());
    }

    #[test]
    fn test_cleanup_fails_if_job_slot_not_released() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        *job_state.slot.lock().unwrap() = Some(JobSlot {
            id: 42,
            child: Command::new("rustc").arg("--version").spawn().unwrap(),
            stopping: true,
        });

        let error = cleanup_owned_processes_with_timeout(
            &job_state,
            &scan_state,
            std::time::Duration::from_millis(50),
        )
        .unwrap_err();

        assert!(error.contains("Timed out"));
        assert!(error.contains("Job cleanup failed"));

        let mut slot = job_state.slot.lock().unwrap().take().unwrap();
        let _ = slot.child.wait();
    }
}

#[cfg(all(test, unix))]
mod unix_scan_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_stop_scan_waits_until_process_is_reaped_and_registration_clears() {
        let state = Arc::new(ScanState::default());
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        *state.active.lock().unwrap() = Some((1, pid));

        let wait_state = Arc::clone(&state);
        let waiter = thread::spawn(move || {
            let registration = ScanRegistration {
                state: &wait_state,
                id: 1,
            };
            let _ = child.wait();
            thread::sleep(std::time::Duration::from_millis(200));
            drop(registration);
        });

        assert!(stop_registered_scan(&state).is_ok());
        assert!(!state.is_active());
        waiter.join().unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_scan_tests {
    use super::*;

    #[test]
    fn test_stopping_an_already_exited_scan_succeeds() {
        let state = ScanState::default();
        let mut child = Command::new("cmd")
            .args(["/C", "exit", "/B", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        *state.active.lock().unwrap() = Some((1, pid));

        assert!(stop_registered_scan(&state).is_ok());
        assert!(!state.is_active());
    }
}
