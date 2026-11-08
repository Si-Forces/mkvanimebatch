//! Run, stop and shutdown control of the Engine Bridge: the session inspect cache, the start of a
//! job from a `BatchRequest`, the Directory Inspection registry, and the teardown that Stop and
//! Close share. The `Launcher` port is how the Job Coordinator in the Desktop Core actor resolves,
//! launches and stops a job; `EngineLauncher` does it with these, and tests script a fake.

use crate::actor::ActorSink;
use crate::job::{display_canonical_path, wait_until, JobConfig, JobSink, JobState};
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::{BatchRequest, EffectiveSettings};
use mkv_core::scan::PersistentInspectCache;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;

struct ActiveScan {
    id: u64,
    cancel_token: CancellationToken,
}

/// The Directory Inspection registry: at most one inspection runs at a time, across both modes.
/// The Inspection Coordinator schedules each inspection on the Desktop Core actor, numbered in the
/// order it starts them, and the one numbered last wins: it cancels a running older one, and an
/// older one that registers after it is refused. Every scheduled inspection stays pending, with its
/// cancel token, until it ends, so a Stop can cancel and wait on it whether it is still
/// canonicalizing or already running. Shared behind an `Arc`, so an inspection that runs off the
/// actor holds the registry its teardown waits on.
pub struct ScanState {
    active: Mutex<Option<ActiveScan>>,
    /// Every scheduled inspection that has not ended, by ID.
    pending: Mutex<HashMap<u64, CancellationToken>>,
    start_lock: Mutex<()>,
    accepting_starts: AtomicBool,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            accepting_starts: AtomicBool::new(true),
        }
    }
}

/// What `begin` answers an inspection a Stop or a Close cancelled or a newer one overtook, and what
/// a stopped start's forced scan settles with: what a cancelled inspection answers too.
pub(crate) const INSPECTION_CANCELLED: &str = "Inspection cancelled";

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

    /// Schedules Directory Inspection `id`, pending with a cancel token of its own until the
    /// returned `Inspection` drops. Keep it alive until the inspection ends: its drop is what tells
    /// a Stop the inspection has ended.
    pub fn schedule(self: &Arc<Self>, id: u64) -> Inspection {
        let cancel_token = CancellationToken::new();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, cancel_token.clone());
        }
        Inspection {
            state: Arc::clone(self),
            id,
            cancel_token,
        }
    }

    /// Cancels inspection `id` alone, then waits up to `timeout` for it to end. One no longer
    /// pending has ended already: the actor schedules an inspection before a Stop can name it.
    pub(crate) fn stop_inspection(&self, id: u64, timeout: Duration) -> Result<(), String> {
        if let Some(cancel_token) = self.pending.lock().map_err(|e| e.to_string())?.get(&id) {
            cancel_token.cancel();
        }
        wait_until(timeout, "directory inspection", || {
            Ok(!self
                .pending
                .lock()
                .map_err(|e| e.to_string())?
                .contains_key(&id))
        })
    }

    /// Waits for the registered inspection alone. One that has not registered yet owns no child
    /// process, and `begin` refuses it once the app closes, so a Close need not wait on it.
    fn wait_until_empty(&self, timeout: Duration) -> Result<(), String> {
        wait_until(timeout, "directory inspection", || {
            Ok(self.active.lock().map_err(|e| e.to_string())?.is_none())
        })
    }
}

/// One scheduled Directory Inspection, from its scheduling until it ends: while its directory is
/// canonicalized, once it registers, and while it runs. Its drop ends it: it leaves the pending
/// inspections, and vacates the active scan while that is still its own.
#[must_use = "dropping it ends the inspection"]
pub struct Inspection {
    state: Arc<ScanState>,
    id: u64,
    cancel_token: CancellationToken,
}

impl Inspection {
    /// Registers this inspection of `dir`, which the caller has already canonicalized, as the
    /// active scan, and returns its cancel token. The caller canonicalizes before this because the
    /// start lock's guard is not `Send` and so cannot be held across that await; the accepting
    /// check repeats under the lock for a shutdown that began meanwhile. An older running
    /// inspection is cancelled, and this one replaces it; the older one's drop then leaves this one
    /// alone.
    ///
    /// # Errors
    ///
    /// Refuses, without cancelling anything, while the app closes, when `dir` is not a directory,
    /// when a Stop or a Close already cancelled this inspection, and when a newer inspection is
    /// registered.
    pub fn begin(&self, dir: &Path) -> Result<CancellationToken, String> {
        let start_lock = self.state.start_lock.lock().map_err(|e| e.to_string())?;
        self.state.ensure_accepting_starts()?;
        if !dir.is_dir() {
            return Err("Directory does not exist or is not a directory".into());
        }
        if self.cancel_token.is_cancelled() {
            return Err(INSPECTION_CANCELLED.into());
        }
        let mut active = self.state.active.lock().map_err(|e| e.to_string())?;
        if let Some(scan) = active.as_ref() {
            if scan.id > self.id {
                return Err(INSPECTION_CANCELLED.into());
            }
            scan.cancel_token.cancel();
        }
        *active = Some(ActiveScan {
            id: self.id,
            cancel_token: self.cancel_token.clone(),
        });
        drop(active);
        drop(start_lock);
        Ok(self.cancel_token.clone())
    }
}

impl Drop for Inspection {
    fn drop(&mut self) {
        if let Ok(mut active) = self.state.active.lock() {
            if active.as_ref().is_some_and(|scan| scan.id == self.id) {
                *active = None;
            }
        }
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.remove(&self.id);
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

/// What a Stop tears down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTarget {
    /// The launched job with this ID, and no Directory Inspection.
    Job(u64),
    /// A start before its launch, which has no job yet: only its own preflight inspection, `None`
    /// until the actor has scheduled it. A scan the user began is never cancelled.
    Start { inspection: Option<u64> },
}

/// Tears down `target`, and returns once it is torn down or the wait for it timed out.
pub fn stop_job_inner(
    state: &JobState,
    scan_state: &ScanState,
    target: StopTarget,
) -> Result<(), String> {
    match target {
        StopTarget::Job(job_id) => state.stop(Some(job_id), STOP_TIMEOUT),
        StopTarget::Start {
            inspection: Some(id),
        } => scan_state.stop_inspection(id, STOP_TIMEOUT),
        StopTarget::Start { inspection: None } => Ok(()),
    }
}

fn stop_scan(state: &ScanState) -> Result<(), String> {
    let _start_lock = state.start_lock.lock().map_err(|e| e.to_string())?;
    stop_registered_scan(state)
}

/// Cancels the active scan but leaves it registered: its `Inspection` vacates it when the
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
    if let Ok(pending) = scan_state.pending.lock() {
        for cancel_token in pending.values() {
            cancel_token.cancel();
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
/// A Run's resolve and its launch both go through it, so the settings it shows first and the job
/// agree on the directory and on every error.
pub fn resolve_canonical(mut request: BatchRequest) -> Result<PipelineConfig, String> {
    request.input_dir =
        std::fs::canonicalize(&request.input_dir).unwrap_or_else(|_| request.input_dir.clone());
    request.resolve()
}

/// The wire name of `mode`: what a Job Snapshot and a `job-done` carry.
pub(crate) fn mode_name(mode: PipelineMode) -> &'static str {
    match mode {
        PipelineMode::Extract => "extract",
        PipelineMode::Remux => "remux",
    }
}

/// Starts a job for `request` in `job`, reporting to `sink`: what `EngineLauncher::launch` runs,
/// and what a test runs against a recording sink.
pub fn launch(
    job: &JobState,
    sink: impl JobSink,
    cache_state: &InspectCacheState,
    request: BatchRequest,
) -> Result<u64, String> {
    let mut pipeline_config = resolve_canonical(request)?;
    let input_dir = display_canonical_path(&pipeline_config.input_dir.to_string_lossy());

    let mode = mode_name(pipeline_config.mode);
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

/// What the Job Coordinator needs to run a job. Each runs off the actor; the actor never awaits
/// one inline, and window close and `ExitRequested` tear down without it.
pub trait Launcher: Send + Sync + 'static {
    /// The Effective Settings `request` would run with, or why it would be refused.
    fn resolve(
        &self,
        request: BatchRequest,
    ) -> impl Future<Output = Result<EffectiveSettings, String>> + Send;

    /// Starts a job for `request` that reports to `sink`, and returns its ID.
    fn launch(
        &self,
        request: BatchRequest,
        sink: ActorSink,
    ) -> impl Future<Output = Result<u64, String>> + Send;

    /// Stops `target`: a launched job, two-phase, or a start before its launch together with its
    /// own preflight inspection, which it waits up to 2 s to end. Returns once that is torn down,
    /// or why it is not.
    fn stop(&self, target: StopTarget) -> impl Future<Output = Result<(), String>> + Send;
}

/// The `Launcher` of the app: the Job Slot, the Directory Inspection registry and the session
/// cache that Stop, Close and every Run share. Each call runs on the blocking pool: resolving
/// canonicalizes the input directory, and stopping waits on the slot.
pub struct EngineLauncher {
    pub job: JobState,
    pub scan: Arc<ScanState>,
    pub cache: InspectCacheState,
}

/// Runs `task` on the blocking pool of the current runtime.
async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|e| format!("Async runtime error: {e}"))?
}

impl Launcher for EngineLauncher {
    async fn resolve(&self, request: BatchRequest) -> Result<EffectiveSettings, String> {
        blocking(move || resolve_canonical(request).map(|config| config.effective_settings())).await
    }

    async fn launch(&self, request: BatchRequest, sink: ActorSink) -> Result<u64, String> {
        let (job, cache) = (self.job.clone(), self.cache.clone());
        blocking(move || launch(&job, sink, &cache, request)).await
    }

    async fn stop(&self, target: StopTarget) -> Result<(), String> {
        let (job, scan) = (self.job.clone(), Arc::clone(&self.scan));
        blocking(move || stop_job_inner(&job, &scan, target)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::test_support::{job_state, poison, runtime, RecordingSink};
    use crate::snapshot::JobSnapshot;

    /// Spins until `ready`, or for at most 5 s, so a test whose stop never comes fails instead of
    /// hanging. Returns whether `ready` came.
    fn spin_until(ready: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// Inspection `id` of `dir`, scheduled and registered, with its cancel token.
    fn begun(state: &Arc<ScanState>, id: u64, dir: &Path) -> (Inspection, CancellationToken) {
        let inspection = state.schedule(id);
        let token = inspection.begin(dir).unwrap();
        (inspection, token)
    }

    #[test]
    fn test_stopping_a_job_leaves_every_inspection_alone() {
        let scan_state = ScanState::default();
        let token = CancellationToken::new();
        *scan_state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        let job = job_state();
        job.occupy(7);
        assert!(stop_job_inner(&job, &scan_state, StopTarget::Job(99)).is_ok());
        assert!(stop_job_inner(&job_state(), &scan_state, StopTarget::Job(99)).is_ok());
        // A job's own Stop too, even while the registry is unusable.
        poison(&scan_state.start_lock);
        let job = job_state();
        job.occupy(7);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                assert!(
                    spin_until(|| job.current_cancelled()),
                    "the job was not stopped"
                );
                job.vacate();
            });
            assert_eq!(
                stop_job_inner(&job, &scan_state, StopTarget::Job(7)),
                Ok(())
            );
        });

        assert!(scan_state.is_active());
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_stopping_a_start_cancels_only_its_own_inspection() {
        let job = job_state();
        job.occupy(7);
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let (_user_scan, user_token) = begun(&scan_state, 1, tmp.path());

        // Its inspection has not begun, or has ended already: the user's scan runs on, and the
        // Stop has nothing to wait on.
        let none = StopTarget::Start { inspection: None };
        assert_eq!(stop_job_inner(&job, &scan_state, none), Ok(()));
        let ended = StopTarget::Start {
            inspection: Some(2),
        };
        assert_eq!(stop_job_inner(&job, &scan_state, ended), Ok(()));
        assert!(!user_token.is_cancelled());
        assert!(scan_state.is_active());
        assert!(!job.current_cancelled(), "a start has no job to stop");
    }

    #[test]
    fn test_the_engine_launcher_stops_a_start_through_its_inspection() {
        let launcher = EngineLauncher {
            job: job_state(),
            scan: Arc::default(),
            cache: InspectCacheState::default(),
        };
        let target = StopTarget::Start {
            inspection: Some(1),
        };
        let inspection = launcher.scan.schedule(1);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let refused = std::thread::scope(|scope| {
            let ending = scope.spawn(move || {
                let cancelled = spin_until(|| inspection.cancel_token.is_cancelled());
                (cancelled, inspection.begin(dir).is_err())
            });
            assert_eq!(runtime().block_on(launcher.stop(target)), Ok(()));
            ending.join().unwrap()
        });

        assert_eq!(refused, (true, true));
    }

    #[test]
    fn test_a_late_older_inspection_is_refused_and_leaves_the_newer_one_running() {
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let (newer, newer_token) = begun(&scan_state, 2, tmp.path());
        let older = scan_state.schedule(1);

        assert_eq!(
            older.begin(tmp.path()).err().as_deref(),
            Some(INSPECTION_CANCELLED)
        );
        assert!(!newer_token.is_cancelled());
        assert!(scan_state.is_active());
        // Once the newer one ended, nothing newer runs, and an older one may.
        drop(newer);
        assert!(older.begin(tmp.path()).is_ok());
    }

    #[test]
    fn test_a_start_stop_waits_until_its_still_canonicalizing_inspection_ends() {
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let (_user_scan, user_token) = begun(&scan_state, 1, dir);
        // Scheduled, not begun: stands in for one still canonicalizing its directory.
        let inspection = scan_state.schedule(2);
        let ended = AtomicBool::new(false);
        let ended_ref = &ended;

        std::thread::scope(|scope| {
            let ending = scope.spawn(move || {
                let cancelled = spin_until(|| inspection.cancel_token.is_cancelled());
                // What its begin answers once its directory is canonicalized.
                let begun = inspection.begin(dir).err();
                ended_ref.store(true, Ordering::SeqCst);
                drop(inspection);
                (cancelled, begun)
            });
            assert_eq!(scan_state.stop_inspection(2, STOP_TIMEOUT), Ok(()));
            assert!(
                ended.load(Ordering::SeqCst),
                "the Stop returned before its cancelled inspection ended"
            );
            let (cancelled, begun) = ending.join().unwrap();
            assert!(cancelled, "the Stop cancelled nothing");
            assert_eq!(begun.as_deref(), Some(INSPECTION_CANCELLED));
        });
        assert!(!user_token.is_cancelled(), "the refusal cancelled nothing");

        // Still canonicalizing past the wait: the Stop answers why.
        let _held = scan_state.schedule(3);
        let error = scan_state
            .stop_inspection(3, Duration::from_millis(20))
            .unwrap_err();
        assert!(error.contains("Timed out"), "{error}");
    }

    #[test]
    fn test_a_stopped_inspection_stays_cancelled_however_many_stops_follow_it() {
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let inspections: Vec<Inspection> = (1..=9).map(|id| scan_state.schedule(id)).collect();

        for id in 1..=9 {
            let error = scan_state
                .stop_inspection(id, Duration::from_millis(5))
                .unwrap_err();
            assert!(error.contains("Timed out"), "{id}: {error}");
        }

        // Each one is still pending, so each one is still cancelled, the first included.
        for inspection in &inspections {
            assert_eq!(
                inspection.begin(tmp.path()).err().as_deref(),
                Some(INSPECTION_CANCELLED),
                "{}",
                inspection.id
            );
        }
        assert!(!scan_state.is_active());
    }

    #[test]
    fn test_a_start_stop_waits_until_its_registered_inspection_ends() {
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let (inspection, token) = begun(&scan_state, 3, tmp.path());
        let released = AtomicBool::new(false);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Stands in for an inspection that ends a while after its cancel.
                spin_until(|| token.is_cancelled());
                std::thread::sleep(Duration::from_millis(100));
                released.store(true, Ordering::SeqCst);
                drop(inspection);
            });
            scan_state.stop_inspection(3, STOP_TIMEOUT).unwrap();
            assert!(
                released.load(Ordering::SeqCst),
                "the Stop returned before its cancelled inspection ended"
            );
        });

        let (_inspection, token) = begun(&scan_state, 4, tmp.path());
        let error = scan_state
            .stop_inspection(4, Duration::from_millis(50))
            .unwrap_err();
        assert!(error.contains("Timed out"), "{error}");
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_close_cancels_the_registered_and_the_still_canonicalizing_inspections() {
        let (job, scan_state) = (job_state(), Arc::new(ScanState::default()));
        let tmp = tempfile::tempdir().unwrap();
        let (inspection, token) = begun(&scan_state, 1, tmp.path());
        let canonicalizing = scan_state.schedule(2);

        assert!(begin_shutdown(&job, &scan_state));
        assert!(token.is_cancelled());
        assert!(canonicalizing.cancel_token.is_cancelled());
        // One still canonicalizing registers nothing once it gets there.
        assert!(canonicalizing.begin(tmp.path()).is_err());
        drop(inspection);
        // The cleanup does not wait on it: unregistered, it owns no child process.
        assert_eq!(cleanup_owned_processes(&job, &scan_state), Ok(()));
        assert!(!scan_state.is_active());

        // A cancelled close leaves it cancelled, as it leaves the registered one.
        cancel_shutdown(&job, &scan_state);
        assert_eq!(
            canonicalizing.begin(tmp.path()).err().as_deref(),
            Some(INSPECTION_CANCELLED)
        );
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
        // Still registered: only the inspection's own drop vacates it.
        assert!(state.is_active());
    }

    #[test]
    fn test_cleanup_waits_until_the_scan_registration_drops() {
        let (job, scan_state) = (job_state(), Arc::new(ScanState::default()));
        let tmp = tempfile::tempdir().unwrap();
        let (inspection, token) = begun(&scan_state, 1, tmp.path());
        let released = AtomicBool::new(false);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Stands in for an inspection that ends a while after its cancel.
                spin_until(|| token.is_cancelled());
                std::thread::sleep(Duration::from_millis(100));
                released.store(true, Ordering::SeqCst);
                drop(inspection);
            });
            cleanup_owned_processes(&job, &scan_state).unwrap();
            assert!(
                released.load(Ordering::SeqCst),
                "cleanup returned before the cancelled inspection ended"
            );
        });

        let (_inspection, _token) = begun(&scan_state, 2, tmp.path());
        let error =
            cleanup_owned_processes_with_timeout(&job, &scan_state, Duration::from_millis(50))
                .unwrap_err();
        assert!(error.contains("Directory inspection cleanup failed"));
        assert!(error.contains("Timed out"));
    }

    #[test]
    fn test_begin_replaces_and_cancels_the_previous_scan() {
        let scan_state = Arc::new(ScanState::default());
        let tmp = tempfile::tempdir().unwrap();
        let (first, first_token) = begun(&scan_state, 1, tmp.path());
        let (_second, second_token) = begun(&scan_state, 2, tmp.path());
        assert!(first_token.is_cancelled());

        drop(first);
        assert!(scan_state.is_active(), "the old drop vacated the new scan");
        assert!(!second_token.is_cancelled());
        let missing = tmp.path().join("missing");
        assert!(scan_state.schedule(3).begin(&missing).is_err());
    }

    #[test]
    fn test_an_inspection_drop_clears_the_active_scan_and_ends_it() {
        let state = Arc::new(ScanState::default());
        let inspection = state.schedule(1);
        let token = CancellationToken::new();
        *state.active.lock().unwrap() = Some(ActiveScan {
            id: 1,
            cancel_token: token.clone(),
        });

        drop(inspection);

        assert!(!state.is_active());
        assert!(!token.is_cancelled());
        assert!(state.pending.lock().unwrap().is_empty());
    }

    /// What `launch` needs to run a request: a job state, a sink recording what its jobs report,
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

    /// What `EngineLauncher::resolve` answers for `request`, on the shared test runtime.
    fn resolve(request: BatchRequest) -> Result<EffectiveSettings, String> {
        let launcher = EngineLauncher {
            job: job_state(),
            scan: Arc::default(),
            cache: InspectCacheState::default(),
        };
        runtime().block_on(launcher.resolve(request))
    }

    /// The View State's `defaults` are what a run of an empty form resolves to.
    #[test]
    fn test_default_settings_match_a_resolved_empty_request() {
        let tmp = tempfile::tempdir().unwrap();
        let defaults = EffectiveSettings::defaults();
        assert_eq!(resolve(dry_run_request(tmp.path())), Ok(defaults.clone()));
        // `BatchRequest::resolve` accepts 1-64; the default is clamped to 1-16.
        assert!(
            (1..=16).contains(&defaults.workers),
            "workers out of range: {}",
            defaults.workers
        );
    }

    #[test]
    fn test_resolve_layers_explicit_values_over_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = resolve(BatchRequest {
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
    fn test_resolve_reports_the_first_validation_error() {
        let tmp = tempfile::tempdir().unwrap();
        let error = resolve(BatchRequest {
            workers: Some(0),
            io_jobs: Some(65),
            ..dry_run_request(tmp.path())
        })
        .unwrap_err();
        assert_eq!(error, "Worker thread count must be between 1 and 64");

        let missing = tmp.path().join("missing");
        let error = resolve(dry_run_request(&missing)).unwrap_err();
        assert!(error.contains("Input directory does not exist"), "{error}");
    }

    #[test]
    fn test_launch_reports_the_effective_settings_to_its_sink() {
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
        assert!(snapshot.ordered_item_ids.is_empty());
        assert_eq!(bridge.done_code(), 0);
    }

    #[test]
    fn test_launch_reports_the_canonical_linked_input_dir_to_its_sink() {
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
            "link creation failed; launch input_dir canonicalization is untested"
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
    fn test_launch_preserves_non_utf8_canonical_input_path() {
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
    fn launch_shares_the_session_cache_unless_no_cache() {
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
