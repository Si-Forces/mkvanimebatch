use crate::contract::{InspectionManifest, JobEvent, JobEventExt};
pub use crate::process::{
    begin_shutdown, cancel_shutdown, cleanup_owned_processes, close_window, stop_job, ItemSnapshot,
    ItemStatus, JobSnapshot, JobState, ScanState,
};
use crate::process::{
    reserve_job_slot, stop_registered_scan, ActiveScan, JobSlot, ScanRegistration,
};
pub use crate::telemetry::{get_system_telemetry, TelemetryState};
pub use crate::window::{
    is_maximized, minimize_window, set_window_theme, start_drag, start_resize, toggle_maximize,
};
use mkv_core::pipeline::{CancellationToken, EventReceiver, LogReceiver, PipelineSummary};
use serde::Deserialize;
use std::future::Future;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectOptions {
    #[serde(default)]
    pub no_cache: Option<bool>,
}

#[tauri::command]
pub async fn inspect_directory(
    app: AppHandle,
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
    let cancel_token = CancellationToken::new();
    *state.active.lock().map_err(|e| e.to_string())? = Some(ActiveScan {
        id: scan_id,
        cancel_token: Some(cancel_token.clone()),
    });
    app.state::<JobState>().clear_snapshot();
    drop(start_lock);
    let registration = ScanRegistration {
        state: &state,
        id: scan_id,
    };

    let no_cache = options.as_ref().and_then(|o| o.no_cache).unwrap_or(false);
    let cache = mkv_core::scan::PersistentInspectCache::default();
    let cache_opt = if no_cache { None } else { Some(&cache) };
    let mut manifest =
        mkv_core::manifest::generate_inspection_manifest(&dir, cache_opt, Some(&cancel_token))
            .await?;
    let clean_canonical = display_canonical_path(&dir.to_string_lossy());
    manifest.extra.insert(
        "canonical_dir".to_string(),
        serde_json::Value::String(clean_canonical),
    );

    drop(registration);
    Ok(manifest)
}

#[tauri::command]
pub fn run_job<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, JobState>,
    mut request: mkv_core::request::BatchRequest,
) -> Result<u64, String> {
    request.validate()?;

    let mode = match request.mode {
        mkv_core::pipeline::PipelineMode::Extract => "extract",
        mkv_core::pipeline::PipelineMode::Remux => "remux",
    }
    .to_string();
    let cancel_token = CancellationToken::new();
    let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
    let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(128);
    let canonical_input =
        std::fs::canonicalize(&request.input_dir).unwrap_or_else(|_| request.input_dir.clone());
    let clean_canonical_input = display_canonical_path(&canonical_input.to_string_lossy());
    request.input_dir = std::path::PathBuf::from(&clean_canonical_input);
    let input_dir = Some(clean_canonical_input);
    let pipeline = mkv_core::pipeline::BatchPipeline::from_request(
        request,
        Some(event_tx),
        cancel_token.clone(),
    )?
    .with_log_sender(Some(log_tx));

    let config = SupervisedJobConfig {
        mode,
        input_dir,
        cancel_token,
    };

    start_supervised_job(app, &state, config, event_rx, log_rx, async move {
        pipeline.run().await
    })
}

fn display_canonical_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

fn bridge_terminal(cancelled: bool, problem: Option<(&str, String)>) -> JobEvent {
    let mut extra = serde_json::Map::new();
    if let Some((key, message)) = problem {
        extra.insert(key.to_string(), message.into());
    }
    JobEvent::BatchComplete {
        version: mkv_core::pipeline::EVENT_VERSION,
        success: false,
        cancelled,
        extra,
    }
}

struct SupervisedJobConfig {
    mode: String,
    input_dir: Option<String>,
    cancel_token: CancellationToken,
}

fn start_supervised_job<R: tauri::Runtime, F>(
    app: AppHandle<R>,
    state: &JobState,
    config: SupervisedJobConfig,
    mut event_rx: EventReceiver,
    mut log_rx: LogReceiver,
    pipeline_future: F,
) -> Result<u64, String>
where
    F: Future<Output = Result<PipelineSummary, String>> + Send + 'static,
{
    let mut lock = reserve_job_slot(state)?;
    let job_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    state.begin_snapshot(job_id, &config.mode, config.input_dir)?;

    let pipeline_handle = tauri::async_runtime::spawn(pipeline_future);
    let pipeline_abort = pipeline_handle.inner().abort_handle();
    let app_task = app.clone();
    let abort_on_supervisor_drop = pipeline_abort.clone();
    let cancel_for_supervisor = config.cancel_token.clone();
    let cancel_token = config.cancel_token;
    let mode = config.mode;
    tauri::async_runtime::spawn(async move {
        struct JobSlotGuard<R: tauri::Runtime> {
            app: AppHandle<R>,
            job_id: u64,
        }
        impl<R: tauri::Runtime> Drop for JobSlotGuard<R> {
            fn drop(&mut self) {
                self.app.state::<JobState>().release_slot(self.job_id);
            }
        }
        struct PipelineAbortGuard(tokio::task::AbortHandle);
        impl Drop for PipelineAbortGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }

        let _slot_guard = JobSlotGuard {
            app: app_task.clone(),
            job_id,
        };
        let _abort_guard = PipelineAbortGuard(abort_on_supervisor_drop);

        let app_event = app_task.clone();
        let mode_event = mode.clone();
        let event_task = async move {
            let mut completion = None;
            while let Some(mut event) = event_rx.recv().await {
                if matches!(event, JobEvent::BatchComplete { .. }) {
                    // One supervisor owns one job ID. Defer its first terminal until worker exit;
                    // an abort or panic can supersede a late cooperative completion.
                    if completion.is_none() {
                        completion = Some(event);
                    }
                    continue;
                }
                if completion.is_none() {
                    event.attach_context(&mode_event, job_id);
                    app_event
                        .state::<JobState>()
                        .update_snapshot(&event, job_id, &mode_event);
                    let _ = app_event.emit("job-event", &event);
                }
            }
            completion
        };
        let app_log = app_task.clone();
        let log_task = async move {
            let mut batch: Vec<String> = Vec::with_capacity(25);
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(25));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    line = log_rx.recv() => {
                        match line {
                            Some(line) => {
                                batch.push(line);
                                if batch.len() >= 25 {
                                    let _ = app_log.emit("job-log", &batch);
                                    batch.clear();
                                }
                            }
                            None => {
                                if !batch.is_empty() {
                                    let _ = app_log.emit("job-log", &batch);
                                }
                                break;
                            }
                        }
                    }
                    _ = interval.tick() => {
                        if !batch.is_empty() {
                            let _ = app_log.emit("job-log", &batch);
                            batch.clear();
                        }
                    }
                }
            }
        };

        let (pipeline_result, completion, ()) = tokio::join!(pipeline_handle, event_task, log_task);
        let mut terminal = match pipeline_result {
            Ok(Ok(summary)) => completion.unwrap_or_else(|| {
                if summary.cancelled {
                    bridge_terminal(true, None)
                } else {
                    bridge_terminal(
                        false,
                        Some((
                            "error",
                            "Pipeline ended without a batch completion event".to_string(),
                        )),
                    )
                }
            }),
            Ok(Err(error)) => {
                let _ = app_task.emit("job-log", vec![format!("[ERR] Pipeline failed: {error}")]);
                bridge_terminal(false, Some(("error", error)))
            }
            Err(tauri::Error::JoinError(join_error))
                if join_error.is_cancelled() && cancel_for_supervisor.is_cancelled() =>
            {
                bridge_terminal(true, None)
            }
            Err(tauri::Error::JoinError(join_error)) if join_error.is_panic() => {
                let payload = join_error.into_panic();
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|value| (*value).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "Unknown pipeline panic".to_string());
                let _ = app_task.emit("job-log", vec![format!("[PANIC] {message}")]);
                bridge_terminal(false, Some(("panic", message)))
            }
            Err(_) => bridge_terminal(
                false,
                Some(("error", "Pipeline task cancelled unexpectedly".to_string())),
            ),
        };
        terminal.attach_context(&mode, job_id);
        let state = app_task.state::<JobState>();
        state.update_snapshot(&terminal, job_id, &mode);
        let code = state
            .snapshot
            .lock()
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .as_ref()
                    .filter(|item| item.job_id == job_id)
                    .map(|item| item.status)
            })
            .map(|status| match status {
                crate::process::BatchStatus::Completed => 0,
                crate::process::BatchStatus::Cancelled => -1,
                _ => 1,
            })
            .unwrap_or(1);
        let _ = app_task.emit("job-event", &terminal);
        let _ = app_task.emit(
            "job-done",
            serde_json::json!({ "mode": mode, "job_id": job_id, "code": code }),
        );
    });

    *lock = Some(JobSlot {
        id: job_id,
        cancel_token,
        pipeline_abort,
        stopping: false,
    });
    Ok(job_id)
}

#[tauri::command]
pub async fn clear_cache(_app: AppHandle, _mode: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cache = mkv_core::scan::PersistentInspectCache::default();
        cache.clear();
        Ok("Persistent inspection cache cleared.".into())
    })
    .await
    .unwrap_or_else(|e| Err(format!("Async runtime error: {}", e)))
}

#[tauri::command]
pub fn pick_directory() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn pick_subtitle_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Subtitle Files", &["ass", "ssa", "srt", "vtt", "sub"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn get_preset_profiles() -> Vec<mkv_core::preset::PresetProfile> {
    mkv_core::preset::get_preset_profiles()
}

#[tauri::command]
pub fn get_job_snapshot(
    state: State<'_, JobState>,
) -> Result<Option<crate::process::JobSnapshot>, String> {
    Ok(state.snapshot.lock().map_err(|e| e.to_string())?.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{stop_job_internal_with_timeout, BatchStatus};
    use mkv_core::pipeline::BatchItemInfo;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::Listener;
    type EventSink = Arc<Mutex<Vec<serde_json::Value>>>;

    fn observed_app() -> (tauri::App<tauri::test::MockRuntime>, EventSink, EventSink) {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let dones = Arc::new(Mutex::new(Vec::new()));
        let event_sink = Arc::clone(&events);
        let done_sink = Arc::clone(&dones);
        app.listen("job-event", move |event| {
            event_sink
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        app.listen("job-done", move |event| {
            done_sink
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        (app, events, dones)
    }

    fn batch_start() -> JobEvent {
        JobEvent::BatchStart {
            version: 1,
            total: 2,
            input_dir: "/anime".into(),
            items: Some(vec![
                BatchItemInfo {
                    id: "ep2".into(),
                    file: "02.mkv".into(),
                },
                BatchItemInfo {
                    id: "ep1".into(),
                    file: "01.mkv".into(),
                },
            ]),
            extra: Default::default(),
        }
    }

    fn terminal_events_for(events: &[serde_json::Value], job_id: u64) -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["type"] == "batch_complete" && event["job_id"] == job_id)
            .collect()
    }

    fn done_events_for(events: &[serde_json::Value], job_id: u64) -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["job_id"] == job_id)
            .collect()
    }

    fn wait_for_progress(state: &JobState, id: &str, target: f64) {
        let start = Instant::now();
        loop {
            let progress = state
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|snapshot| snapshot.items.get(id))
                .and_then(|item| item.progress);
            if progress == Some(target) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "progress never reached {target}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    async fn panicking_worker(
        event_tx: mkv_core::pipeline::EventSender,
    ) -> Result<PipelineSummary, String> {
        event_tx.send(batch_start()).await.unwrap();
        panic!("bridge worker panicked");
    }

    #[test]
    fn test_inspect_options_deserialization() {
        let opts: InspectOptions = serde_json::from_str(r#"{"noCache": true}"#).unwrap();
        assert_eq!(opts.no_cache, Some(true));

        let opts_default: InspectOptions = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(opts_default.no_cache, None);

        let opts_false: InspectOptions = serde_json::from_str(r#"{"noCache": false}"#).unwrap();
        assert_eq!(opts_false.no_cache, Some(false));

        assert!(serde_json::from_str::<InspectOptions>(r#"{"noCache": 1}"#).is_err());
        assert!(serde_json::from_str::<InspectOptions>(r#"{"noCache": "true"}"#).is_err());
    }

    #[test]
    fn test_get_and_clear_job_snapshot() {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());

        let state = app.state::<JobState>();
        let snap = get_job_snapshot(state).expect("get_job_snapshot should succeed");
        assert!(snap.is_none());

        let state = app.state::<JobState>();
        let start_event = mkv_core::pipeline::JobEvent::BatchStart {
            version: 1,
            total: 2,
            input_dir: "/test".to_string(),
            items: None,
            extra: Default::default(),
        };
        state.update_snapshot(&start_event, 101, "remux");

        let state = app.state::<JobState>();
        let snap = get_job_snapshot(state)
            .expect("get_job_snapshot should succeed")
            .expect("snapshot should be present");
        assert_eq!(snap.job_id, 101);
        assert_eq!(snap.mode, "remux");
        assert_eq!(snap.total_items, 2);

        let state = app.state::<JobState>();
        state.clear_snapshot();

        let state = app.state::<JobState>();
        let snap = get_job_snapshot(state).expect("get_job_snapshot should succeed");
        assert!(snap.is_none());
    }

    #[test]
    fn test_run_job_starts_without_panic() {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());

        let temp_dir = tempfile::tempdir().unwrap();
        let request = mkv_core::request::BatchRequest {
            mode: mkv_core::pipeline::PipelineMode::Extract,
            input_dir: temp_dir.path().to_path_buf(),
            dry_run: Some(true),
            ..Default::default()
        };

        let state = app.state::<JobState>();
        let res = run_job(app.handle().clone(), state, request);
        assert!(res.is_ok());
        let job_id = res.unwrap();
        let verify_state = app.state::<JobState>();
        assert!(verify_state
            .wait_until_released(job_id, std::time::Duration::from_secs(5))
            .is_ok());
    }
    #[test]
    fn test_run_job_canonicalizes_symlinked_input_dir() {
        let (app, _events, _dones) = observed_app();
        let target_dir = tempfile::tempdir().unwrap();
        let parent_dir = tempfile::tempdir().unwrap();
        let symlink_path = parent_dir.path().join("symlink_target");

        #[cfg(unix)]
        std::os::unix::fs::symlink(target_dir.path(), &symlink_path).unwrap();
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(target_dir.path(), &symlink_path);

        if symlink_path.exists() {
            let request = mkv_core::request::BatchRequest {
                mode: mkv_core::pipeline::PipelineMode::Extract,
                input_dir: symlink_path,
                dry_run: Some(true),
                ..Default::default()
            };

            let state = app.state::<JobState>();
            let res = run_job(app.handle().clone(), state, request);
            assert!(res.is_ok());
            let job_id = res.unwrap();
            let verify_state = app.state::<JobState>();
            let _ = verify_state.wait_until_released(job_id, std::time::Duration::from_secs(5));

            let snapshot = verify_state.snapshot.lock().unwrap().clone().unwrap();
            let canonical_target = std::fs::canonicalize(target_dir.path()).unwrap();
            let clean_canonical = display_canonical_path(&canonical_target.to_string_lossy());
            assert_eq!(
                snapshot.input_dir.as_deref(),
                Some(clean_canonical.as_str())
            );
        }
    }
    #[test]
    fn forced_stop_aborts_only_worker_and_settles_once() {
        let (app, events, dones) = observed_app();
        let state = app.state::<JobState>();
        let token = CancellationToken::new();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start()).await.unwrap();
            event_tx
                .send(JobEvent::ItemStart {
                    version: 1,
                    id: "ep2".into(),
                    file: "02.mkv".into(),
                    extra: Default::default(),
                })
                .await
                .unwrap();
            event_tx
                .send(JobEvent::ItemProgress {
                    version: 1,
                    id: "ep2".into(),
                    file: "02.mkv".into(),
                    progress: 70.0,
                    speed: None,
                    eta: None,
                    extra: Default::default(),
                })
                .await
                .unwrap();
            let mut extra = serde_json::Map::new();
            extra.insert("source".to_string(), "cooperative".into());
            event_tx
                .send(JobEvent::BatchComplete {
                    version: 1,
                    success: false,
                    cancelled: true,
                    extra,
                })
                .await
                .unwrap();
            std::future::pending::<Result<PipelineSummary, String>>().await
        };
        let job_id = start_supervised_job(
            app.handle().clone(),
            &state,
            SupervisedJobConfig {
                mode: "remux".into(),
                input_dir: Some("/anime".into()),
                cancel_token: token,
            },
            event_rx,
            log_rx,
            worker,
        )
        .unwrap();
        wait_for_progress(&state, "ep2", 70.0);
        stop_job_internal_with_timeout(&state, Some(job_id), Duration::from_millis(20)).unwrap();

        assert!(!state.is_active());
        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(
            snapshot.ordered_item_ids,
            vec!["ep2".to_string(), "ep1".to_string()]
        );
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.completed_items, 2);
        assert_eq!(snapshot.items["ep2"].progress, Some(70.0));
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Cancelled);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Cancelled);
        let events = events.lock().unwrap();
        let terminals = terminal_events_for(&events, job_id);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["cancelled"], true);
        assert!(terminals[0]["source"].is_null());
        let dones = dones.lock().unwrap();
        let completions = done_events_for(&dones, job_id);
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0]["code"], -1);
    }

    #[test]
    fn cooperative_stop_ignores_stale_id_and_duplicate_completion() {
        let (app, events, dones) = observed_app();
        let state = app.state::<JobState>();
        let (previous_tx, previous_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, previous_logs) = mkv_core::pipeline::create_log_channel(8);
        let previous_worker = async move {
            previous_tx
                .send(JobEvent::BatchStart {
                    version: 1,
                    total: 0,
                    input_dir: "/anime".into(),
                    items: Some(Vec::new()),
                    extra: Default::default(),
                })
                .await
                .unwrap();
            previous_tx
                .send(JobEvent::BatchComplete {
                    version: 1,
                    success: true,
                    cancelled: false,
                    extra: Default::default(),
                })
                .await
                .unwrap();
            Ok(PipelineSummary::default())
        };
        let previous_id = start_supervised_job(
            app.handle().clone(),
            &state,
            SupervisedJobConfig {
                mode: "remux".into(),
                input_dir: Some("/anime".into()),
                cancel_token: CancellationToken::new(),
            },
            previous_rx,
            previous_logs,
            previous_worker,
        )
        .unwrap();
        state
            .wait_until_released(previous_id, Duration::from_secs(2))
            .unwrap();

        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start()).await.unwrap();
            event_tx
                .send(JobEvent::ItemStart {
                    version: 1,
                    id: "ep2".into(),
                    file: "02.mkv".into(),
                    extra: Default::default(),
                })
                .await
                .unwrap();
            event_tx
                .send(JobEvent::ItemProgress {
                    version: 1,
                    id: "ep2".into(),
                    file: "02.mkv".into(),
                    progress: 70.0,
                    speed: None,
                    eta: None,
                    extra: Default::default(),
                })
                .await
                .unwrap();
            worker_token.cancelled().await;
            event_tx
                .send(JobEvent::BatchComplete {
                    version: 1,
                    success: false,
                    cancelled: true,
                    extra: Default::default(),
                })
                .await
                .unwrap();
            event_tx
                .send(JobEvent::BatchComplete {
                    version: 1,
                    success: true,
                    cancelled: false,
                    extra: Default::default(),
                })
                .await
                .unwrap();
            Ok(PipelineSummary {
                cancelled: true,
                ..Default::default()
            })
        };
        let job_id = start_supervised_job(
            app.handle().clone(),
            &state,
            SupervisedJobConfig {
                mode: "remux".into(),
                input_dir: Some("/anime".into()),
                cancel_token: token.clone(),
            },
            event_rx,
            log_rx,
            worker,
        )
        .unwrap();
        wait_for_progress(&state, "ep2", 70.0);
        stop_job_internal_with_timeout(&state, Some(previous_id), Duration::from_millis(10))
            .unwrap();
        assert!(!token.is_cancelled());
        assert!(state.is_active());
        stop_job_internal_with_timeout(&state, Some(job_id), Duration::from_secs(1)).unwrap();

        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(snapshot.completed_items, 2);
        let events = events.lock().unwrap();
        let terminals = terminal_events_for(&events, job_id);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["cancelled"], true);
        let dones = dones.lock().unwrap();
        assert_eq!(done_events_for(&dones, job_id).len(), 1);
    }

    #[test]
    fn worker_panic_unwinds_into_one_error_terminal() {
        let (app, events, dones) = observed_app();
        let state = app.state::<JobState>();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let job_id = start_supervised_job(
            app.handle().clone(),
            &state,
            SupervisedJobConfig {
                mode: "remux".into(),
                input_dir: Some("/anime".into()),
                cancel_token: CancellationToken::new(),
            },
            event_rx,
            log_rx,
            panicking_worker(event_tx),
        )
        .unwrap();
        state
            .wait_until_released(job_id, Duration::from_secs(2))
            .unwrap();

        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Error);
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Error);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Error);
        let events = events.lock().unwrap();
        let terminals = terminal_events_for(&events, job_id);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["panic"], "bridge worker panicked");
        let dones = dones.lock().unwrap();
        let completions = done_events_for(&dones, job_id);
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0]["code"], 1);
    }

    #[test]
    fn display_canonical_path_preserves_unc_and_drive_paths() {
        assert_eq!(
            display_canonical_path(r"\\?\C:\Anime\Show"),
            r"C:\Anime\Show"
        );
        assert_eq!(
            display_canonical_path(r"\\?\UNC\server\share\Show"),
            r"\\server\share\Show"
        );
        assert_eq!(display_canonical_path("/home/user/show"), "/home/user/show");
    }
}
