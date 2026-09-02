use std::process::Child;
#[cfg(windows)]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::State;

#[allow(dead_code)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: i32, action: &str) -> Result<(), String> {
    if pid == 0 {
        return Err(format!("Invalid PID 0 to {action}"));
    }
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("Failed to {action} process group"))
    }
}

#[cfg(unix)]
fn process_group_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
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
    pub(crate) id: u64,
    pub(crate) child: Child,
    pub(crate) stopping: bool,
}

pub(crate) fn reserve_job_slot(
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
    pub(crate) active: Mutex<Option<(u64, u32)>>,
    pub(crate) next_id: AtomicU64,
    pub(crate) start_lock: Mutex<()>,
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

    pub(crate) fn ensure_accepting_starts(&self) -> Result<(), String> {
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

pub(crate) struct ScanRegistration<'a> {
    pub(crate) state: &'a ScanState,
    pub(crate) id: u64,
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

    pub(crate) fn wait_until_released(
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

pub(crate) fn stop_scan_internal_with_timeout(
    state: &ScanState,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let _start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    stop_registered_scan_with_timeout(state, timeout)
}

pub(crate) fn stop_registered_scan(state: &ScanState) -> Result<(), String> {
    stop_registered_scan_with_timeout(state, STOP_TIMEOUT)
}

pub(crate) fn stop_registered_scan_with_timeout(
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
        let iterations = (timeout.as_millis() / 100).max(1);
        for _ in 0..iterations {
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

pub(crate) fn stop_job_internal_with_timeout(
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
                    #[cfg(unix)]
                    signal_process_group(slot.child.id(), libc::SIGKILL, "force-kill job")?;
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

pub(crate) fn cleanup_owned_processes_with_timeout(
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
