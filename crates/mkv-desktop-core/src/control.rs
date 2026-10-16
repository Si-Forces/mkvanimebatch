//! Run, stop and shutdown control of the Engine Bridge: the session inspect cache, the start of a
//! job from a `BatchRequest`, the Directory Inspection registry, and the teardown that Stop and
//! Close share. A UI adapter wraps these in its own commands.

use crate::job::{display_canonical_path, wait_until, JobConfig, JobSink, JobState};
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::BatchRequest;
use mkv_core::scan::PersistentInspectCache;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;

struct ActiveScan {
    id: u64,
    cancel_token: CancellationToken,
}

pub struct ScanState {
    active: Mutex<Option<ActiveScan>>,
    next_id: AtomicU64,
    start_lock: Mutex<()>,
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

    pub fn ensure_accepting_starts(&self) -> Result<(), String> {
        if self.accepting_starts.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Application is closing; no new directory inspection can start.".into())
        }
    }

    /// Registers a Directory Inspection of `dir`, which the caller has already canonicalized,
    /// and returns its registration and cancel token. The caller canonicalizes before this
    /// because the start lock's guard is not `Send` and so cannot be held across that await; the
    /// accepting check repeats under the lock for a shutdown that began meanwhile. A previous
    /// inspection is cancelled, and the new one replaces it as the active scan; the previous
    /// registration's drop then leaves the new one alone. Keep the registration alive until the
    /// inspection ends: its drop is what tells teardown the inspection has finished.
    pub fn begin(&self, dir: &Path) -> Result<(ScanRegistration<'_>, CancellationToken), String> {
        let start_lock = self.start_lock.lock().map_err(|e| e.to_string())?;
        self.ensure_accepting_starts()?;
        if !dir.is_dir() {
            return Err("Directory does not exist or is not a directory".into());
        }
        stop_registered_scan(self)?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel_token = CancellationToken::new();
        *self.active.lock().map_err(|e| e.to_string())? = Some(ActiveScan {
            id,
            cancel_token: cancel_token.clone(),
        });
        drop(start_lock);
        Ok((ScanRegistration { state: self, id }, cancel_token))
    }

    fn wait_until_empty(&self, timeout: Duration) -> Result<(), String> {
        wait_until(timeout, "directory inspection", || {
            Ok(self.active.lock().map_err(|e| e.to_string())?.is_none())
        })
    }
}

/// A running Directory Inspection. Its drop is the only place that vacates the active scan, and
/// only while the scan is still its own.
pub struct ScanRegistration<'a> {
    state: &'a ScanState,
    id: u64,
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

pub fn stop_job_inner(
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

fn stop_scan(state: &ScanState) -> Result<(), String> {
    let _start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    stop_registered_scan(state)
}

/// Cancels the active scan but leaves it registered: its `ScanRegistration` vacates it when the
/// inspection actually ends, so `wait_until_empty` waits for that rather than for the cancel.
fn stop_registered_scan(state: &ScanState) -> Result<(), String> {
    let active = state.active.lock().map_err(|e| e.to_string())?;
    if let Some(scan) = active.as_ref() {
        scan.cancel_token.cancel();
    }
    Ok(())
}

pub fn cleanup_owned_processes(job_state: &JobState, scan_state: &ScanState) -> Result<(), String> {
    cleanup_owned_processes_with_timeout(job_state, scan_state, STOP_TIMEOUT)
}

pub fn cleanup_owned_processes_with_timeout(
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

/// The one inspect cache of an app session, shared by every Inspect and Run. The first Inspect
/// or Run that wants a cache loads it; a `noCache` request never does. Its loads and clears use
/// the blocking pool of the tokio runtime they are awaited on.
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
    tokio::task::spawn_blocking(move || {
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
    pub async fn shared(&self) -> Result<Arc<PersistentInspectCache>, String> {
        self.cell
            .get_or_try_init(|| load_cache(self.cache_file.clone(), false))
            .await
            .cloned()
    }

    /// Empties the session cache and deletes its file. When the cache is not loaded yet, the
    /// file is deleted unparsed and the session starts from an empty cache. A load already in
    /// progress is awaited and then cleared, so it cannot bring the deleted entries back.
    pub async fn clear(&self) -> Result<(), String> {
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
            tokio::task::spawn_blocking(move || cache.clear())
                .await
                .map_err(|e| format!("Async runtime error: {e}"))?;
        }
        Ok(())
    }
}

/// Resolves `request` into the config a Batch Pipeline runs, after canonicalizing its
/// `input_dir`. A path that cannot be canonicalized stays as given, for `resolve` to reject.
/// `run_job` and `resolve_batch_request` both go through it, so the UI's preview and the run
/// agree on the directory and on every error.
pub fn resolve_canonical(mut request: BatchRequest) -> Result<PipelineConfig, String> {
    request.input_dir =
        std::fs::canonicalize(&request.input_dir).unwrap_or_else(|_| request.input_dir.clone());
    request.resolve()
}

/// Starts a job for `request` in `job`, reporting to `sink`: what the `run_job` command runs, and
/// what a test runs against a recording sink.
pub fn launch(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::test_support::{job_state, poison, runtime, RecordingSink};
    use crate::snapshot::JobSnapshot;

    #[test]
    fn test_stale_stop_job_does_not_stop_active_scan() {
        let scan_state = ScanState::default();
        let token = CancellationToken::new();
        *scan_state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        let job = job_state();
        job.occupy(7);
        assert!(stop_job_inner(&job, &scan_state, Some(99)).is_ok());
        assert!(stop_job_inner(&job_state(), &scan_state, Some(99)).is_ok());

        assert!(scan_state.is_active());
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_stop_job_proceeds_when_scan_teardown_fails() {
        let scan_state = ScanState::default();
        poison(&scan_state.start_lock);

        let job = job_state();
        job.occupy(7);

        let error = stop_job_inner(&job, &scan_state, Some(7)).unwrap_err();
        assert!(error.contains("poisoned"));
        assert!(job.current_cancelled());
    }

    #[test]
    fn test_cleanup_attempts_scan_and_job_teardown() {
        let scan_state = ScanState::default();
        poison(&scan_state.start_lock);
        let job = job_state();
        job.poison_slot();

        let error = cleanup_owned_processes(&job, &scan_state).unwrap_err();
        assert!(error.contains("Directory inspection cleanup failed"));
        assert!(error.contains("Job cleanup failed"));
    }

    #[test]
    fn test_scan_startup_is_visible_before_process_registration() {
        let job = job_state();
        let scan_state = ScanState::default();

        let _startup = scan_state.start_lock.lock().unwrap();
        assert!(begin_shutdown(&job, &scan_state));
        assert!(scan_state.ensure_accepting_starts().is_err());
        assert!(job.ensure_accepting_starts().is_err());
    }

    #[test]
    fn test_cancel_shutdown_restores_accepting_starts() {
        let job = job_state();
        let scan_state = ScanState::default();

        begin_shutdown(&job, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_err());
        assert!(job.ensure_accepting_starts().is_err());

        cancel_shutdown(&job, &scan_state);
        assert!(scan_state.ensure_accepting_starts().is_ok());
        assert!(job.ensure_accepting_starts().is_ok());
    }

    #[test]
    fn test_cleanup_fails_if_job_slot_not_released() {
        let job = job_state();
        let scan_state = ScanState::default();
        job.occupy(42);

        let error =
            cleanup_owned_processes_with_timeout(&job, &scan_state, Duration::from_millis(50))
                .unwrap_err();

        assert!(error.contains("Timed out"));
        assert!(error.contains("Job cleanup failed"));
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
        // Still registered: only the inspection's own registration vacates it.
        assert!(state.is_active());
    }

    #[test]
    fn test_cleanup_waits_until_the_scan_registration_drops() {
        let (job, scan_state) = (job_state(), ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let (registration, token) = scan_state.begin(tmp.path()).unwrap();
        let released = AtomicBool::new(false);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Stands in for an inspection that ends a while after its cancel.
                while !token.is_cancelled() {
                    std::thread::yield_now();
                }
                std::thread::sleep(Duration::from_millis(100));
                released.store(true, Ordering::SeqCst);
                drop(registration);
            });
            cleanup_owned_processes(&job, &scan_state).unwrap();
            assert!(
                released.load(Ordering::SeqCst),
                "cleanup returned before the cancelled inspection ended"
            );
        });

        let (_registration, _token) = scan_state.begin(tmp.path()).unwrap();
        let error =
            cleanup_owned_processes_with_timeout(&job, &scan_state, Duration::from_millis(50))
                .unwrap_err();
        assert!(error.contains("Directory inspection cleanup failed"));
        assert!(error.contains("Timed out"));
    }

    #[test]
    fn test_begin_replaces_and_cancels_the_previous_scan() {
        let scan_state = ScanState::default();
        let tmp = tempfile::tempdir().unwrap();
        let (first, first_token) = scan_state.begin(tmp.path()).unwrap();
        let (_second, second_token) = scan_state.begin(tmp.path()).unwrap();
        assert!(first_token.is_cancelled());

        drop(first);
        assert!(scan_state.is_active(), "the old drop vacated the new scan");
        assert!(!second_token.is_cancelled());
        assert!(scan_state.begin(&tmp.path().join("missing")).is_err());
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

    /// What `run_job` has to run a request: a job state, a sink recording what its jobs report,
    /// and the session cache.
    struct Bridge {
        job: JobState,
        sink: RecordingSink,
        cache_state: InspectCacheState,
    }

    impl Bridge {
        /// Backed by the default cache file, so its runs set `no_cache`. A test that loads the
        /// cache gives the bridge its own `cache_state`.
        fn new() -> Self {
            Self {
                job: job_state(),
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
            self.sink.snapshot()
        }

        /// The `job-done` code of the last job this bridge ran.
        fn done_code(&self) -> i64 {
            let dones = self.sink.dones();
            dones.last().expect("no job-done recorded")["code"]
                .as_i64()
                .unwrap()
        }
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
    fn test_run_job_reports_the_effective_settings_to_its_sink() {
        let bridge = Bridge::new();
        let tmp = tempfile::tempdir().unwrap();
        let request = BatchRequest {
            workers: Some(3),
            io_jobs: Some(0),
            bitrate: Some("256k".into()),
            ..dry_run_request(tmp.path())
        };
        let expected = resolve_canonical(request.clone())
            .unwrap()
            .effective_settings();

        let snapshot = bridge.run_to_completion(request);

        // The snapshot is the finished job's: BatchStart replaced its batch, not its settings.
        assert_eq!(snapshot.effective_settings, expected);
        assert_eq!(snapshot.total_items, 0);
        assert_eq!(bridge.done_code(), 0);
    }

    #[test]
    fn test_run_job_reports_the_canonical_linked_input_dir_to_its_sink() {
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
        let snapshot_input_dir = bridge.run_dry_extract(symlink_path).input_dir;
        assert_eq!(bridge.done_code(), 0);
        let canonical_target = std::fs::canonicalize(target_dir.path()).unwrap();
        assert_ne!(snapshot_input_dir, linked_input_dir);
        assert_eq!(
            std::fs::canonicalize(&snapshot_input_dir).unwrap(),
            canonical_target
        );

        // Directory Identity: the emitted BatchStart must carry the same
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
        assert_eq!(snapshot.input_dir, expected_display);
        assert_eq!(bridge.done_code(), 0);
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

        let first = runtime().block_on(state.shared()).unwrap();
        assert!(first.get_path(&video).is_some());
        std::fs::write(tmp.path().join("inspect_cache.json"), b"{}").unwrap();
        let second = runtime().block_on(state.shared()).unwrap();

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

        runtime().block_on(state.clear()).unwrap();

        assert!(!tmp.path().join("inspect_cache.json").exists());
        let cache = runtime().block_on(state.shared()).unwrap();
        assert_eq!(cache.get_path(&video), None, "the cleared file was loaded");
    }

    #[test]
    fn clear_after_load_empties_the_shared_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, video) = session_cache_with_saved_entry(tmp.path());
        let loaded = runtime().block_on(state.shared()).unwrap();

        runtime().block_on(state.clear()).unwrap();

        assert!(!tmp.path().join("inspect_cache.json").exists());
        let after = runtime().block_on(state.shared()).unwrap();
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
        let shared = runtime().block_on(bridge.cache_state.shared());
        assert!(Arc::ptr_eq(&used, &shared.unwrap()));
    }
}
