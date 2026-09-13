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
use serde::Deserialize;
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
    let cancel_token = tokio_util::sync::CancellationToken::new();
    *state.active.lock().map_err(|e| e.to_string())? = Some(ActiveScan {
        id: scan_id,
        pid: 0,
        cancel_token: Some(cancel_token.clone()),
    });
    app.state::<JobState>().clear_snapshot();
    drop(start_lock);
    let registration = ScanRegistration {
        state: &state,
        id: scan_id,
    };

    let no_cache = options.as_ref().and_then(|o| o.no_cache).unwrap_or(false);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let cache = mkv_core::scan::PersistentInspectCache::default();
        let cache_opt = if no_cache { None } else { Some(&cache) };
        mkv_core::manifest::generate_inspection_manifest(&dir, cache_opt, Some(&cancel_token))
    })
    .await
    .unwrap_or_else(|e| Err(format!("Async runtime error: {}", e)));

    drop(registration);
    result
}

#[tauri::command]
pub fn run_job<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, JobState>,
    request: mkv_core::request::BatchRequest,
) -> Result<u64, String> {
    request.validate()?;

    let mut lock = reserve_job_slot(&state)?;

    let mode_str = match request.mode {
        mkv_core::pipeline::PipelineMode::Extract => "extract",
        mkv_core::pipeline::PipelineMode::Remux => "remux",
    }
    .to_string();

    let job_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let cancel_token = tokio_util::sync::CancellationToken::new();

    let (event_tx, mut event_rx) = mkv_core::pipeline::create_event_channel(64);
    let (log_tx, mut log_rx) = mkv_core::pipeline::create_log_channel(128);

    let pipeline = mkv_core::pipeline::BatchPipeline::from_request(
        request,
        Some(event_tx),
        cancel_token.clone(),
    )?
    .with_log_sender(Some(log_tx));

    let app_task = app.clone();
    let mode_task = mode_str;
    let task_handle = tauri::async_runtime::spawn(async move {
        struct JobSlotGuard<R: tauri::Runtime> {
            app: AppHandle<R>,
            job_id: u64,
        }
        impl<R: tauri::Runtime> Drop for JobSlotGuard<R> {
            fn drop(&mut self) {
                let state = self.app.state::<JobState>();
                state.release_slot(self.job_id);
                state.cancel_snapshot_if_running();
            }
        }
        let _guard = JobSlotGuard {
            app: app_task.clone(),
            job_id,
        };

        let app_event = app_task.clone();
        let mode_event = mode_task.clone();
        let state_for_event = app_task.state::<JobState>();
        let event_task = async move {
            while let Some(mut event) = event_rx.recv().await {
                event.attach_context(&mode_event, job_id);
                state_for_event.update_snapshot(&event, job_id, &mode_event);
                let _ = app_event.emit("job-event", &event);
            }
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
                            Some(l) => {
                                batch.push(l);
                                if batch.len() >= 25 {
                                    let _ = app_log.emit("job-log", &batch);
                                    batch.clear();
                                }
                            }
                            None => {
                                if !batch.is_empty() {
                                    let _ = app_log.emit("job-log", &batch);
                                    batch.clear();
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

        struct PipelineAbortGuard(tokio::task::AbortHandle);
        impl Drop for PipelineAbortGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }

        let pipeline_handle = tokio::spawn(async move { pipeline.run().await });
        let _abort_guard = PipelineAbortGuard(pipeline_handle.abort_handle());

        let (pipeline_result, (), ()) = tokio::join!(pipeline_handle, event_task, log_task);

        let code = match pipeline_result {
            Ok(Ok(summary)) => {
                if summary.cancelled {
                    -1
                } else if summary.errors > 0 {
                    1
                } else {
                    0
                }
            }
            Ok(Err(e)) => {
                let _ = app_task.emit("job-log", vec![format!("[ERR] Pipeline failed: {e}")]);
                1
            }
            Err(join_err) => {
                let panic_msg = if join_err.is_panic() {
                    let panic_payload = join_err.into_panic();
                    if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown pipeline panic".to_string()
                    }
                } else {
                    "Pipeline task cancelled or aborted".to_string()
                };

                let mut extra = serde_json::Map::new();
                extra.insert(
                    "mode".to_string(),
                    serde_json::Value::String(mode_task.clone()),
                );
                extra.insert(
                    "job_id".to_string(),
                    serde_json::Value::Number(job_id.into()),
                );
                extra.insert(
                    "panic".to_string(),
                    serde_json::Value::String(panic_msg.clone()),
                );

                let event = JobEvent::BatchComplete {
                    version: 1,
                    success: false,
                    cancelled: false,
                    extra,
                };
                let state_for_panic = app_task.state::<JobState>();
                state_for_panic.update_snapshot(&event, job_id, &mode_task);
                let _ = app_task.emit("job-event", &event);
                let _ = app_task.emit("job-log", vec![format!("[PANIC] {panic_msg}")]);
                1
            }
        };

        let _ = app_task.emit(
            "job-done",
            serde_json::json!({
                "mode": mode_task,
                "job_id": job_id,
                "code": code
            }),
        );
    });

    *lock = Some(JobSlot {
        id: job_id,
        cancel_token,
        task_handle,
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
pub fn pick_folder() -> Option<String> {
    pick_directory()
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
}
