use crate::job::{display_canonical_path, JobConfig, JobSink, JobState};
use crate::process::{stop_registered_scan, ActiveScan, ScanRegistration, ScanState};
use crate::snapshot::JobSnapshot;
use mkv_core::manifest::InspectionManifest;
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::{BatchRequest, EffectiveSettings};
use mkv_core::scan::PersistentInspectCache;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::sync::OnceCell;

pub(crate) const CLEANUP_ERROR: &str = "cleanup-error";

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

/// Canonicalizes `path` and pairs the result with its Directory Identity: the display form
/// of the canonical path, which is the only spelling the UI compares directories by.
fn canonicalize_directory(path: &str) -> Result<(PathBuf, String), String> {
    let dir = std::fs::canonicalize(path).map_err(|e| format!("Invalid directory path: {e}"))?;
    let identity = display_canonical_path(&dir.to_string_lossy());
    Ok((dir, identity))
}

/// The Directory Identity of `path`: what `inspect_directory` reports as the manifest's
/// `canonical_dir`. The UI resolves a typed path through it before comparing directories.
#[tauri::command]
pub async fn canonical_directory(path: String) -> Result<String, String> {
    canonicalize_directory(&path).map(|(_, identity)| identity)
}

#[tauri::command]
pub async fn inspect_directory<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, ScanState>,
    cache_state: State<'_, InspectCacheState>,
    path: String,
    options: Option<InspectOptions>,
) -> Result<InspectionManifest, String> {
    let start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    state.ensure_accepting_starts()?;
    let (dir, identity) = canonicalize_directory(&path)?;
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
    manifest
        .extra
        .insert("canonical_dir".to_string(), identity.into());
    Ok(manifest)
}

/// Resolves `request` into the config a Batch Pipeline runs, after canonicalizing its
/// `input_dir`. A path that cannot be canonicalized stays as given, for `resolve` to reject.
/// `run_job` and `resolve_batch_request` both go through it, so the UI's preview and the run
/// agree on the directory and on every error.
fn resolve_canonical(mut request: BatchRequest) -> Result<PipelineConfig, String> {
    request.input_dir =
        std::fs::canonicalize(&request.input_dir).unwrap_or_else(|_| request.input_dir.clone());
    request.resolve()
}

/// The Effective Settings `run_job` would run `request` with, or the reason it would refuse it.
/// Synchronous and slot-free, so the UI can check a request before it starts its preflight scan.
#[tauri::command]
pub fn resolve_batch_request(request: BatchRequest) -> Result<EffectiveSettings, String> {
    resolve_canonical(request).map(|config| config.effective_settings())
}

/// Starts a job for `request` in `job`, reporting to `sink`. `run_job` without the Tauri
/// plumbing, so a test can run a real request against a recording sink.
fn launch(
    job: &JobState,
    sink: impl JobSink,
    cache_state: &InspectCacheState,
    request: BatchRequest,
) -> Result<u64, String> {
    let mut pipeline_config = resolve_canonical(request)?;
    let input_dir = display_canonical_path(&pipeline_config.input_dir.to_string_lossy());

    let mode = match pipeline_config.mode {
        PipelineMode::Extract => "extract",
        PipelineMode::Remux => "remux",
    };
    let cancel_token = CancellationToken::new();
    let (event_tx, event_rx) = mkv_core::pipeline::create_event_channel(64);
    let (log_tx, log_rx) = mkv_core::pipeline::create_log_channel(128);
    let session_cache = (!pipeline_config.no_cache).then(|| cache_state.clone());
    let pipeline_cancel = cancel_token.clone();

    let config = JobConfig {
        mode,
        input_dir,
        effective_settings: pipeline_config.effective_settings(),
        cancel_token,
    };
    job.start(sink, config, event_rx, log_rx, async move {
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

#[tauri::command]
pub fn run_job<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, JobState>,
    cache_state: State<'_, InspectCacheState>,
    request: BatchRequest,
) -> Result<u64, String> {
    launch(&state, app, &cache_state, request)
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

/// The Effective Settings of a `BatchRequest` that leaves every setting unset, for the UI's
/// "auto (N)" and idle displays.
#[tauri::command]
pub fn get_default_settings() -> EffectiveSettings {
    EffectiveSettings::defaults()
}

#[tauri::command]
pub fn get_job_snapshot(state: State<'_, JobState>) -> Result<Option<JobSnapshot>, String> {
    state.snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::test_support::RecordingSink;
    use std::time::Duration;

    /// What `run_job` has to run a request: a job state, a sink recording what its jobs report,
    /// and the session cache.
    struct Bridge {
        job: JobState,
        sink: RecordingSink,
        cache_state: InspectCacheState,
    }

    impl Bridge {
        /// Never loads the cache: the runs these tests start all set `no_cache`.
        fn new() -> Self {
            Self {
                job: JobState::default(),
                sink: RecordingSink::default(),
                cache_state: InspectCacheState::default(),
            }
        }

        /// Runs a dry-run extract of `input_dir`, without the inspect cache, to completion and
        /// returns its snapshot.
        fn run_dry_extract(&self, input_dir: PathBuf) -> JobSnapshot {
            self.run_dry_extract_with_cache(input_dir, false)
        }

        fn run_dry_extract_with_cache(&self, input_dir: PathBuf, cache: bool) -> JobSnapshot {
            self.run_to_completion(BatchRequest {
                no_cache: Some(!cache),
                ..dry_run_request(&input_dir)
            })
        }

        /// Runs `request` through `launch` to completion and returns its snapshot.
        fn run_to_completion(&self, request: BatchRequest) -> JobSnapshot {
            let job_id = launch(&self.job, self.sink.clone(), &self.cache_state, request)
                .expect("dry-run extract must start");
            self.job
                .wait_until_released(job_id, Duration::from_secs(5))
                .unwrap();
            let snapshot = self.job.snapshot().unwrap();
            snapshot.expect("a finished job must leave a snapshot")
        }
    }

    #[test]
    fn test_inspect_options_deserialization() {
        let opts: InspectOptions = serde_json::from_str(r#"{"noCache": true}"#).unwrap();
        assert_eq!(opts.no_cache, Some(true));

        let opts_default: InspectOptions = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(opts_default.no_cache, None);
    }

    /// A dry-run extract request of `input_dir` that sets nothing else.
    fn dry_run_request(input_dir: &std::path::Path) -> BatchRequest {
        BatchRequest {
            mode: PipelineMode::Extract,
            input_dir: input_dir.to_path_buf(),
            dry_run: Some(true),
            no_cache: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn test_get_default_settings_match_a_resolved_empty_request() {
        let tmp = tempfile::tempdir().unwrap();
        let defaults = get_default_settings();
        assert_eq!(
            resolve_batch_request(dry_run_request(tmp.path())),
            Ok(defaults.clone())
        );
        // `BatchRequest::resolve` accepts 1-64; the default is clamped to 1-16.
        assert!(
            (1..=16).contains(&defaults.workers),
            "workers out of range: {}",
            defaults.workers
        );
    }

    #[test]
    fn test_resolve_batch_request_layers_explicit_values_over_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = resolve_batch_request(BatchRequest {
            workers: Some(3),
            io_jobs: Some(0),
            ffmpeg_threads: Some(2),
            bitrate: Some(" 256k ".into()),
            complexity: Some(8),
            ..dry_run_request(tmp.path())
        })
        .unwrap();
        // An explicit `io_jobs` of 0 is the worker count.
        assert_eq!(
            settings,
            EffectiveSettings {
                workers: 3,
                io_jobs: 3,
                ffmpeg_threads: 2,
                bitrate: "256k".into(),
                complexity: 8,
            }
        );
    }

    #[test]
    fn test_resolve_batch_request_reports_the_first_validation_error() {
        let tmp = tempfile::tempdir().unwrap();
        let error = resolve_batch_request(BatchRequest {
            workers: Some(0),
            io_jobs: Some(65),
            ..dry_run_request(tmp.path())
        })
        .unwrap_err();
        assert_eq!(error, "Worker thread count must be between 1 and 64");

        let missing = tmp.path().join("missing");
        let error = resolve_batch_request(dry_run_request(&missing)).unwrap_err();
        assert!(error.contains("Input directory does not exist"), "{error}");
    }

    #[test]
    fn test_canonical_directory_is_the_display_form_of_the_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        // A dotted spelling of the directory.
        let spelled = tmp.path().join(".");
        let expected =
            display_canonical_path(&std::fs::canonicalize(tmp.path()).unwrap().to_string_lossy());

        let identity = tauri::async_runtime::block_on(canonical_directory(
            spelled.to_string_lossy().into_owned(),
        ))
        .unwrap();

        assert_eq!(identity, expected);
    }

    #[test]
    fn test_inspect_directory_reports_the_identity_canonical_directory_returns() {
        let app = tauri::test::mock_app();
        app.manage(JobState::default());
        app.manage(ScanState::default());
        app.manage(InspectCacheState::default());
        let tmp = tempfile::tempdir().unwrap();
        let spelled = tmp.path().join(".").to_string_lossy().into_owned();

        let manifest = tauri::async_runtime::block_on(inspect_directory(
            app.handle().clone(),
            app.state(),
            app.state(),
            spelled.clone(),
            Some(InspectOptions {
                no_cache: Some(true),
            }),
        ))
        .unwrap();
        let identity = tauri::async_runtime::block_on(canonical_directory(spelled)).unwrap();

        assert_eq!(
            manifest.extra["canonical_dir"].as_str(),
            Some(identity.as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_canonical_directory_resolves_a_symlink_to_its_target() {
        let target = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        let identity = tauri::async_runtime::block_on(canonical_directory(
            link.to_string_lossy().into_owned(),
        ))
        .unwrap();

        let expected = std::fs::canonicalize(target.path()).unwrap();
        assert_eq!(identity, expected.to_string_lossy());
    }

    #[test]
    fn test_canonical_directory_rejects_a_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");

        let error = tauri::async_runtime::block_on(canonical_directory(
            missing.to_string_lossy().into_owned(),
        ))
        .unwrap_err();

        assert!(error.starts_with("Invalid directory path: "), "{error}");
    }

    #[test]
    fn test_run_job_snapshot_carries_the_effective_settings() {
        let bridge = Bridge::new();
        let tmp = tempfile::tempdir().unwrap();
        let request = BatchRequest {
            workers: Some(3),
            io_jobs: Some(0),
            bitrate: Some("256k".into()),
            ..dry_run_request(tmp.path())
        };
        let expected = resolve_batch_request(request.clone()).unwrap();

        let snapshot = bridge.run_to_completion(request);

        // The snapshot is the finished job's, after BatchStart replaced the one `run_job` began.
        assert_eq!(snapshot.effective_settings, Some(expected));
        assert_eq!(snapshot.total_items, 0);
    }

    #[test]
    fn test_run_job_canonicalizes_linked_input_dir() {
        let bridge = Bridge::new();
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
        let snapshot_input_dir = bridge
            .run_dry_extract(symlink_path)
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
        let emitted = bridge.sink.events();
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

        let bridge = Bridge::new();
        let target_dir = tempfile::tempdir().unwrap();
        let parent_dir = tempfile::tempdir().unwrap();
        let non_utf8_target = target_dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 0xff, b'r']));
        std::fs::create_dir(&non_utf8_target).unwrap();
        let symlink_path = parent_dir.path().join("input-link");
        std::os::unix::fs::symlink(&non_utf8_target, &symlink_path).unwrap();

        let snapshot = bridge.run_dry_extract(symlink_path);
        let expected_display = std::fs::canonicalize(non_utf8_target)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            snapshot.input_dir.as_deref(),
            Some(expected_display.as_str())
        );
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
        let bridge = Bridge {
            cache_state: InspectCacheState {
                cache_file: Some(tmp.path().join("inspect_cache.json")),
                ..Default::default()
            },
            ..Bridge::new()
        };
        let cell = || bridge.cache_state.cell.get().cloned();

        bridge.run_dry_extract(input.clone());
        assert!(cell().is_none(), "a noCache run must not load the cache");

        bridge.run_dry_extract_with_cache(input, true);
        let used = cell().expect("a cached run loads the session cache");
        let shared = tauri::async_runtime::block_on(bridge.cache_state.shared());
        assert!(Arc::ptr_eq(&used, &shared.unwrap()));
    }
}
