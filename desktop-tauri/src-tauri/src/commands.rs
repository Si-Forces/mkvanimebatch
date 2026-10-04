use crate::process::{
    reserve_job_slot, stop_registered_scan, ActiveScan, BatchStatus, JobSlot, JobSnapshot,
    JobState, ScanRegistration, ScanState,
};
use mkv_core::manifest::InspectionManifest;
use mkv_core::pipeline::{
    CancellationToken, EventReceiver, JobEvent, LogReceiver, PipelineMode, PipelineSummary,
};
use mkv_core::scan::PersistentInspectCache;
use serde::Deserialize;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::OnceCell;

const JOB_EVENT: &str = "job-event";
const JOB_LOG: &str = "job-log";
const JOB_DONE: &str = "job-done";
pub(crate) const CLEANUP_ERROR: &str = "cleanup-error";
/// Log lines are forwarded to the UI in batches of at most this many lines...
const LOG_BATCH_MAX_LINES: usize = 25;
/// ...or whenever this interval elapses with lines pending.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectOptions {
    no_cache: Option<bool>,
}

/// The one inspect cache of an app session, shared by every Inspect and Run. The first Inspect
/// or Run that wants a cache loads it; a `noCache` request never does.
#[derive(Clone, Default)]
pub struct InspectCacheState {
    cell: Arc<OnceCell<Arc<PersistentInspectCache>>>,
    /// `None` is the default cache file; only tests set a temporary one.
    cache_file: Option<PathBuf>,
}

/// Loads the cache on the blocking pool. With `delete_first`, a disabled instance first deletes
/// the file without parsing it, which leaves the loaded instance nothing to read.
async fn load_cache(
    cache_file: Option<PathBuf>,
    delete_first: bool,
) -> Result<Arc<PersistentInspectCache>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if delete_first {
            PersistentInspectCache::new(cache_file.clone(), false).clear();
        }
        Arc::new(PersistentInspectCache::new(cache_file, true))
    })
    .await
    .map_err(|e| format!("Async runtime error: {e}"))
}

impl InspectCacheState {
    /// The session cache, parsed on the blocking pool by whichever caller gets here first.
    async fn shared(&self) -> Result<Arc<PersistentInspectCache>, String> {
        self.cell
            .get_or_try_init(|| load_cache(self.cache_file.clone(), false))
            .await
            .cloned()
    }

    /// Empties the session cache and deletes its file. When the cache is not loaded yet, the
    /// file is deleted unparsed and the session starts from an empty cache. A load already in
    /// progress is awaited and then cleared, so it cannot bring the deleted entries back.
    async fn clear(&self) -> Result<(), String> {
        let mut loaded = true;
        let cache = self
            .cell
            .get_or_try_init(|| {
                loaded = false;
                load_cache(self.cache_file.clone(), true)
            })
            .await?;
        if loaded {
            let cache = Arc::clone(cache);
            tauri::async_runtime::spawn_blocking(move || cache.clear())
                .await
                .map_err(|e| format!("Async runtime error: {e}"))?;
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn inspect_directory(
    app: AppHandle,
    state: State<'_, ScanState>,
    cache_state: State<'_, InspectCacheState>,
    path: String,
    options: Option<InspectOptions>,
) -> Result<InspectionManifest, String> {
    let start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    state.ensure_accepting_starts()?;
    let dir = std::fs::canonicalize(&path).map_err(|e| format!("Invalid directory path: {e}"))?;
    if !dir.is_dir() {
        return Err("Directory does not exist or is not a directory".into());
    }
    stop_registered_scan(&state)?;

    let scan_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let cancel_token = CancellationToken::new();
    *state.active.lock().map_err(|e| e.to_string())? = Some(ActiveScan {
        id: scan_id,
        cancel_token: cancel_token.clone(),
    });
    app.state::<JobState>().clear_snapshot();
    drop(start_lock);
    let _registration = ScanRegistration {
        state: &state,
        id: scan_id,
    };

    let no_cache = options.and_then(|o| o.no_cache).unwrap_or(false);
    let cache = if no_cache {
        None
    } else {
        Some(cache_state.shared().await?)
    };
    let mut manifest =
        mkv_core::manifest::generate_inspection_manifest(&dir, cache, Some(&cancel_token)).await?;
    manifest.extra.insert(
        "canonical_dir".to_string(),
        display_canonical_path(&dir.to_string_lossy()).into(),
    );
    Ok(manifest)
}

#[tauri::command]
pub fn run_job<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, JobState>,
    cache_state: State<'_, InspectCacheState>,
    mut request: mkv_core::request::BatchRequest,
) -> Result<u64, String> {
    request.input_dir =
        std::fs::canonicalize(&request.input_dir).unwrap_or_else(|_| request.input_dir.clone());
    let input_dir = display_canonical_path(&request.input_dir.to_string_lossy());
    request.validate()?;

    let mode = match request.mode {
        PipelineMode::Extract => "extract",
        PipelineMode::Remux => "remux",
    };
    let cancel_token = CancellationToken::new();
    let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
    let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(128);
    let mut pipeline_config = request.into_pipeline_config()?;
    let session_cache = (!pipeline_config.no_cache).then(|| cache_state.inner().clone());
    let pipeline_cancel = cancel_token.clone();

    let config = SupervisedJobConfig {
        mode,
        input_dir,
        cancel_token,
    };
    start_supervised_job(app, &state, config, event_rx, log_rx, async move {
        // Loaded inside the job, so the first Run of a session never blocks the command.
        if let Some(session_cache) = session_cache {
            pipeline_config.cache = Some(session_cache.shared().await?);
        }
        mkv_core::pipeline::BatchPipeline::new(pipeline_config, Some(event_tx), pipeline_cancel)
            .with_log_sender(log_tx)
            .run()
            .await
    })
}

/// Strips the Windows verbatim prefix (`\\?\`, `\\?\UNC\`) that `canonicalize`
/// adds, so every path the UI compares uses the same display form.
fn display_canonical_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

/// Tags an engine event with the job that produced it so the UI can route it.
fn attach_context(event: &mut JobEvent, mode: &str, job_id: u64) {
    let (JobEvent::BatchStart { extra, .. }
    | JobEvent::ItemStart { extra, .. }
    | JobEvent::ItemProgress { extra, .. }
    | JobEvent::ItemComplete { extra, .. }
    | JobEvent::BatchComplete { extra, .. }) = event;
    extra.insert("mode".to_string(), mode.into());
    extra.insert("job_id".to_string(), job_id.into());
}

/// A terminal the supervisor synthesises when the worker did not deliver one.
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
    mode: &'static str,
    input_dir: String,
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
    let SupervisedJobConfig {
        mode,
        input_dir,
        cancel_token,
    } = config;
    let mut lock = reserve_job_slot(state)?;
    let job_id = state.next_id.fetch_add(1, Ordering::SeqCst);
    state.begin_snapshot(job_id, mode, input_dir)?;

    let pipeline_handle = tauri::async_runtime::spawn(pipeline_future);
    let pipeline_abort = pipeline_handle.inner().abort_handle();
    let abort_on_supervisor_drop = pipeline_abort.clone();
    let supervisor_cancel = cancel_token.clone();
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
            app: app.clone(),
            job_id,
        };
        let _abort_guard = PipelineAbortGuard(abort_on_supervisor_drop);

        let app_event = app.clone();
        let event_task = async move {
            let state = app_event.state::<JobState>();
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
                    // The engine reports the raw canonical input path, which is
                    // verbatim (`\\?\`) on Windows. Normalize it to the same display
                    // form the Inspection Manifest uses so both sides of the Queue
                    // Tracker's directory-identity comparison agree.
                    if let JobEvent::BatchStart { input_dir, .. } = &mut event {
                        *input_dir = display_canonical_path(input_dir);
                    }
                    attach_context(&mut event, mode, job_id);
                    state.update_snapshot(&event, job_id, mode);
                    let _ = app_event.emit(JOB_EVENT, &event);
                }
            }
            completion
        };
        let app_log = app.clone();
        let log_task = async move {
            let flush = |batch: &mut Vec<String>| {
                if !batch.is_empty() {
                    let _ = app_log.emit(JOB_LOG, &*batch);
                    batch.clear();
                }
            };
            let mut batch = Vec::with_capacity(LOG_BATCH_MAX_LINES);
            let mut interval = tokio::time::interval(LOG_FLUSH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    line = log_rx.recv() => match line {
                        Some(line) => {
                            batch.push(line);
                            if batch.len() >= LOG_BATCH_MAX_LINES {
                                flush(&mut batch);
                            }
                        }
                        None => {
                            flush(&mut batch);
                            break;
                        }
                    },
                    _ = interval.tick() => flush(&mut batch),
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
            // Stop wins over an incidental worker error: the job was already
            // cancelled, so the terminal must stay cancelled. Either way the
            // error is still surfaced in the job log.
            Ok(Err(error)) => {
                let _ = app.emit(JOB_LOG, [format!("[ERR] Pipeline failed: {error}")]);
                if supervisor_cancel.is_cancelled() {
                    bridge_terminal(true, None)
                } else {
                    bridge_terminal(false, Some(("error", error)))
                }
            }
            Err(tauri::Error::JoinError(join_error))
                if join_error.is_cancelled() && supervisor_cancel.is_cancelled() =>
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
                let _ = app.emit(JOB_LOG, [format!("[PANIC] {message}")]);
                bridge_terminal(false, Some(("panic", message)))
            }
            Err(_) => bridge_terminal(
                false,
                Some(("error", "Pipeline task cancelled unexpectedly".to_string())),
            ),
        };
        attach_context(&mut terminal, mode, job_id);
        let state = app.state::<JobState>();
        state.update_snapshot(&terminal, job_id, mode);
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
                BatchStatus::Completed => 0,
                BatchStatus::Cancelled => -1,
                _ => 1,
            })
            .unwrap_or(1);
        let _ = app.emit(JOB_EVENT, &terminal);
        let _ = app.emit(
            JOB_DONE,
            serde_json::json!({ "mode": mode, "job_id": job_id, "code": code }),
        );
    });

    *lock = Some(JobSlot {
        id: job_id,
        cancel_token,
        pipeline_abort,
    });
    Ok(job_id)
}

#[tauri::command]
pub async fn clear_cache(cache_state: State<'_, InspectCacheState>) -> Result<String, String> {
    // Clearing a separate instance would not stick: the shared one's next save would write
    // the cleared entries back.
    cache_state.clear().await?;
    Ok("Persistent inspection cache cleared.".to_string())
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
pub fn get_job_snapshot(state: State<'_, JobState>) -> Result<Option<JobSnapshot>, String> {
    Ok(state.snapshot.lock().map_err(|e| e.to_string())?.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::stop_job_internal;
    use crate::process::test_support::{batch_complete, batch_start, item_progress, item_start};
    use crate::process::ItemStatus;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tauri::test::MockRuntime;
    use tauri::Listener;

    type Sink<T> = Arc<Mutex<Vec<T>>>;
    type App = tauri::App<MockRuntime>;

    fn record<T: serde::de::DeserializeOwned + Send + 'static>(app: &App, event: &str) -> Sink<T> {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&sink);
        app.listen(event, move |event| {
            writer
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        sink
    }

    /// A mock app recording every `job-event`, `job-done` and `job-log` payload.
    fn observed_app() -> (App, Sink<Value>, Sink<Value>, Sink<Vec<String>>) {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        // Never loaded: the runs these tests start all set `no_cache`.
        app.manage(InspectCacheState::default());
        let events = record(&app, JOB_EVENT);
        let dones = record(&app, JOB_DONE);
        let logs = record(&app, JOB_LOG);
        (app, events, dones, logs)
    }

    fn start_remux<F>(
        app: &App,
        cancel_token: CancellationToken,
        event_rx: EventReceiver,
        log_rx: LogReceiver,
        worker: F,
    ) -> u64
    where
        F: Future<Output = Result<PipelineSummary, String>> + Send + 'static,
    {
        let config = SupervisedJobConfig {
            mode: "remux",
            input_dir: "/anime".into(),
            cancel_token,
        };
        let state = app.state::<JobState>();
        start_supervised_job(
            app.handle().clone(),
            &state,
            config,
            event_rx,
            log_rx,
            worker,
        )
        .unwrap()
    }

    /// Runs a dry-run extract of `input_dir`, without the inspect cache, to completion and
    /// returns its snapshot.
    fn run_dry_extract(app: &App, input_dir: PathBuf) -> JobSnapshot {
        run_dry_extract_with_cache(app, input_dir, false)
    }

    fn run_dry_extract_with_cache(app: &App, input_dir: PathBuf, cache: bool) -> JobSnapshot {
        let request = mkv_core::request::BatchRequest {
            mode: PipelineMode::Extract,
            input_dir,
            dry_run: Some(true),
            no_cache: Some(!cache),
            ..Default::default()
        };
        let job_id = run_job(
            app.handle().clone(),
            app.state::<JobState>(),
            app.state::<InspectCacheState>(),
            request,
        )
        .expect("dry-run extract must start");
        let state = app.state::<JobState>();
        state
            .wait_until_released(job_id, Duration::from_secs(5))
            .unwrap();
        let snapshot = state.snapshot.lock().unwrap().clone();
        snapshot.expect("a finished job must leave a snapshot")
    }

    /// Asserts exactly one payload in `sink` matches `job_id` (and `pred`) and returns it.
    fn only_one(sink: &Sink<Value>, job_id: u64, pred: impl Fn(&Value) -> bool) -> Value {
        let matching: Vec<Value> = sink
            .lock()
            .unwrap()
            .iter()
            .filter(|payload| payload["job_id"] == job_id && pred(payload))
            .cloned()
            .collect();
        assert_eq!(matching.len(), 1, "job {job_id}: {matching:?}");
        matching.into_iter().next().unwrap()
    }

    fn terminal_for(events: &Sink<Value>, job_id: u64) -> Value {
        only_one(events, job_id, |event| event["type"] == "batch_complete")
    }

    fn done_for(dones: &Sink<Value>, job_id: u64) -> Value {
        only_one(dones, job_id, |_| true)
    }

    fn poll_until(what: &str, mut ready: impl FnMut() -> bool) {
        let start = Instant::now();
        while !ready() {
            assert!(start.elapsed() < Duration::from_secs(2), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_progress(state: &JobState, id: &str, target: f64) {
        poll_until(&format!("progress never reached {target}"), || {
            state
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|snapshot| snapshot.items.get(id))
                .and_then(|item| item.progress)
                == Some(target)
        });
    }

    fn logged(logs: &Sink<Vec<String>>, needle: &str) -> bool {
        logs.lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|line| line.contains(needle))
    }

    async fn panicking_worker(
        event_tx: mkv_core::pipeline::EventSender,
    ) -> Result<PipelineSummary, String> {
        event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
        panic!("bridge worker panicked");
    }

    #[test]
    fn test_inspect_options_deserialization() {
        let opts: InspectOptions = serde_json::from_str(r#"{"noCache": true}"#).unwrap();
        assert_eq!(opts.no_cache, Some(true));

        let opts_default: InspectOptions = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(opts_default.no_cache, None);
    }

    #[test]
    fn test_run_job_canonicalizes_linked_input_dir() {
        let (app, events, _, _) = observed_app();
        let target_dir = tempfile::tempdir().unwrap();
        let parent_dir = tempfile::tempdir().unwrap();
        let symlink_path = parent_dir.path().join("symlink_target");

        #[cfg(unix)]
        std::os::unix::fs::symlink(target_dir.path(), &symlink_path).unwrap();
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&symlink_path)
                .arg(target_dir.path())
                .output()
                .expect("cmd.exe must be available to create a directory junction");
            assert!(
                output.status.success(),
                "mklink /J failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        assert!(
            symlink_path.exists(),
            "link creation failed; run_job input_dir canonicalization is untested"
        );
        let linked_input_dir = symlink_path.to_string_lossy().into_owned();
        let snapshot_input_dir = run_dry_extract(&app, symlink_path)
            .input_dir
            .expect("snapshot should preserve input path");
        let canonical_target = std::fs::canonicalize(target_dir.path()).unwrap();
        assert_ne!(snapshot_input_dir, linked_input_dir);
        assert_eq!(
            std::fs::canonicalize(&snapshot_input_dir).unwrap(),
            canonical_target
        );

        // Canonical Directory Identity: the emitted BatchStart must carry the same
        // display form as the snapshot and the Inspection Manifest's `canonical_dir`.
        // A Windows verbatim `\\?\` prefix surviving here splits directory identity
        // and silently drops Queue Selection on the next same-directory rescan.
        let emitted = events.lock().unwrap();
        let start = emitted
            .iter()
            .find(|event| event["type"] == "batch_start")
            .expect("a batch must emit BatchStart");
        let emitted_input_dir = start["input_dir"].as_str().unwrap();
        assert_eq!(emitted_input_dir, snapshot_input_dir);
        assert!(
            !emitted_input_dir.starts_with(r"\\?\"),
            "BatchStart.input_dir leaked a verbatim prefix: {emitted_input_dir}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_run_job_preserves_non_utf8_canonical_input_path() {
        use std::os::unix::ffi::OsStringExt;

        let (app, _, _, _) = observed_app();
        let target_dir = tempfile::tempdir().unwrap();
        let parent_dir = tempfile::tempdir().unwrap();
        let non_utf8_target = target_dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 0xff, b'r']));
        std::fs::create_dir(&non_utf8_target).unwrap();
        let symlink_path = parent_dir.path().join("input-link");
        std::os::unix::fs::symlink(&non_utf8_target, &symlink_path).unwrap();

        let snapshot = run_dry_extract(&app, symlink_path);
        let expected_display = std::fs::canonicalize(non_utf8_target)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            snapshot.input_dir.as_deref(),
            Some(expected_display.as_str())
        );
    }

    #[test]
    fn emitted_job_event_carries_top_level_mode_and_job_id() {
        let (app, events, _, _) = observed_app();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            // An engine-supplied `extra` entry doubles as the flattening probe: if
            // `extra` stops being lifted, it nests and `mode` never reaches the top level.
            let mut extra = serde_json::Map::new();
            extra.insert("source".to_string(), "engine".into());
            event_tx
                .send(JobEvent::BatchStart {
                    version: 1,
                    total: 1,
                    input_dir: "/anime".into(),
                    items: None,
                    extra,
                })
                .await
                .unwrap();
            event_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let job_id = start_remux(&app, CancellationToken::new(), event_rx, log_rx, worker);
        app.state::<JobState>()
            .wait_until_released(job_id, Duration::from_secs(2))
            .unwrap();

        let emitted = events.lock().unwrap();
        let start = emitted
            .iter()
            .find(|event| event["type"] == "batch_start")
            .expect("a batch must emit BatchStart");
        assert_eq!(start["mode"], "remux");
        assert_eq!(start["job_id"], job_id);
        assert_eq!(start["source"], "engine");
    }

    #[test]
    fn forced_stop_aborts_only_worker_and_settles_once() {
        let (app, events, dones, _) = observed_app();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 70.0)).await.unwrap();
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
        let job_id = start_remux(&app, CancellationToken::new(), event_rx, log_rx, worker);
        let state = app.state::<JobState>();
        wait_for_progress(&state, "ep2", 70.0);
        stop_job_internal(&state, Some(job_id), Duration::from_millis(20)).unwrap();

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
        let terminal = terminal_for(&events, job_id);
        assert_eq!(terminal["cancelled"], true);
        assert!(terminal["source"].is_null());
        assert_eq!(done_for(&dones, job_id)["code"], -1);
    }

    #[test]
    fn worker_error_after_stop_settles_as_cancelled() {
        let (app, events, dones, logs) = observed_app();
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            log_tx.send("probing batch".to_string()).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 40.0)).await.unwrap();
            worker_token.cancelled().await;
            Err("scan failed after stop".to_string())
        };
        let job_id = start_remux(&app, token, event_rx, log_rx, worker);
        let state = app.state::<JobState>();
        wait_for_progress(&state, "ep2", 40.0);
        stop_job_internal(&state, Some(job_id), Duration::from_secs(1)).unwrap();

        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(snapshot.completed_items, 2);
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Cancelled);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Cancelled);
        let terminal = terminal_for(&events, job_id);
        assert_eq!(terminal["cancelled"], true);
        assert!(terminal["error"].is_null());
        assert_eq!(done_for(&dones, job_id)["code"], -1);
        assert!(
            logged(&logs, "scan failed after stop"),
            "worker error must stay in the job log"
        );
    }

    #[test]
    fn worker_error_without_stop_settles_as_failed_with_the_error() {
        let (app, events, dones, logs) = observed_app();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(16);
        let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            log_tx.send("probing batch".to_string()).await.unwrap();
            Err("scan failed before any stop".to_string())
        };
        let job_id = start_remux(&app, CancellationToken::new(), event_rx, log_rx, worker);
        poll_until(&format!("job {job_id} never reported a completion"), || {
            dones
                .lock()
                .unwrap()
                .iter()
                .any(|done| done["job_id"] == job_id)
        });

        let terminal = terminal_for(&events, job_id);
        assert_eq!(terminal["cancelled"], false);
        assert_eq!(terminal["error"], "scan failed before any stop");
        assert_ne!(done_for(&dones, job_id)["code"], -1);
        assert!(
            logged(&logs, "scan failed before any stop"),
            "worker error must reach the job log"
        );
    }

    #[test]
    fn cooperative_stop_ignores_stale_id_and_duplicate_completion() {
        let (app, events, dones, _) = observed_app();
        let state = app.state::<JobState>();
        let (previous_tx, previous_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, previous_logs) = mkv_core::pipeline::create_log_channel(8);
        let previous_worker = async move {
            previous_tx.send(batch_start(&[])).await.unwrap();
            previous_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary::default())
        };
        let previous_id = start_remux(
            &app,
            CancellationToken::new(),
            previous_rx,
            previous_logs,
            previous_worker,
        );
        state
            .wait_until_released(previous_id, Duration::from_secs(2))
            .unwrap();

        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let worker = async move {
            event_tx.send(batch_start(&["ep2", "ep1"])).await.unwrap();
            event_tx.send(item_start("ep2")).await.unwrap();
            event_tx.send(item_progress("ep2", 70.0)).await.unwrap();
            worker_token.cancelled().await;
            event_tx.send(batch_complete(false, true)).await.unwrap();
            event_tx.send(batch_complete(true, false)).await.unwrap();
            Ok(PipelineSummary {
                cancelled: true,
                ..Default::default()
            })
        };
        let job_id = start_remux(&app, token.clone(), event_rx, log_rx, worker);
        wait_for_progress(&state, "ep2", 70.0);
        stop_job_internal(&state, Some(previous_id), Duration::from_millis(10)).unwrap();
        assert!(!token.is_cancelled());
        assert!(state.is_active());
        stop_job_internal(&state, Some(job_id), Duration::from_secs(1)).unwrap();

        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Cancelled);
        assert_eq!(snapshot.completed_items, 2);
        assert_eq!(terminal_for(&events, job_id)["cancelled"], true);
        done_for(&dones, job_id);
    }

    #[test]
    fn worker_panic_unwinds_into_one_error_terminal() {
        let (app, events, dones, _) = observed_app();
        let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(8);
        let (_, log_rx) = mkv_core::pipeline::create_log_channel(8);
        let job_id = start_remux(
            &app,
            CancellationToken::new(),
            event_rx,
            log_rx,
            panicking_worker(event_tx),
        );
        let state = app.state::<JobState>();
        state
            .wait_until_released(job_id, Duration::from_secs(2))
            .unwrap();

        let snapshot = state.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.status, BatchStatus::Error);
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Error);
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Error);
        assert_eq!(
            terminal_for(&events, job_id)["panic"],
            "bridge worker panicked"
        );
        assert_eq!(done_for(&dones, job_id)["code"], 1);
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

    /// A session cache backed by `dir/inspect_cache.json`, whose file already holds an entry
    /// for `dir/video.mkv`. Returns the state and the video path.
    fn session_cache_with_saved_entry(dir: &std::path::Path) -> (InspectCacheState, PathBuf) {
        let cache_file = dir.join("inspect_cache.json");
        let video = dir.join("video.mkv");
        std::fs::write(&video, b"video").unwrap();
        let writer = PersistentInspectCache::new(Some(cache_file.clone()), true);
        writer.put_path(&video, Default::default());
        writer.save();
        let state = InspectCacheState {
            cache_file: Some(cache_file),
            ..Default::default()
        };
        (state, video)
    }

    #[test]
    fn session_cache_reads_its_file_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, video) = session_cache_with_saved_entry(tmp.path());

        let first = tauri::async_runtime::block_on(state.shared()).unwrap();
        assert!(first.get_path(&video).is_some());
        std::fs::write(tmp.path().join("inspect_cache.json"), b"{}").unwrap();
        let second = tauri::async_runtime::block_on(state.shared()).unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert!(
            second.get_path(&video).is_some(),
            "a second caller must not reload the file"
        );
    }

    #[test]
    fn clear_before_load_deletes_the_file_without_loading_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, video) = session_cache_with_saved_entry(tmp.path());

        tauri::async_runtime::block_on(state.clear()).unwrap();

        assert!(!tmp.path().join("inspect_cache.json").exists());
        let cache = tauri::async_runtime::block_on(state.shared()).unwrap();
        assert_eq!(cache.get_path(&video), None, "the cleared file was loaded");
    }

    #[test]
    fn clear_after_load_empties_the_shared_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, video) = session_cache_with_saved_entry(tmp.path());
        let loaded = tauri::async_runtime::block_on(state.shared()).unwrap();

        tauri::async_runtime::block_on(state.clear()).unwrap();

        assert!(!tmp.path().join("inspect_cache.json").exists());
        let after = tauri::async_runtime::block_on(state.shared()).unwrap();
        assert!(Arc::ptr_eq(&loaded, &after));
        assert_eq!(after.get_path(&video), None);
        after.save();
        assert!(
            !tmp.path().join("inspect_cache.json").exists(),
            "a later save must not restore cleared entries"
        );
    }

    #[test]
    fn run_job_shares_the_session_cache_unless_no_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("Show - 01.mkv"), b"video").unwrap();
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        app.manage(InspectCacheState {
            cache_file: Some(tmp.path().join("inspect_cache.json")),
            ..Default::default()
        });
        let cell = || app.state::<InspectCacheState>().cell.get().cloned();

        run_dry_extract(&app, input.clone());
        assert!(cell().is_none(), "a noCache run must not load the cache");

        run_dry_extract_with_cache(&app, input, true);
        let used = cell().expect("a cached run loads the session cache");
        let shared = tauri::async_runtime::block_on(app.state::<InspectCacheState>().shared());
        assert!(Arc::ptr_eq(&used, &shared.unwrap()));
    }
}
