use std::process::Child;
#[cfg(windows)]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use sysinfo::Pid;
#[cfg(windows)]
use sysinfo::{ProcessesToUpdate, System};
use tauri::State;

#[allow(dead_code)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveScan {
    pub(crate) id: u64,
    pub(crate) pid: u32,
    pub(crate) cancel_token: Option<tokio_util::sync::CancellationToken>,
}

pub struct ScanState {
    pub(crate) active: Mutex<Option<ActiveScan>>,
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

    pub fn active_pid(&self) -> Option<Pid> {
        self.active
            .lock()
            .ok()?
            .as_ref()
            .map(|scan| Pid::from_u32(scan.pid))
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
                .as_ref()
                .is_some_and(|scan| scan.id == scan_id))
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
            if active.as_ref().is_some_and(|scan| scan.id == self.id) {
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

    pub fn active_pid(&self) -> Option<Pid> {
        self.slot
            .lock()
            .ok()?
            .as_ref()
            .map(|slot| Pid::from_u32(slot.child.id()))
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
    let target = state.active.lock().map_err(|e| e.to_string())?.clone();
    let Some(scan) = target else {
        return Ok(());
    };
    let scan_id = scan.id;
    let pid = scan.pid;

    if let Some(token) = scan.cancel_token {
        token.cancel();
    }

    if pid == 0 {
        let mut active = state.active.lock().map_err(|e| e.to_string())?;
        if active
            .as_ref()
            .is_some_and(|target| target.id == scan_id && target.pid == 0)
        {
            *active = None;
        }
        return Ok(());
    }
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
                .is_some_and(|target| target.id != scan_id || target.pid != pid)
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
            .as_ref()
            .is_some_and(|scan| scan.id == scan_id)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    use std::process::Command;
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

        *scan_state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            pid: 99999,
            cancel_token: None,
        });

        let result = stop_job_inner(&job_state, &scan_state, Some(99));
        assert!(result.is_ok());
        assert_eq!(
            *scan_state.active.lock().unwrap(),
            Some(ActiveScan {
                id: 1,
                pid: 99999,
                cancel_token: None,
            })
        );

        let empty_job_state = JobState::default();
        let result_empty = stop_job_inner(&empty_job_state, &scan_state, Some(99));
        assert!(result_empty.is_ok());
        assert_eq!(
            *scan_state.active.lock().unwrap(),
            Some(ActiveScan {
                id: 1,
                pid: 99999,
                cancel_token: None,
            })
        );

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
    use std::process::Command;
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

    #[test]
    fn test_stop_registered_scan_cancels_token() {
        let state = ScanState::default();
        let token = tokio_util::sync::CancellationToken::new();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            pid: 0,
            cancel_token: Some(token.clone()),
        });

        assert!(!token.is_cancelled());
        assert!(stop_registered_scan(&state).is_ok());
        assert!(token.is_cancelled());
        assert!(!state.is_active());
    }

    #[test]
    fn test_scan_registration_drop_clears_active_scan() {
        let state = ScanState::default();
        let token = tokio_util::sync::CancellationToken::new();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            pid: 0,
            cancel_token: Some(token.clone()),
        });

        {
            let _registration = ScanRegistration {
                state: &state,
                id: 1,
            };
        }

        assert!(!state.is_active());
        assert!(!token.is_cancelled());
    }
}

#[cfg(all(test, unix))]
mod unix_scan_tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::Arc;

    #[test]
    fn test_stop_scan_waits_until_process_is_reaped_and_registration_clears() {
        let state = Arc::new(ScanState::default());
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            pid,
            cancel_token: None,
        });

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
    use std::os::windows::process::CommandExt;
    use std::process::Command;

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
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            pid,
            cancel_token: None,
        });

        assert!(stop_registered_scan(&state).is_ok());
        assert!(!state.is_active());
    }
}
