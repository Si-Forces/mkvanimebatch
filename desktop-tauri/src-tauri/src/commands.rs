use crate::contract::{parse_job_event, InspectionManifest};
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
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectOptions {
    #[allow(dead_code)]
    workers: Option<i64>,
    #[allow(dead_code)]
    io_jobs: Option<i64>,
    no_cache: Option<bool>,
}

#[allow(dead_code)]
fn inspect_options_to_args(options: Option<&InspectOptions>) -> Result<Vec<String>, String> {
    let Some(opts) = options else {
        return Ok(Vec::new());
    };
    let mut args = Vec::new();
    if let Some(workers) = opts.workers {
        if !(1..=64).contains(&workers) {
            return Err(format!(
                "Invalid workers '{workers}': must be an integer between 1 and 64"
            ));
        }
        args.push("--workers".to_string());
        args.push(workers.to_string());
    }
    if let Some(io_jobs) = opts.io_jobs {
        if !(0..=64).contains(&io_jobs) {
            return Err(format!(
                "Invalid io_jobs '{io_jobs}': must be an integer between 0 and 64"
            ));
        }
        args.push("--io-jobs".to_string());
        args.push(io_jobs.to_string());
    }
    if opts.no_cache.unwrap_or(false) {
        args.push("--no-cache".to_string());
    }
    Ok(args)
}

#[tauri::command]
pub async fn inspect_directory(
    _app: AppHandle,
    state: State<'_, ScanState>,
    path: String,
    options: Option<InspectOptions>,
) -> Result<InspectionManifest, String> {
    let start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    state.ensure_accepting_starts()?;
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {}", e))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }
    stop_registered_scan(&state)?;

    let scan_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    *state.active.lock().map_err(|e| e.to_string())? = Some((scan_id, 0));
    drop(start_lock);
    let registration = ScanRegistration {
        state: &state,
        id: scan_id,
    };

    let no_cache = options.as_ref().and_then(|o| o.no_cache).unwrap_or(false);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let cache = mkv_core::scan::PersistentInspectCache::default();
        let cache_opt = if no_cache { None } else { Some(&cache) };
        Ok(mkv_core::manifest::generate_inspection_manifest(
            &dir, cache_opt,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inspect_options_omitted_and_defaults() {
        assert!(inspect_options_to_args(None)
            .expect("must succeed")
            .is_empty());
        let default_opts = InspectOptions::default();
        assert!(inspect_options_to_args(Some(&default_opts))
            .expect("must succeed")
            .is_empty());
    }

    #[test]
    fn test_inspect_options_zero_and_nocache_mapping() {
        let opts = InspectOptions {
            workers: Some(8),
            io_jobs: Some(0),
            no_cache: Some(false),
        };
        assert_eq!(
            inspect_options_to_args(Some(&opts)).expect("must succeed"),
            vec!["--workers", "8", "--io-jobs", "0"]
        );

        let opts_nc = InspectOptions {
            workers: Some(4),
            io_jobs: Some(2),
            no_cache: Some(true),
        };
        assert_eq!(
            inspect_options_to_args(Some(&opts_nc)).expect("must succeed"),
            vec!["--workers", "4", "--io-jobs", "2", "--no-cache"]
        );
    }

    #[test]
    fn test_inspect_options_boundaries() {
        for (w, expected) in [
            (1, Some(vec!["--workers", "1"])),
            (64, Some(vec!["--workers", "64"])),
            (0, None),
            (-1, None),
            (65, None),
        ] {
            let opts = InspectOptions {
                workers: Some(w),
                ..Default::default()
            };
            let res = inspect_options_to_args(Some(&opts));
            if let Some(exp) = expected {
                assert_eq!(res.unwrap(), exp);
            } else {
                assert!(res.is_err());
            }
        }

        for (j, expected) in [
            (0, Some(vec!["--io-jobs", "0"])),
            (64, Some(vec!["--io-jobs", "64"])),
            (-1, None),
            (65, None),
        ] {
            let opts = InspectOptions {
                io_jobs: Some(j),
                ..Default::default()
            };
            let res = inspect_options_to_args(Some(&opts));
            if let Some(exp) = expected {
                assert_eq!(res.unwrap(), exp);
            } else {
                assert!(res.is_err());
            }
        }
    }

    #[test]
    fn test_inspect_options_deserialization() {
        let opts: InspectOptions =
            serde_json::from_str(r#"{"workers": 6, "ioJobs": 0, "noCache": true}"#).unwrap();
        assert_eq!(opts.workers, Some(6));
        assert_eq!(opts.io_jobs, Some(0));
        assert_eq!(opts.no_cache, Some(true));

        for invalid in [
            r#"{"workers": 4.5}"#,
            r#"{"workers": "4"}"#,
            r#"{"workers": true}"#,
            r#"{"ioJobs": 2.5}"#,
            r#"{"ioJobs": "2"}"#,
            r#"{"ioJobs": false}"#,
            r#"{"noCache": 1}"#,
            r#"{"noCache": "true"}"#,
        ] {
            assert!(
                serde_json::from_str::<InspectOptions>(invalid).is_err(),
                "Expected deserialization error for: {invalid}"
            );
        }
    }
}
