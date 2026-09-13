use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    pub(crate) cancel_token: tokio_util::sync::CancellationToken,
    pub(crate) task_handle: tauri::async_runtime::JoinHandle<()>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    Queued,
    Running,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Processing,
    Completed,
    Skipped,
    Error,
    Cancelled,
    Planned,
}

impl From<mkv_core::pipeline::ItemOutcome> for ItemStatus {
    fn from(outcome: mkv_core::pipeline::ItemOutcome) -> Self {
        match outcome {
            mkv_core::pipeline::ItemOutcome::Completed => ItemStatus::Completed,
            mkv_core::pipeline::ItemOutcome::Skipped => ItemStatus::Skipped,
            mkv_core::pipeline::ItemOutcome::Error => ItemStatus::Error,
            mkv_core::pipeline::ItemOutcome::Cancelled => ItemStatus::Cancelled,
            mkv_core::pipeline::ItemOutcome::Planned => ItemStatus::Planned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSnapshot {
    pub id: String,
    pub file: String,
    pub status: ItemStatus,
    pub outcome: Option<mkv_core::pipeline::ItemOutcome>,
    pub duration_sec: Option<f64>,
    pub reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    pub start_time_ms: u64,
    pub duration_sec: Option<f64>,
    pub total_items: usize,
    pub completed_items: usize,
    pub items: std::collections::HashMap<String, ItemSnapshot>,
}

pub struct JobState {
    pub slot: Mutex<Option<JobSlot>>,
    pub next_id: AtomicU64,
    pub snapshot: std::sync::Mutex<Option<JobSnapshot>>,
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
            snapshot: std::sync::Mutex::new(None),
            accepting_starts: AtomicBool::new(true),
        }
    }
}

impl JobState {
    fn is_active(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.is_some())
    }

    pub fn active_pid(&self) -> Option<Pid> {
        None
    }

    pub(crate) fn release_slot(&self, job_id: u64) {
        if let Ok(mut lock) = self.slot.lock() {
            if lock.as_ref().is_some_and(|slot| slot.id == job_id) {
                *lock = None;
            }
        }
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

    pub fn update_snapshot(&self, event: &mkv_core::pipeline::JobEvent, job_id: u64, mode: &str) {
        if let Ok(mut snap) = self.snapshot.lock() {
            match event {
                mkv_core::pipeline::JobEvent::BatchStart { total, .. } => {
                    let start_time_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    *snap = Some(JobSnapshot {
                        job_id,
                        mode: mode.to_string(),
                        status: BatchStatus::Running,
                        start_time_ms,
                        duration_sec: None,
                        total_items: *total,
                        completed_items: 0,
                        items: HashMap::new(),
                    });
                }
                mkv_core::pipeline::JobEvent::ItemStart { id, file, .. } => {
                    if let Some(snapshot) = snap.as_mut() {
                        if let Some(item) = snapshot.items.get_mut(id) {
                            item.file = file.clone();
                            item.status = ItemStatus::Processing;
                        } else {
                            snapshot.items.insert(
                                id.clone(),
                                ItemSnapshot {
                                    id: id.clone(),
                                    file: file.clone(),
                                    status: ItemStatus::Processing,
                                    duration_sec: None,
                                    reason: None,
                                    error: None,
                                    outcome: None,
                                },
                            );
                        }
                    }
                }
                mkv_core::pipeline::JobEvent::ItemComplete {
                    id,
                    file,
                    status,
                    reason,
                    error,
                    duration_sec,
                    ..
                } => {
                    if let Some(snapshot) = snap.as_mut() {
                        snapshot.items.insert(
                            id.clone(),
                            ItemSnapshot {
                                id: id.clone(),
                                file: file.clone(),
                                status: (*status).into(),
                                duration_sec: *duration_sec,
                                reason: reason.clone(),
                                error: error.clone(),
                                outcome: Some(*status),
                            },
                        );
                        snapshot.completed_items += 1;
                    }
                }
                mkv_core::pipeline::JobEvent::BatchComplete {
                    success,
                    cancelled,
                    extra,
                    ..
                } => {
                    if let Some(snapshot) = snap.as_mut() {
                        snapshot.status = if *cancelled {
                            BatchStatus::Cancelled
                        } else if *success {
                            BatchStatus::Completed
                        } else {
                            BatchStatus::Error
                        };
                        if *cancelled {
                            for item in snapshot.items.values_mut() {
                                if item.status == ItemStatus::Processing {
                                    item.status = ItemStatus::Cancelled;
                                    item.reason = Some("Operation cancelled".to_string());
                                }
                            }
                        } else if !*success {
                            let err_msg = extra
                                .get("error")
                                .or_else(|| extra.get("reason"))
                                .or_else(|| extra.get("panic"))
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "Batch failed".to_string());
                            let reason_msg = extra
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string);
                            for item in snapshot.items.values_mut() {
                                if item.status == ItemStatus::Processing {
                                    item.status = ItemStatus::Error;
                                    item.error = Some(err_msg.clone());
                                    if reason_msg.is_some() {
                                        item.reason = reason_msg.clone();
                                    }
                                }
                            }
                        }
                        let duration_sec = extra
                            .get("duration_sec")
                            .and_then(|v| v.as_f64())
                            .or_else(|| {
                                if snapshot.start_time_ms > 0 {
                                    let now_ms = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                    if now_ms >= snapshot.start_time_ms {
                                        Some((now_ms - snapshot.start_time_ms) as f64 / 1000.0)
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            });
                        snapshot.duration_sec = duration_sec;
                    }
                }
                mkv_core::pipeline::JobEvent::ItemProgress { .. } => {}
            }
        }
    }

    pub fn clear_snapshot(&self) {
        if let Ok(mut snap) = self.snapshot.lock() {
            *snap = None;
        }
    }

    pub fn cancel_snapshot_if_running(&self) {
        if let Ok(mut snap) = self.snapshot.lock() {
            if let Some(snapshot) = snap.as_mut() {
                if snapshot.status == BatchStatus::Running {
                    snapshot.status = BatchStatus::Cancelled;
                    for item in snapshot.items.values_mut() {
                        if item.status == ItemStatus::Processing {
                            item.status = ItemStatus::Cancelled;
                            item.reason = Some("Operation cancelled".to_string());
                        }
                    }
                }
            }
        }
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
    let target = {
        let mut lock = state.slot.lock().map_err(|e| e.to_string())?;
        match &mut *lock {
            Some(slot) if expected_id.is_some_and(|id| id != slot.id) => return Ok(()),
            Some(slot) => {
                slot.stopping = true;
                slot.cancel_token.cancel();
                Some(slot.id)
            }
            None => None,
        }
    };

    let Some(target_id) = target else {
        return Ok(());
    };

    // Phase 1: Wait up to timeout for the task to finish cooperatively and release the slot.
    if state.wait_until_released(target_id, timeout).is_err() {
        // Phase 2: If unreleased after timeout, call slot.task_handle.abort().
        {
            let lock = state.slot.lock().map_err(|e| e.to_string())?;
            if let Some(slot) = lock.as_ref() {
                if slot.id == target_id {
                    slot.task_handle.abort();
                }
            }
        }
        // Brief grace period for abort cleanup
        let _ = state.wait_until_released(target_id, std::time::Duration::from_millis(200));
    }
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
        .and_then(|()| {
            if job_state.is_active() {
                job_state.wait_until_empty(std::time::Duration::from_millis(50))
            } else {
                Ok(())
            }
        })
        .err();

    // Release all slots if still present
    if job_error.is_some() {
        if let Ok(mut lock) = job_state.slot.lock() {
            if let Some(slot) = lock.as_mut() {
                slot.task_handle.abort();
            }
            *lock = None;
        }
    }
    if scan_error.is_some() {
        if let Ok(mut lock) = scan_state.active.lock() {
            *lock = None;
        }
    }

    combine_errors([
        scan_error.map(|error| format!("Directory inspection cleanup failed: {error}")),
        job_error.map(|error| format!("Job cleanup failed: {error}")),
    ])
}

pub fn begin_shutdown(job_state: &JobState, scan_state: &ScanState) -> bool {
    job_state.accepting_starts.store(false, Ordering::SeqCst);
    scan_state.accepting_starts.store(false, Ordering::SeqCst);
    if let Ok(mut lock) = job_state.slot.lock() {
        if let Some(slot) = lock.as_mut() {
            slot.stopping = true;
            slot.cancel_token.cancel();
        }
    }
    if let Ok(mut lock) = scan_state.active.lock() {
        if let Some(scan) = lock.as_mut() {
            if let Some(token) = &scan.cancel_token {
                token.cancel();
            }
        }
    }
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
        assert!(state.snapshot.lock().unwrap().is_none());
        assert_eq!(state.next_id.load(Ordering::SeqCst), 1);
        let id1 = state.next_id.fetch_add(1, Ordering::SeqCst);
        let id2 = state.next_id.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(state.next_id.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_job_state_snapshot_lifecycle() {
        let state = JobState::default();
        assert!(state.snapshot.lock().unwrap().is_none());

        let start_event = mkv_core::pipeline::JobEvent::BatchStart {
            version: 1,
            total: 2,
            input_dir: "/test".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&start_event, 10, "remux");
        {
            let snap_guard = state.snapshot.lock().unwrap();
            let snap = snap_guard.as_ref().unwrap();
            assert_eq!(snap.job_id, 10);
            assert_eq!(snap.mode, "remux");
            assert_eq!(snap.status, BatchStatus::Running);
            assert_eq!(snap.total_items, 2);
            assert_eq!(snap.completed_items, 0);
            assert!(snap.items.is_empty());
        }

        let item_start = mkv_core::pipeline::JobEvent::ItemStart {
            version: 1,
            id: "ep1".to_string(),
            file: "ep1.mkv".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&item_start, 10, "remux");
        {
            let snap_guard = state.snapshot.lock().unwrap();
            let snap = snap_guard.as_ref().unwrap();
            assert_eq!(snap.items.len(), 1);
            let item = snap.items.get("ep1").unwrap();
            assert_eq!(item.status, ItemStatus::Processing);
        }

        let item_complete = mkv_core::pipeline::JobEvent::ItemComplete {
            version: 1,
            id: "ep1".to_string(),
            file: "ep1.mkv".to_string(),
            status: mkv_core::pipeline::ItemOutcome::Completed,
            reason: None,
            error: None,
            duration_sec: Some(42.5),
            extra: Default::default(),
        };
        state.update_snapshot(&item_complete, 10, "remux");
        {
            let snap_guard = state.snapshot.lock().unwrap();
            let snap = snap_guard.as_ref().unwrap();
            assert_eq!(snap.completed_items, 1);
            let item = snap.items.get("ep1").unwrap();
            assert_eq!(item.status, ItemStatus::Completed);
            assert_eq!(item.duration_sec, Some(42.5));
            assert_eq!(
                item.outcome,
                Some(mkv_core::pipeline::ItemOutcome::Completed)
            );
        }

        let batch_complete = mkv_core::pipeline::JobEvent::BatchComplete {
            version: 1,
            success: true,
            cancelled: false,
            extra: Default::default(),
        };
        state.update_snapshot(&batch_complete, 10, "remux");
        {
            let snap_guard = state.snapshot.lock().unwrap();
            let snap = snap_guard.as_ref().unwrap();
            assert_eq!(snap.status, BatchStatus::Completed);
            assert!(snap.duration_sec.is_some());
        }

        state.clear_snapshot();
        assert!(state.snapshot.lock().unwrap().is_none());
    }

    #[test]
    fn test_item_status_serde_and_from() {
        assert_eq!(
            serde_json::to_string(&ItemStatus::Processing).unwrap(),
            "\"processing\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::Skipped).unwrap(),
            "\"skipped\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::Error).unwrap(),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::Planned).unwrap(),
            "\"planned\""
        );

        assert_eq!(
            ItemStatus::from(mkv_core::pipeline::ItemOutcome::Completed),
            ItemStatus::Completed
        );
        assert_eq!(
            ItemStatus::from(mkv_core::pipeline::ItemOutcome::Skipped),
            ItemStatus::Skipped
        );
        assert_eq!(
            ItemStatus::from(mkv_core::pipeline::ItemOutcome::Error),
            ItemStatus::Error
        );
        assert_eq!(
            ItemStatus::from(mkv_core::pipeline::ItemOutcome::Cancelled),
            ItemStatus::Cancelled
        );
        assert_eq!(
            ItemStatus::from(mkv_core::pipeline::ItemOutcome::Planned),
            ItemStatus::Planned
        );
    }

    #[test]
    fn test_clear_snapshot_safely_clears_without_panic() {
        let state = JobState::default();
        // Clearing when already None does not panic
        state.clear_snapshot();
        assert!(state.snapshot.lock().unwrap().is_none());

        // Populate a snapshot
        let start_event = mkv_core::pipeline::JobEvent::BatchStart {
            version: 1,
            total: 1,
            input_dir: "/test".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&start_event, 42, "remux");
        assert!(state.snapshot.lock().unwrap().is_some());

        // Clear existing snapshot safely
        state.clear_snapshot();
        assert!(state.snapshot.lock().unwrap().is_none());
    }

    #[test]
    fn test_cancel_snapshot_if_running_transitions_processing_items() {
        let state = JobState::default();
        let start_event = mkv_core::pipeline::JobEvent::BatchStart {
            version: 1,
            total: 2,
            input_dir: "/test".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&start_event, 10, "remux");

        let item1_start = mkv_core::pipeline::JobEvent::ItemStart {
            version: 1,
            id: "ep1".to_string(),
            file: "ep1.mkv".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&item1_start, 10, "remux");

        let item2_start = mkv_core::pipeline::JobEvent::ItemStart {
            version: 1,
            id: "ep2".to_string(),
            file: "ep2.mkv".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&item2_start, 10, "remux");

        let item1_complete = mkv_core::pipeline::JobEvent::ItemComplete {
            version: 1,
            id: "ep1".to_string(),
            file: "ep1.mkv".to_string(),
            status: mkv_core::pipeline::ItemOutcome::Completed,
            reason: None,
            error: None,
            duration_sec: Some(15.2),
            extra: Default::default(),
        };
        state.update_snapshot(&item1_complete, 10, "remux");

        state.cancel_snapshot_if_running();

        let snap_guard = state.snapshot.lock().unwrap();
        let snap = snap_guard.as_ref().unwrap();
        assert_eq!(snap.status, BatchStatus::Cancelled);

        let ep1 = snap.items.get("ep1").unwrap();
        assert_eq!(ep1.status, ItemStatus::Completed);
        assert_eq!(ep1.duration_sec, Some(15.2));

        let ep2 = snap.items.get("ep2").unwrap();
        assert_eq!(ep2.status, ItemStatus::Cancelled);
        assert_eq!(ep2.reason.as_deref(), Some("Operation cancelled"));
    }

    #[test]
    fn test_update_snapshot_batch_complete_transitions_processing_items() {
        // Test cancelled batch complete
        let state = JobState::default();
        let start_event = mkv_core::pipeline::JobEvent::BatchStart {
            version: 1,
            total: 1,
            input_dir: "/test".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&start_event, 1, "remux");
        let item_start = mkv_core::pipeline::JobEvent::ItemStart {
            version: 1,
            id: "ep1".to_string(),
            file: "ep1.mkv".to_string(),
            extra: Default::default(),
        };
        state.update_snapshot(&item_start, 1, "remux");

        let batch_cancel = mkv_core::pipeline::JobEvent::BatchComplete {
            version: 1,
            success: false,
            cancelled: true,
            extra: Default::default(),
        };
        state.update_snapshot(&batch_cancel, 1, "remux");

        {
            let snap = state.snapshot.lock().unwrap();
            let snap = snap.as_ref().unwrap();
            assert_eq!(snap.status, BatchStatus::Cancelled);
            let ep1 = snap.items.get("ep1").unwrap();
            assert_eq!(ep1.status, ItemStatus::Cancelled);
            assert_eq!(ep1.reason.as_deref(), Some("Operation cancelled"));
        }

        // Test error batch complete
        let state2 = JobState::default();
        state2.update_snapshot(&start_event, 2, "remux");
        state2.update_snapshot(&item_start, 2, "remux");
        let mut err_extra = serde_json::Map::new();
        err_extra.insert(
            "error".to_string(),
            serde_json::Value::String("Fatal error".to_string()),
        );
        let batch_err = mkv_core::pipeline::JobEvent::BatchComplete {
            version: 1,
            success: false,
            cancelled: false,
            extra: err_extra,
        };
        state2.update_snapshot(&batch_err, 2, "remux");

        {
            let snap = state2.snapshot.lock().unwrap();
            let snap = snap.as_ref().unwrap();
            assert_eq!(snap.status, BatchStatus::Error);
            let ep1 = snap.items.get("ep1").unwrap();
            assert_eq!(ep1.status, ItemStatus::Error);
            assert_eq!(ep1.error.as_deref(), Some("Fatal error"));
        }
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) fn dummy_slot(id: u64, stopping: bool) -> JobSlot {
        JobSlot {
            id,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            task_handle: tauri::async_runtime::spawn(async {}),
            stopping,
        }
    }

    #[test]
    fn test_job_slot_transitions() {
        let mut slot = dummy_slot(42, false);
        assert_eq!(slot.id, 42);
        assert!(!slot.stopping);
        slot.stopping = true;
        assert!(slot.stopping);
    }

    #[test]
    fn test_wait_for_job_slot_release_waits_for_cleanup() {
        let state = Arc::new(JobState::default());
        *state.slot.lock().unwrap() = Some(dummy_slot(42, true));

        let cleanup_state = Arc::clone(&state);
        let cleanup = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(100));
            let _ = cleanup_state.slot.lock().unwrap().take();
        });

        assert!(state
            .wait_until_released(42, std::time::Duration::from_secs(1))
            .is_ok());
        cleanup.join().unwrap();
    }

    #[test]
    fn test_wait_for_job_slot_release_times_out_while_owned() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42, true));

        let result = state.wait_until_released(42, std::time::Duration::from_millis(10));
        assert!(result.unwrap_err().contains("Timed out"));
        let _ = state.slot.lock().unwrap().take();
    }

    #[test]
    fn test_stale_stop_job_does_not_stop_active_scan() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        *job_state.slot.lock().unwrap() = Some(dummy_slot(7, false));

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

        let _ = job_state.slot.lock().unwrap().take();
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
        *job_state.slot.lock().unwrap() = Some(dummy_slot(7, false));

        let error = stop_job_inner(&job_state, &scan_state, Some(7)).unwrap_err();
        assert!(error.contains("poisoned"));
        // The job stop still ran despite the scan failure: the slot is stopping.
        assert!(job_state
            .slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|slot| slot.stopping));

        let _ = job_state.slot.lock().unwrap().take();
    }

    #[test]
    fn test_stop_job_internal_succeeds_even_if_slot_release_times_out() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42, true));

        let result =
            stop_job_internal_with_timeout(&state, None, std::time::Duration::from_millis(50));
        assert!(result.is_ok());

        let _ = state.slot.lock().unwrap().take();
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

        *job_state.slot.lock().unwrap() = Some(job_tests::dummy_slot(42, true));

        let error = cleanup_owned_processes_with_timeout(
            &job_state,
            &scan_state,
            std::time::Duration::from_millis(50),
        )
        .unwrap_err();

        assert!(error.contains("Timed out"));
        assert!(error.contains("Job cleanup failed"));

        let _ = job_state.slot.lock().unwrap().take();
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
