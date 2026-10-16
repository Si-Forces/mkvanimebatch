//! The Stop and Close commands: thin wrappers that run the Desktop Core's teardown on Tauri's
//! blocking pool, reading the states through the `AppHandle`.

use mkv_desktop_core::control::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, stop_job_inner, ScanState,
};
use mkv_desktop_core::job::JobState;
use std::sync::Arc;
use tauri::{AppHandle, Manager, Runtime};

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
        teardown(&app.state::<JobState>(), &app.state::<Arc<ScanState>>())
    })
    .await
    .map_err(|error| format!("Async runtime error: {error}"))?
}

/// Allowed while a Stop is pending: both cancel the same token and wait on the same slot, and a
/// second abort is a no-op.
#[tauri::command]
pub async fn close_window(window: tauri::Window, app: AppHandle) -> Result<(), String> {
    // Refuse starts before the blocking-pool hop, so a `run_job` that lands while the pool
    // schedules the teardown is refused instead of starting a job the teardown then cancels. One
    // async hop remains: this body runs on a Tokio task, not at dispatch.
    let (state, scan_state) = (app.state::<JobState>(), app.state::<Arc<ScanState>>());
    begin_shutdown(&state, &scan_state);
    run_teardown(app.clone(), cleanup_owned_processes)
        .await
        .and_then(|()| window.close().map_err(|error| error.to_string()))
        .inspect_err(|_| cancel_shutdown(&state, &scan_state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_core::pipeline::{CancellationToken, PipelineSummary};
    use mkv_desktop_core::control::cleanup_owned_processes_with_timeout;
    use mkv_desktop_core::job::test_support::RecordingSink;
    use mkv_desktop_core::job::JobConfig;
    use mkv_desktop_core::snapshot::test_support::effective_settings;
    use std::time::{Duration, Instant};

    #[test]
    fn test_teardown_leaves_the_calling_runtime_free_while_it_waits() {
        let app = tauri::test::mock_app();
        app.manage(mkv_desktop_core::job::test_support::job_state());
        app.manage(Arc::new(ScanState::default()));
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
    fn close_during_a_pending_stop_settles_the_job_once() {
        let app = tauri::test::mock_app();
        app.manage(mkv_desktop_core::job::test_support::job_state());
        app.manage(Arc::new(ScanState::default()));
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
}
