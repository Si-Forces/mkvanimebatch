use crate::contract::{parse_job_event, parse_manifest_json, InspectionManifest};
pub use crate::process::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, close_window, stop_job, JobState,
    ScanState,
};
#[cfg(test)]
use crate::process::{
    cleanup_owned_processes_with_timeout, stop_job_inner, stop_job_internal_with_timeout,
};
use crate::process::{reserve_job_slot, stop_registered_scan, JobSlot, ScanRegistration};
use crate::sidecar::{get_sidecar_path, run_sidecar_once, sidecar_name};
#[cfg(test)]
use crate::telemetry::is_engine_process;
pub use crate::telemetry::{get_system_telemetry, TelemetryState};
pub use crate::window::{
    is_maximized, minimize_window, set_window_theme, start_drag, start_resize, toggle_maximize,
};
#[cfg(test)]
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
#[cfg(test)]
use sysinfo::Pid;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_engine_process_ancestry_and_naming() {
        let self_pid = Pid::from_u32(100);
        let sidecar_pid = Pid::from_u32(101);
        let grandchild_pid = Pid::from_u32(102);
        let unrelated_sidecar = Pid::from_u32(201);
        let unrelated_grandchild = Pid::from_u32(202);
        let unrelated_app = Pid::from_u32(300);

        let mut parents = HashMap::new();
        parents.insert(sidecar_pid, self_pid);
        parents.insert(grandchild_pid, sidecar_pid);
        parents.insert(unrelated_sidecar, Pid::from_u32(999));
        parents.insert(unrelated_grandchild, unrelated_sidecar);
        parents.insert(unrelated_app, self_pid);

        let parent_fn = |p| parents.get(&p).copied();

        // 1. self_pid is always engine
        assert!(is_engine_process(
            self_pid,
            "mkvanimebatch",
            parent_fn,
            self_pid
        ));

        // 2. sidecar descendant of self_pid is engine
        assert!(is_engine_process(
            sidecar_pid,
            "mkv-remux-x86_64",
            parent_fn,
            self_pid
        ));

        // 3. grandchild (mkvmerge, mkvextract, ffmpeg) descending from self_pid is engine
        assert!(is_engine_process(
            grandchild_pid,
            "mkvmerge",
            parent_fn,
            self_pid
        ));

        // 4. unrelated sidecar or grandchild with different ancestor is NOT engine
        assert!(!is_engine_process(
            unrelated_sidecar,
            "mkv-remux-x86_64",
            parent_fn,
            self_pid
        ));
        assert!(!is_engine_process(
            unrelated_grandchild,
            "mkvmerge",
            parent_fn,
            self_pid
        ));

        // 5. unrelated binary even if child of self_pid is NOT engine
        assert!(!is_engine_process(
            unrelated_app,
            "chrome",
            parent_fn,
            self_pid
        ));

        // 6. Unicode process names must not panic on UTF-8 character boundaries
        assert!(!is_engine_process(
            sidecar_pid,
            "ééééé",
            parent_fn,
            self_pid
        ));
        assert!(!is_engine_process(
            sidecar_pid,
            "アニメプロセス",
            parent_fn,
            self_pid
        ));
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
