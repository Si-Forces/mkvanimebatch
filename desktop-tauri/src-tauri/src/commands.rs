use crate::contract::{parse_job_event, parse_manifest_json, InspectionManifest};
#[cfg(windows)]
use crate::process::CREATE_NO_WINDOW;
pub use crate::process::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, close_window, stop_job, JobState,
    ScanState,
};
use crate::process::{reserve_job_slot, stop_registered_scan, JobSlot, ScanRegistration};
use crate::sidecar::{get_sidecar_path, run_sidecar_once, sidecar_name};
pub use crate::telemetry::{get_system_telemetry, TelemetryState};
pub use crate::window::{
    is_maximized, minimize_window, set_window_theme, start_drag, start_resize, toggle_maximize,
};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[tauri::command]
pub async fn inspect_directory(
    app: AppHandle,
    state: State<'_, ScanState>,
    path: String,
) -> Result<InspectionManifest, String> {
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
                    if let Ok(mut event) = parse_job_event(trimmed) {
                        if let Some(ref mode) = event_mode {
                            event.attach_context(mode, job_id);
                        }
                        let _ = app.emit("job-event", event);
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
