use mkv_core::pipeline::{CancellationToken, ItemOutcome, JobEvent, CANCELLED_REASON};
use mkv_core::request::EffectiveSettings;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

pub struct JobSlot {
    pub(crate) id: u64,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) pipeline_abort: tokio::task::AbortHandle,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    Running,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Queued,
    Processing,
    Completed,
    Skipped,
    Error,
    Cancelled,
    Planned,
}

impl ItemStatus {
    /// Whether the item has a final outcome. `Planned` is the dry-run outcome,
    /// not a pending state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ItemStatus::Completed
                | ItemStatus::Skipped
                | ItemStatus::Error
                | ItemStatus::Cancelled
                | ItemStatus::Planned
        )
    }
}

impl From<ItemOutcome> for ItemStatus {
    fn from(outcome: ItemOutcome) -> Self {
        match outcome {
            ItemOutcome::Completed => ItemStatus::Completed,
            ItemOutcome::Skipped => ItemStatus::Skipped,
            ItemOutcome::Error => ItemStatus::Error,
            ItemOutcome::Cancelled => ItemStatus::Cancelled,
            ItemOutcome::Planned => ItemStatus::Planned,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemSnapshot {
    pub id: String,
    pub file: String,
    pub status: ItemStatus,
    pub outcome: Option<ItemOutcome>,
    pub progress: Option<f64>,
    pub start_time_ms: Option<u64>,
    pub duration_sec: Option<f64>,
    pub reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    pub start_time_ms: u64,
    pub duration_sec: Option<f64>,
    pub total_items: usize,
    pub completed_items: usize,
    pub input_dir: Option<String>,
    /// What the job runs with, fixed when it starts. `None` only for a snapshot that no
    /// `begin_snapshot` preceded.
    pub effective_settings: Option<EffectiveSettings>,
    pub ordered_item_ids: Vec<String>,
    pub items: HashMap<String, ItemSnapshot>,
}

pub struct JobState {
    pub slot: Mutex<Option<JobSlot>>,
    pub next_id: AtomicU64,
    pub snapshot: Mutex<Option<JobSnapshot>>,
    accepting_starts: AtomicBool,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
            next_id: AtomicU64::new(1),
            snapshot: Mutex::new(None),
            accepting_starts: AtomicBool::new(true),
        }
    }
}

pub(crate) struct ActiveScan {
    pub(crate) id: u64,
    pub(crate) cancel_token: CancellationToken,
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

    pub(crate) fn ensure_accepting_starts(&self) -> Result<(), String> {
        if self.accepting_starts.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Application is closing; no new directory inspection can start.".into())
        }
    }

    fn wait_until_empty(&self, timeout: Duration) -> Result<(), String> {
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

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl JobState {
    pub fn is_active(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.is_some())
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
        timeout: Duration,
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

    fn wait_until_empty(&self, timeout: Duration) -> Result<(), String> {
        wait_until(timeout, "job", || {
            Ok(self.slot.lock().map_err(|e| e.to_string())?.is_none())
        })
    }

    pub(crate) fn begin_snapshot(
        &self,
        job_id: u64,
        mode: &str,
        input_dir: String,
        effective_settings: EffectiveSettings,
    ) -> Result<(), String> {
        *self.snapshot.lock().map_err(|error| error.to_string())? = Some(JobSnapshot {
            job_id,
            mode: mode.to_string(),
            status: BatchStatus::Running,
            start_time_ms: timestamp_ms(),
            duration_sec: None,
            total_items: 0,
            completed_items: 0,
            input_dir: Some(input_dir),
            effective_settings: Some(effective_settings),
            ordered_item_ids: Vec::new(),
            items: HashMap::new(),
        });
        Ok(())
    }

    pub fn update_snapshot(&self, event: &JobEvent, job_id: u64, mode: &str) {
        if let Ok(mut snap) = self.snapshot.lock() {
            if !matches!(event, JobEvent::BatchStart { .. })
                && !snap.as_ref().is_some_and(|snapshot| {
                    snapshot.job_id == job_id && snapshot.status == BatchStatus::Running
                })
            {
                return;
            }
            match event {
                JobEvent::BatchStart { total, items, .. } => {
                    let start_time_ms = snap
                        .as_ref()
                        .filter(|snapshot| snapshot.job_id == job_id)
                        .map(|snapshot| snapshot.start_time_ms)
                        .unwrap_or_else(timestamp_ms);
                    let mut snapshot_items = HashMap::new();
                    let mut ordered_item_ids = Vec::new();
                    if let Some(items) = items {
                        for item in items {
                            if snapshot_items.contains_key(&item.id) {
                                continue;
                            }
                            ordered_item_ids.push(item.id.clone());
                            snapshot_items.insert(
                                item.id.clone(),
                                ItemSnapshot {
                                    id: item.id.clone(),
                                    file: item.file.clone(),
                                    status: ItemStatus::Queued,
                                    outcome: None,
                                    progress: None,
                                    start_time_ms: None,
                                    duration_sec: None,
                                    reason: None,
                                    error: None,
                                },
                            );
                        }
                    }
                    // Only this job's own snapshot, begun by `run_job`, carries over.
                    let own = snap.as_ref().filter(|snapshot| snapshot.job_id == job_id);
                    *snap = Some(JobSnapshot {
                        job_id,
                        mode: mode.to_string(),
                        status: BatchStatus::Running,
                        start_time_ms,
                        duration_sec: None,
                        total_items: *total,
                        completed_items: 0,
                        input_dir: own.and_then(|snapshot| snapshot.input_dir.clone()),
                        effective_settings: own
                            .and_then(|snapshot| snapshot.effective_settings.clone()),
                        ordered_item_ids,
                        items: snapshot_items,
                    });
                }
                JobEvent::ItemStart { id, file, .. } => {
                    if let Some(item) = snap
                        .as_mut()
                        .and_then(|snapshot| snapshot.items.get_mut(id))
                    {
                        if !item.status.is_terminal() {
                            item.file = file.clone();
                            item.status = ItemStatus::Processing;
                            item.start_time_ms.get_or_insert_with(timestamp_ms);
                        }
                    }
                }
                JobEvent::ItemProgress { id, progress, .. } => {
                    if let Some(item) = snap
                        .as_mut()
                        .and_then(|snapshot| snapshot.items.get_mut(id))
                    {
                        if item.status == ItemStatus::Processing && progress.is_finite() {
                            item.progress = Some(progress.clamp(0.0, 100.0));
                        }
                    }
                }
                JobEvent::ItemComplete {
                    id,
                    file,
                    status,
                    reason,
                    error,
                    duration_sec,
                    ..
                } => {
                    if let Some(snapshot) = snap.as_mut() {
                        if let Some(item) = snapshot.items.get_mut(id) {
                            if !item.status.is_terminal() {
                                item.file = file.clone();
                                item.status = (*status).into();
                                item.outcome = Some(*status);
                                item.duration_sec = *duration_sec;
                                item.reason = reason.clone();
                                item.error = error.clone();
                                snapshot.completed_items += 1;
                            }
                        }
                    }
                }
                JobEvent::BatchComplete {
                    success,
                    cancelled,
                    extra,
                    ..
                } => {
                    if let Some(snapshot) = snap.as_mut() {
                        let has_non_terminal = snapshot
                            .items
                            .values()
                            .any(|item| !item.status.is_terminal());
                        snapshot.status = terminal_status(*success, *cancelled, has_non_terminal);
                        settle_open_items(snapshot, *success, *cancelled, extra);
                        snapshot.duration_sec =
                            extra.get("duration_sec").and_then(|value| value.as_f64());
                    }
                }
            }
        }
    }

    pub fn clear_snapshot(&self) {
        if self.is_active() {
            return;
        }
        if let Ok(mut snap) = self.snapshot.lock() {
            *snap = None;
        }
    }
}

fn terminal_status(success: bool, cancelled: bool, has_non_terminal_items: bool) -> BatchStatus {
    if cancelled {
        BatchStatus::Cancelled
    } else if success && !has_non_terminal_items {
        BatchStatus::Completed
    } else {
        BatchStatus::Error
    }
}

fn settle_open_items(
    snapshot: &mut JobSnapshot,
    success: bool,
    cancelled: bool,
    extra: &serde_json::Map<String, serde_json::Value>,
) {
    let text = |key: &str| extra.get(key).and_then(|value| value.as_str());
    let reason = text("reason");
    let error_message = text("error")
        .or(reason)
        .or_else(|| text("panic"))
        .unwrap_or("Batch failed");
    for item in snapshot.items.values_mut() {
        if item.status.is_terminal() {
            continue;
        }
        snapshot.completed_items += 1;
        if cancelled {
            item.status = ItemStatus::Cancelled;
            item.outcome = Some(ItemOutcome::Cancelled);
            item.reason = Some(CANCELLED_REASON.to_string());
        } else {
            item.status = ItemStatus::Error;
            item.outcome = Some(ItemOutcome::Error);
            item.error = Some(if success {
                "Protocol error: missing completion event".to_string()
            } else {
                error_message.to_string()
            });
            item.reason = reason.map(ToString::to_string);
        }
    }
}

/// Default bound on waiting for owned-process teardown to finish.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

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
    let scan_error = stop_scan(scan_state).err();
    let job_error = stop_job_internal(state, job_id, STOP_TIMEOUT).err();
    combine_errors([scan_error, job_error])
}

#[tauri::command]
pub async fn stop_job(app: AppHandle, job_id: Option<u64>) -> Result<(), String> {
    run_teardown(app, move |state, scan_state| {
        stop_job_inner(state, scan_state, job_id)
    })
    .await
}

/// Runs a sync teardown on the blocking pool. A sync command would run it on the main thread and
/// freeze the window; a plain async one would park a Tokio worker the supervisor needs. The
/// states are read through `app` because `State<'_>` is not `'static`.
pub(crate) async fn run_teardown<R: Runtime>(
    app: AppHandle<R>,
    teardown: impl FnOnce(&JobState, &ScanState) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        teardown(&app.state::<JobState>(), &app.state::<ScanState>())
    })
    .await
    .map_err(|error| format!("Async runtime error: {error}"))?
}

fn stop_scan(state: &ScanState) -> Result<(), String> {
    let _start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    stop_registered_scan(state)
}

pub(crate) fn stop_registered_scan(state: &ScanState) -> Result<(), String> {
    let mut active = state.active.lock().map_err(|e| e.to_string())?;
    if let Some(scan) = active.take() {
        scan.cancel_token.cancel();
    }
    Ok(())
}

pub(crate) fn stop_job_internal(
    state: &JobState,
    expected_id: Option<u64>,
    timeout: Duration,
) -> Result<(), String> {
    let target = {
        let mut lock = state.slot.lock().map_err(|e| e.to_string())?;
        match &mut *lock {
            Some(slot) if expected_id.is_some_and(|id| id != slot.id) => return Ok(()),
            Some(slot) => {
                slot.cancel_token.cancel();
                Some(slot.id)
            }
            None => None,
        }
    };

    let Some(target_id) = target else {
        return Ok(());
    };

    // The supervisor must survive the worker abort to publish terminal events and release the slot.
    if state.wait_until_released(target_id, timeout).is_err() {
        let lock = state.slot.lock().map_err(|error| error.to_string())?;
        if let Some(slot) = lock.as_ref().filter(|slot| slot.id == target_id) {
            slot.pipeline_abort.abort();
        }
        drop(lock);
        return state.wait_until_released(target_id, Duration::from_secs(1));
    }
    Ok(())
}

fn wait_until(
    timeout: Duration,
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
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn cleanup_owned_processes(job_state: &JobState, scan_state: &ScanState) -> Result<(), String> {
    cleanup_owned_processes_with_timeout(job_state, scan_state, STOP_TIMEOUT)
}

pub(crate) fn cleanup_owned_processes_with_timeout(
    job_state: &JobState,
    scan_state: &ScanState,
    timeout: Duration,
) -> Result<(), String> {
    // Both stops run before either wait; a failed stop already reports why, so skip its wait.
    let scan_stop = stop_scan(scan_state);
    let job_stop = stop_job_internal(job_state, None, timeout);
    let scan_error = scan_stop
        .and_then(|()| scan_state.wait_until_empty(timeout))
        .err();
    let job_error = job_stop
        .and_then(|()| {
            if job_state.is_active() {
                job_state.wait_until_empty(Duration::from_millis(50))
            } else {
                Ok(())
            }
        })
        .err();

    combine_errors([
        scan_error.map(|error| format!("Directory inspection cleanup failed: {error}")),
        job_error.map(|error| format!("Job cleanup failed: {error}")),
    ])
}

pub fn begin_shutdown(job_state: &JobState, scan_state: &ScanState) -> bool {
    job_state.accepting_starts.store(false, Ordering::SeqCst);
    scan_state.accepting_starts.store(false, Ordering::SeqCst);
    if let Ok(lock) = job_state.slot.lock() {
        if let Some(slot) = lock.as_ref() {
            slot.cancel_token.cancel();
        }
    }
    if let Ok(lock) = scan_state.active.lock() {
        if let Some(scan) = lock.as_ref() {
            scan.cancel_token.cancel();
        }
    }
    let scan_starting = scan_state.start_lock.try_lock().is_err();
    scan_starting || scan_state.is_active() || job_state.is_active()
}

pub fn cancel_shutdown(job_state: &JobState, scan_state: &ScanState) {
    job_state.accepting_starts.store(true, Ordering::SeqCst);
    scan_state.accepting_starts.store(true, Ordering::SeqCst);
}

/// Allowed while a Stop is pending: both cancel the same token and wait on the same slot, and a
/// second abort is a no-op.
#[tauri::command]
pub async fn close_window(window: tauri::Window, app: AppHandle) -> Result<(), String> {
    // Refuse starts before the blocking-pool hop, so a `run_job` that lands while the pool
    // schedules the teardown is refused instead of starting a job the teardown then cancels. One
    // async hop remains: this body runs on a Tokio task, not at dispatch.
    let (state, scan_state) = (app.state::<JobState>(), app.state::<ScanState>());
    begin_shutdown(&state, &scan_state);
    run_teardown(app.clone(), cleanup_owned_processes)
        .await
        .and_then(|()| window.close().map_err(|error| error.to_string()))
        .inspect_err(|_| cancel_shutdown(&state, &scan_state))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::JobSlot;
    use mkv_core::pipeline::{BatchItemInfo, CancellationToken, ItemOutcome, JobEvent};
    use mkv_core::request::EffectiveSettings;

    pub(crate) fn dummy_slot(id: u64) -> JobSlot {
        JobSlot {
            id,
            cancel_token: CancellationToken::new(),
            pipeline_abort: tauri::async_runtime::spawn(async {}).inner().abort_handle(),
        }
    }

    /// Effective Settings that differ from every default, so a test sees them pass through
    /// unchanged.
    pub(crate) fn effective_settings() -> EffectiveSettings {
        EffectiveSettings {
            workers: 3,
            io_jobs: 5,
            ffmpeg_threads: 2,
            bitrate: "256k".into(),
            complexity: 8,
        }
    }

    /// A `BatchStart` listing `ids` in engine order; each item's file is `<id>.mkv`.
    pub(crate) fn batch_start(ids: &[&str]) -> JobEvent {
        JobEvent::BatchStart {
            version: 1,
            total: ids.len(),
            input_dir: "/anime".into(),
            items: Some(
                ids.iter()
                    .map(|id| BatchItemInfo {
                        id: (*id).into(),
                        file: format!("{id}.mkv"),
                    })
                    .collect(),
            ),
            extra: Default::default(),
        }
    }

    pub(crate) fn item_start(id: &str) -> JobEvent {
        JobEvent::ItemStart {
            version: 1,
            id: id.into(),
            file: format!("{id}.mkv"),
            extra: Default::default(),
        }
    }

    pub(crate) fn item_progress(id: &str, progress: f64) -> JobEvent {
        JobEvent::ItemProgress {
            version: 1,
            id: id.into(),
            file: format!("{id}.mkv"),
            progress,
            speed: None,
            eta: None,
            extra: Default::default(),
        }
    }

    pub(crate) fn item_complete(id: &str, duration_sec: f64) -> JobEvent {
        JobEvent::ItemComplete {
            version: 1,
            id: id.into(),
            file: format!("{id}.mkv"),
            status: ItemOutcome::Completed,
            reason: None,
            error: None,
            duration_sec: Some(duration_sec),
            extra: Default::default(),
        }
    }

    pub(crate) fn batch_complete(success: bool, cancelled: bool) -> JobEvent {
        JobEvent::BatchComplete {
            version: 1,
            success,
            cancelled,
            extra: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn snapshot(state: &JobState) -> JobSnapshot {
        state.snapshot.lock().unwrap().clone().unwrap()
    }

    fn poison<T: Send>(mutex: &Mutex<T>) {
        let _ = thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = mutex.lock().unwrap();
                    panic!("poison {}", std::any::type_name::<T>());
                })
                .join()
        });
    }

    #[test]
    fn test_job_state_snapshot_lifecycle() {
        let state = JobState::default();
        state.update_snapshot(&batch_start(&["ep1"]), 10, "remux");
        let snap = snapshot(&state);
        assert_eq!(snap.job_id, 10);
        assert_eq!(snap.mode, "remux");
        assert_eq!(snap.status, BatchStatus::Running);
        assert_eq!(snap.total_items, 1);
        assert_eq!(snap.completed_items, 0);
        assert_eq!(snap.ordered_item_ids, vec!["ep1".to_string()]);

        state.update_snapshot(&item_start("ep1"), 10, "remux");
        assert_eq!(snapshot(&state).items["ep1"].status, ItemStatus::Processing);

        let complete = item_complete("ep1", 42.5);
        state.update_snapshot(&complete, 10, "remux");
        let snap = snapshot(&state);
        assert_eq!(snap.completed_items, 1);
        let item = &snap.items["ep1"];
        assert_eq!(item.status, ItemStatus::Completed);
        assert_eq!(item.duration_sec, Some(42.5));
        assert_eq!(item.outcome, Some(ItemOutcome::Completed));

        // A duplicate completion must not count the item twice.
        state.update_snapshot(&complete, 10, "remux");
        assert_eq!(snapshot(&state).completed_items, 1);

        state.update_snapshot(&batch_complete(true, false), 10, "remux");
        let snap = snapshot(&state);
        assert_eq!(snap.status, BatchStatus::Completed);
        assert_eq!(snap.duration_sec, None);

        state.clear_snapshot();
        assert!(state.snapshot.lock().unwrap().is_none());
    }

    #[test]
    fn test_snapshot_carries_the_effective_settings_of_its_job() {
        let state = JobState::default();
        let settings = Some(effective_settings());
        state
            .begin_snapshot(7, "remux", "/anime".into(), effective_settings())
            .unwrap();
        assert_eq!(snapshot(&state).effective_settings, settings);

        // Neither the engine's BatchStart nor the terminal event replaces them.
        state.update_snapshot(&batch_start(&["ep1"]), 7, "remux");
        assert_eq!(snapshot(&state).effective_settings, settings);
        state.update_snapshot(&batch_complete(true, false), 7, "remux");
        assert_eq!(snapshot(&state).effective_settings, settings);

        let json = serde_json::to_value(snapshot(&state)).unwrap();
        assert_eq!(json["effective_settings"]["io_jobs"], 5);
        assert_eq!(json["effective_settings"]["bitrate"], "256k");

        // A later job inherits neither them nor the input directory from the snapshot it
        // replaces.
        state.update_snapshot(&batch_start(&["ep1"]), 8, "remux");
        assert_eq!(snapshot(&state).effective_settings, None);
        assert_eq!(snapshot(&state).input_dir, None);
    }

    #[test]
    fn test_batch_complete_success_transitions_lingering_queued_items_to_error() {
        let state = JobState::default();
        state.update_snapshot(&batch_start(&["ep1", "ep2"]), 10, "remux");
        state.update_snapshot(&item_complete("ep1", 10.0), 10, "remux");
        state.update_snapshot(&batch_complete(true, false), 10, "remux");

        let snap = snapshot(&state);
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Completed);
        let ep2 = &snap.items["ep2"];
        assert_eq!(ep2.status, ItemStatus::Error);
        assert_eq!(
            ep2.error.as_deref(),
            Some("Protocol error: missing completion event")
        );
    }

    #[test]
    fn test_item_status_wire_names() {
        let statuses = [
            ItemStatus::Queued,
            ItemStatus::Processing,
            ItemStatus::Completed,
            ItemStatus::Skipped,
            ItemStatus::Error,
            ItemStatus::Cancelled,
            ItemStatus::Planned,
        ];
        assert_eq!(
            serde_json::to_value(statuses).unwrap(),
            serde_json::json!([
                "queued",
                "processing",
                "completed",
                "skipped",
                "error",
                "cancelled",
                "planned"
            ])
        );
    }

    #[test]
    fn test_live_snapshot_retains_only_active_order_and_clamped_progress() {
        let state = JobState::default();
        state
            .begin_snapshot(10, "remux", "/anime".into(), effective_settings())
            .unwrap();
        let start_time_ms = snapshot(&state).start_time_ms;
        state.update_snapshot(&batch_start(&["ep2", "ep1"]), 10, "remux");
        for id in ["ep3", "ep2"] {
            state.update_snapshot(&item_start(id), 10, "remux");
        }
        for progress in [150.0, -8.0, 70.0] {
            state.update_snapshot(&item_progress("ep2", progress), 10, "remux");
            assert_eq!(
                snapshot(&state).items["ep2"].progress,
                Some(progress.clamp(0.0, 100.0))
            );
        }

        let snapshot = snapshot(&state);
        assert_eq!(snapshot.start_time_ms, start_time_ms);
        assert_eq!(
            snapshot.ordered_item_ids,
            vec!["ep2".to_string(), "ep1".to_string()]
        );
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.input_dir.as_deref(), Some("/anime"));
        assert!(!snapshot.items.contains_key("ep3"));
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Processing);
        assert!(snapshot.items["ep2"].start_time_ms.is_some());
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Queued);
        assert_eq!(snapshot.items["ep1"].file, "ep1.mkv");
        assert_eq!(snapshot.items["ep1"].start_time_ms, None);
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["ordered_item_ids"], serde_json::json!(["ep2", "ep1"]));
        assert_eq!(value["items"]["ep2"]["progress"], 70.0);
        assert!(value["items"]["ep2"]["start_time_ms"].is_number());
    }

    #[test]
    fn test_update_snapshot_batch_complete_transitions_processing_items() {
        let cancelled = JobState::default();
        cancelled.update_snapshot(&batch_start(&["ep1"]), 1, "remux");
        cancelled.update_snapshot(&item_start("ep1"), 1, "remux");
        cancelled.update_snapshot(&batch_complete(false, true), 1, "remux");
        let snap = snapshot(&cancelled);
        assert_eq!(snap.status, BatchStatus::Cancelled);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Cancelled);
        assert_eq!(snap.items["ep1"].reason.as_deref(), Some(CANCELLED_REASON));

        let failed = JobState::default();
        failed.update_snapshot(&batch_start(&["ep1"]), 2, "remux");
        failed.update_snapshot(&item_start("ep1"), 2, "remux");
        let mut extra = serde_json::Map::new();
        extra.insert("error".into(), "Fatal error".into());
        extra.insert("duration_sec".into(), serde_json::json!(18.25));
        let batch_err = JobEvent::BatchComplete {
            version: 1,
            success: false,
            cancelled: false,
            extra,
        };
        failed.update_snapshot(&batch_err, 2, "remux");
        let snap = snapshot(&failed);
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.duration_sec, Some(18.25));
        assert_eq!(snap.items["ep1"].status, ItemStatus::Error);
        assert_eq!(snap.items["ep1"].error.as_deref(), Some("Fatal error"));
    }

    #[test]
    fn test_terminal_status() {
        // Cancelled takes precedence over success and non-terminal items
        assert_eq!(terminal_status(false, true, false), BatchStatus::Cancelled);
        assert_eq!(terminal_status(true, true, false), BatchStatus::Cancelled);
        assert_eq!(terminal_status(true, true, true), BatchStatus::Cancelled);
        assert_eq!(terminal_status(false, true, true), BatchStatus::Cancelled);

        // Success with complete items
        assert_eq!(terminal_status(true, false, false), BatchStatus::Completed);

        // Success with uncompleted items (downgraded to error)
        assert_eq!(terminal_status(true, false, true), BatchStatus::Error);

        // Failure
        assert_eq!(terminal_status(false, false, false), BatchStatus::Error);
        assert_eq!(terminal_status(false, false, true), BatchStatus::Error);
    }

    #[test]
    fn test_clear_snapshot_ignored_while_job_is_active() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42));
        state
            .begin_snapshot(42, "remux", "/anime".into(), effective_settings())
            .unwrap();
        state.clear_snapshot();
        assert!(state.snapshot.lock().unwrap().is_some());
    }

    #[test]
    fn test_wait_for_job_slot_release_waits_for_cleanup() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42));

        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                state.slot.lock().unwrap().take();
            });
            assert!(state
                .wait_until_released(42, Duration::from_secs(1))
                .is_ok());
        });
    }

    #[test]
    fn test_wait_for_job_slot_release_times_out_while_owned() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42));

        let result = state.wait_until_released(42, Duration::from_millis(10));
        assert!(result.unwrap_err().contains("Timed out"));
    }

    #[test]
    fn test_stale_stop_job_does_not_stop_active_scan() {
        let scan_state = ScanState::default();
        let token = CancellationToken::new();
        *scan_state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        let job_state = JobState::default();
        *job_state.slot.lock().unwrap() = Some(dummy_slot(7));
        assert!(stop_job_inner(&job_state, &scan_state, Some(99)).is_ok());
        assert!(stop_job_inner(&JobState::default(), &scan_state, Some(99)).is_ok());

        assert!(scan_state.is_active());
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_stop_job_proceeds_when_scan_teardown_fails() {
        let scan_state = ScanState::default();
        poison(&scan_state.start_lock);

        let job_state = JobState::default();
        *job_state.slot.lock().unwrap() = Some(dummy_slot(7));

        let error = stop_job_inner(&job_state, &scan_state, Some(7)).unwrap_err();
        assert!(error.contains("poisoned"));
        assert!(job_state
            .slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|slot| slot.cancel_token.is_cancelled()));
    }

    #[test]
    fn test_stop_job_internal_reports_unreleased_slot() {
        let state = JobState::default();
        *state.slot.lock().unwrap() = Some(dummy_slot(42));

        let result = stop_job_internal(&state, None, Duration::from_millis(50));
        assert!(result.unwrap_err().contains("Timed out"));
        assert!(state.is_active());
    }

    #[test]
    fn test_cleanup_attempts_scan_and_job_teardown() {
        let scan_state = ScanState::default();
        poison(&scan_state.start_lock);
        let job_state = JobState::default();
        poison(&job_state.slot);

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
        *job_state.slot.lock().unwrap() = Some(dummy_slot(42));

        let error = cleanup_owned_processes_with_timeout(
            &job_state,
            &scan_state,
            Duration::from_millis(50),
        )
        .unwrap_err();

        assert!(error.contains("Timed out"));
        assert!(error.contains("Job cleanup failed"));
    }

    #[test]
    fn test_teardown_leaves_the_calling_runtime_free_while_it_waits() {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        app.manage(ScanState::default());
        // Nothing releases this slot, so the teardown waits out both phases.
        *app.state::<JobState>().slot.lock().unwrap() = Some(dummy_slot(42));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            let mut teardown =
                std::pin::pin!(run_teardown(app.handle().clone(), |state, scan_state| {
                    cleanup_owned_processes_with_timeout(
                        state,
                        scan_state,
                        Duration::from_millis(200),
                    )
                }));
            // A teardown that blocked this thread would return before the timer could fire.
            tokio::select! {
                _ = &mut teardown => panic!("teardown returned before the runtime's timer fired"),
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
            teardown.await
        });

        assert!(result.unwrap_err().contains("Timed out"));
    }

    #[test]
    fn test_stop_registered_scan_cancels_token() {
        let state = ScanState::default();
        let token = CancellationToken::new();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        assert!(stop_registered_scan(&state).is_ok());
        assert!(token.is_cancelled());
        assert!(!state.is_active());
    }

    #[test]
    fn test_scan_registration_drop_clears_active_scan() {
        let state = ScanState::default();
        let token = CancellationToken::new();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        drop(ScanRegistration {
            state: &state,
            id: 1,
        });

        assert!(!state.is_active());
        assert!(!token.is_cancelled());
    }
}
