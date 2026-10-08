use crate::job::{wait_until, JobState};
use mkv_core::pipeline::CancellationToken;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

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
        if !state.holds_slot(expected_id)? {
            return Ok(());
        }
    }
    // Stopping the job must proceed even if scan teardown fails; report both.
    let scan_error = stop_scan(scan_state).err();
    let job_error = state.stop(job_id, STOP_TIMEOUT).err();
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
    let job_stop = job_state.stop(None, timeout);
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
    job_state.close_starts();
    scan_state.accepting_starts.store(false, Ordering::SeqCst);
    job_state.cancel_current();
    if let Ok(lock) = scan_state.active.lock() {
        if let Some(scan) = lock.as_ref() {
            scan.cancel_token.cancel();
        }
    }
    let scan_starting = scan_state.start_lock.try_lock().is_err();
    scan_starting || scan_state.is_active() || job_state.is_active()
}

pub fn cancel_shutdown(job_state: &JobState, scan_state: &ScanState) {
    job_state.open_starts();
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
mod tests {
    use super::*;
    use crate::job::test_support::{poison, RecordingSink};
    use crate::job::JobConfig;
    use crate::snapshot::test_support::effective_settings;
    use mkv_core::pipeline::PipelineSummary;
    use std::time::Instant;

    #[test]
    fn test_stale_stop_job_does_not_stop_active_scan() {
        let scan_state = ScanState::default();
        let token = CancellationToken::new();
        *scan_state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        let job_state = JobState::default();
        job_state.occupy(7);
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
        job_state.occupy(7);

        let error = stop_job_inner(&job_state, &scan_state, Some(7)).unwrap_err();
        assert!(error.contains("poisoned"));
        assert!(job_state.current_cancelled());
    }

    #[test]
    fn test_cleanup_attempts_scan_and_job_teardown() {
        let scan_state = ScanState::default();
        poison(&scan_state.start_lock);
        let job_state = JobState::default();
        job_state.poison_slot();

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
        assert!(job_state.ensure_accepting_starts().is_err());
    }

    #[test]
    fn test_cancel_shutdown_restores_accepting_starts() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();

        begin_shutdown(&job_state, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_err());
        assert!(job_state.ensure_accepting_starts().is_err());

        cancel_shutdown(&job_state, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_ok());
        assert!(job_state.ensure_accepting_starts().is_ok());
    }

    #[test]
    fn test_cleanup_fails_if_job_slot_not_released() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();
        job_state.occupy(42);

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
        app.state::<JobState>().occupy(42);

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

    /// Stop and Close race on one Job Slot through `run_teardown`, the route the commands take.
    #[test]
    fn test_close_during_a_pending_stop_settles_the_job_once_through_the_teardown() {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        app.manage(ScanState::default());
        let sink = RecordingSink::default();
        let token = CancellationToken::new();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let config = JobConfig {
            mode: "remux",
            input_dir: "/anime".into(),
            effective_settings: effective_settings(),
            cancel_token: token.clone(),
        };
        // Ignores its cancel token, so only an abort ends it. It holds the senders, as the
        // engine does, so the abort is what closes the supervisor's channels.
        let worker = async move {
            let _senders = (event_tx, log_tx);
            std::future::pending::<Result<PipelineSummary, String>>().await
        };
        let job_id = app
            .state::<JobState>()
            .start(sink.clone(), config, event_rx, log_rx, worker)
            .unwrap();

        let start = Instant::now();
        let stop = tauri::async_runtime::spawn(run_teardown(
            app.handle().clone(),
            move |state, scan_state| stop_job_inner(state, scan_state, Some(job_id)),
        ));
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(Duration::from_secs(2), token.cancelled())
                .await
                .expect("Stop never cancelled the job")
        });
        let close = run_teardown(app.handle().clone(), |state, scan_state| {
            cleanup_owned_processes_with_timeout(state, scan_state, Duration::from_millis(200))
        });

        assert_eq!(tauri::async_runtime::block_on(close), Ok(()));
        assert_eq!(tauri::async_runtime::block_on(stop).unwrap(), Ok(()));
        // Close's abort, not Stop's own deadline, ended the wait for both.
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!app.state::<JobState>().is_active());
        let terminals: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event["type"] == "batch_complete")
            .collect();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["cancelled"], true);
        let dones = sink.dones();
        assert_eq!(dones.len(), 1);
        assert_eq!(dones[0]["job_id"], job_id);
        assert_eq!(dones[0]["code"], -1);
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
