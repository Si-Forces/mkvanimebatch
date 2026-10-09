//! Batch pipeline execution, event streams, and cancellation coordination.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub use crate::domain::ItemOutcome;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Semaphore};
pub use tokio_util::sync::CancellationToken;

use crate::assets::{claim_leftover_chapters, ReleasePreparer};
use crate::domain::{resolve_output_dir, IO_GOVERNOR_CLOSED};
use crate::extract::extract_mkv_file;
use crate::fonts::{FontHashMemo, FontRegistry};
use crate::inspect::inspect_videos;
use crate::item_id::ItemId;
use crate::media_tool::{MediaToolRunner, SubprocessRunner};
use crate::remux::{execute_remux_job, RemuxOptions};
use crate::remux_plan::{PreparedDir, RemuxPlan, RemuxPlanner, RemuxWork};
use crate::request::{resolve_selected_items, EffectiveSettings, TrackAssignment};
use crate::scan::{
    get_or_probe_video, scan_mkv_files, InspectedVideo, InspectionResult, ParsedFile,
    PersistentInspectCache,
};

pub const EVENT_VERSION: u64 = 1;

/// `reason` of every item settled by a stop request.
pub const CANCELLED_REASON: &str = "Operation cancelled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchItemInfo {
    pub id: ItemId,
    pub file: String,
}

impl BatchItemInfo {
    /// Identity of a discovered file: its [`ItemId`] under `input_dir` and its scanned
    /// file name. Taking the `ParsedFile` reuses the name discovery already computed, so every
    /// event reports exactly the name the scan produced.
    pub(crate) fn new(file: &ParsedFile, input_dir: &Path) -> Self {
        Self {
            id: ItemId::of(&file.path, input_dir),
            file: file.name.clone(),
        }
    }
}

/// One step of a run, streamed to the CLI and the desktop. Item events carry the item's
/// [`ItemId`]; the desktop snapshot and the JS reducer key items by its string form.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    BatchStart {
        version: u64,
        total: usize,
        input_dir: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        items: Option<Vec<BatchItemInfo>>,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemStart {
        version: u64,
        id: ItemId,
        file: String,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemProgress {
        version: u64,
        id: ItemId,
        file: String,
        progress: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        speed: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        eta: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemComplete {
        version: u64,
        id: ItemId,
        file: String,
        status: ItemOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_sec: Option<f64>,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    BatchComplete {
        version: u64,
        success: bool,
        cancelled: bool,
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
}

impl JobEvent {
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(format!("@event: {json}"))
    }
}

pub type EventSender = mpsc::Sender<JobEvent>;
pub type EventReceiver = mpsc::Receiver<JobEvent>;

pub fn create_event_channel(buffer_size: usize) -> (EventSender, EventReceiver) {
    mpsc::channel(buffer_size)
}

pub type LogSender = mpsc::Sender<String>;
pub type LogReceiver = mpsc::Receiver<String>;

pub fn create_log_channel(buffer_size: usize) -> (LogSender, LogReceiver) {
    mpsc::channel(buffer_size)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineMode {
    Extract,
    Remux,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub input_dir: PathBuf,
    pub out_dir: Option<PathBuf>,
    pub fonts_dir: Option<PathBuf>,
    pub assets_dir: Option<PathBuf>,
    pub mode: PipelineMode,
    pub dry_run: bool,
    pub no_cache: bool,
    pub cache: Option<Arc<PersistentInspectCache>>,
    pub font_hash_memo: Option<Arc<parking_lot::Mutex<FontHashMemo>>>,
    pub workers: usize,
    pub io_jobs: usize,
    pub bitrate: String,
    pub complexity: u32,
    /// ffmpeg `-threads` per transcode: the FLAC decoder thread cap (ADR-0013, 2026-10-05
    /// correction, #36).
    pub ffmpeg_threads: usize,
    /// Global subtitle sync offset in milliseconds; a Track Assignment's `offset_ms` replaces it.
    pub default_sub_offset_ms: i64,
    /// Track Assignments for Remux; every target must match exactly one discovered item.
    pub track_assignments: Option<Vec<TrackAssignment>>,
    pub selected_items: Option<Vec<String>>,
}

impl PipelineConfig {
    /// The concurrency and audio transcoding values this config runs with.
    pub fn effective_settings(&self) -> EffectiveSettings {
        EffectiveSettings {
            workers: self.workers,
            io_jobs: self.io_jobs,
            ffmpeg_threads: self.ffmpeg_threads,
            bitrate: self.bitrate.clone(),
            complexity: self.complexity,
        }
    }
}

/// A fixed, deterministic fixture for tests and benches, not the production defaults: those
/// come from `BatchRequest::resolve`.
impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            input_dir: PathBuf::from("."),
            out_dir: None,
            fonts_dir: None,
            assets_dir: None,
            mode: PipelineMode::Remux,
            dry_run: false,
            no_cache: false,
            cache: None,
            font_hash_memo: None,
            workers: 4,
            io_jobs: 2,
            bitrate: "192k".to_string(),
            complexity: 5,
            ffmpeg_threads: 2,
            default_sub_offset_ms: 0,
            track_assignments: None,
            selected_items: None,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct PipelineSummary {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub cancelled: bool,
    pub log: Vec<String>,
}

fn cancelled_item(item: BatchItemInfo, duration_sec: Option<f64>) -> JobEvent {
    let BatchItemInfo { id, file } = item;
    JobEvent::ItemComplete {
        version: EVENT_VERSION,
        id,
        file,
        status: ItemOutcome::Cancelled,
        reason: Some(CANCELLED_REASON.into()),
        error: None,
        duration_sec,
        extra: Default::default(),
    }
}

/// The terminal event of an item that planning skipped.
fn skipped_item(item: BatchItemInfo, reason: String) -> JobEvent {
    let BatchItemInfo { id, file } = item;
    JobEvent::ItemComplete {
        version: EVENT_VERSION,
        id,
        file,
        status: ItemOutcome::Skipped,
        reason: Some(reason),
        error: None,
        duration_sec: None,
        extra: Default::default(),
    }
}

/// The terminal event of an item that planning failed.
fn errored_item(item: BatchItemInfo, message: String) -> JobEvent {
    let BatchItemInfo { id, file } = item;
    JobEvent::ItemComplete {
        version: EVENT_VERSION,
        id,
        file,
        status: ItemOutcome::Error,
        reason: None,
        error: Some(message),
        duration_sec: None,
        extra: Default::default(),
    }
}

fn planned_item(item: BatchItemInfo) -> JobEvent {
    let BatchItemInfo { id, file } = item;
    JobEvent::ItemComplete {
        version: EVENT_VERSION,
        id,
        file,
        status: ItemOutcome::Planned,
        reason: None,
        error: None,
        duration_sec: None,
        extra: Default::default(),
    }
}

fn fonts_dir_for(
    mkv_dir: &Path,
    input_dir: &Path,
    out_override: Option<&Path>,
    fonts_override: Option<&Path>,
) -> PathBuf {
    if let Some(f) = fonts_override {
        return f.to_path_buf();
    }
    resolve_output_dir(&mkv_dir.join("placeholder.mkv"), input_dir, out_override).join("fonts")
}

/// Mirrors `mkv_dir`'s position under `input_dir` inside `assets`, falling back to the
/// assets root when no mirrored directory exists.
fn release_assets_dir(assets: &Path, mkv_dir: &Path, input_dir: &Path) -> PathBuf {
    let rel = mkv_dir
        .strip_prefix(input_dir)
        .map(Path::to_path_buf)
        .ok()
        .or_else(|| {
            let mkv_dir = mkv_dir.canonicalize().ok()?;
            let input_dir = input_dir.canonicalize().ok()?;
            mkv_dir.strip_prefix(&input_dir).map(Path::to_path_buf).ok()
        });
    if let Some(rel) = rel {
        let candidate = assets.join(rel);
        if candidate.is_dir() {
            return candidate;
        }
    }
    assets.to_path_buf()
}

/// Rejects discovered files that share an [`ItemId`], then narrows the batch to
/// `selected_items` in selection order. Every selection must name a discovered file.
fn apply_selected_items(
    files: Vec<ParsedFile>,
    input_dir: &Path,
    selected_items: Option<&[String]>,
) -> Result<Vec<ParsedFile>, String> {
    let mut index_by_id = HashMap::with_capacity(files.len());
    for (idx, file) in files.iter().enumerate() {
        match index_by_id.entry(ItemId::of(&file.path, input_dir)) {
            Entry::Vacant(entry) => {
                entry.insert(idx);
            }
            Entry::Occupied(entry) => {
                return Err(format!(
                    "Discovered MKV files share canonical ID '{}'",
                    entry.key()
                ));
            }
        }
    }
    let Some(selected) = selected_items else {
        return Ok(files);
    };

    let order = resolve_selected_items(&index_by_id, selected)?;
    let mut slots: Vec<Option<ParsedFile>> = files.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .filter_map(|idx| slots[idx].take())
        .collect())
}

/// Requires every assignment target to match exactly one discovered video's [`ItemId`].
///
/// A manual assignment that cannot be consumed fails closed: it is never silently dropped in
/// favor of automatic resolution. The check runs after discovery and selection, so the later
/// per-asset existence check only has to cover assets that disappear after preflight.
fn validate_assignment_targets(
    assignments: &[TrackAssignment],
    files: &[ParsedFile],
    input_dir: &Path,
    selected_items: Option<&[String]>,
) -> Result<(), String> {
    if assignments.is_empty() {
        return Ok(());
    }

    let mut discovered: HashMap<ItemId, usize> = HashMap::with_capacity(files.len());
    for file in files {
        *discovered
            .entry(ItemId::of(&file.path, input_dir))
            .or_default() += 1;
    }

    let mut unmatched_seen = HashSet::with_capacity(assignments.len());
    let mut unmatched: Vec<String> = Vec::new();
    for assignment in assignments {
        let target = assignment.target_id();
        if discovered.get(&target) != Some(&1) && unmatched_seen.insert(target.clone()) {
            unmatched.push(target.into());
        }
    }
    if unmatched.is_empty() {
        return Ok(());
    }

    let scope = if selected_items.is_some() {
        "the active batch selection"
    } else {
        "the discovered MKV files"
    };
    Err(format!(
        "Track assignment targets do not match exactly one video in {scope}: {}",
        unmatched.join(", ")
    ))
}

/// Orders planning outcomes by batch position, settling every item without one as cancelled.
fn settle_cancelled_planning_items(
    files: &[ParsedFile],
    input_dir: &Path,
    outcomes: Vec<JobEvent>,
) -> Vec<JobEvent> {
    let mut outcomes_by_id = HashMap::with_capacity(outcomes.len());
    for outcome in outcomes {
        let id = match &outcome {
            JobEvent::ItemComplete { id, .. } => id.clone(),
            _ => unreachable!("planning outcomes are item completions"),
        };
        outcomes_by_id.insert(id, outcome);
    }

    files
        .iter()
        .map(|file| {
            let item = BatchItemInfo::new(file, input_dir);
            outcomes_by_id
                .remove(&item.id)
                .unwrap_or_else(|| cancelled_item(item, None))
        })
        .collect()
}

/// Runs the Asset Resolver and font scan over each Release Directory. The pass is synchronous
/// file I/O with no `.await`, so `cancel_token` is checked before each directory; returns
/// `None` once it is cancelled.
pub(crate) fn prepare_release_dirs<'a>(
    videos_by_dir: &BTreeMap<&'a Path, Vec<ParsedFile>>,
    all_inspections: &HashMap<PathBuf, InspectionResult>,
    config: &PipelineConfig,
    cancel_token: &CancellationToken,
) -> Option<HashMap<&'a Path, PreparedDir>> {
    let mut preparer = ReleasePreparer::default();
    let mut prepared_dirs = HashMap::with_capacity(videos_by_dir.len());
    for (&mkv_dir, dir_videos) in videos_by_dir {
        if cancel_token.is_cancelled() {
            return None;
        }
        let resolved_assets_dir = config
            .assets_dir
            .as_deref()
            .map(|assets| release_assets_dir(assets, mkv_dir, &config.input_dir));
        let prepared = preparer.prepare(
            mkv_dir,
            &config.input_dir,
            config.fonts_dir.as_deref(),
            resolved_assets_dir.as_deref(),
        );
        let font_info = preparer.fonts(&prepared.fonts_dir);
        let chapter_claims =
            claim_leftover_chapters(dir_videos, all_inspections, &prepared.resolver);

        prepared_dirs.insert(
            mkv_dir,
            PreparedDir {
                resolver: prepared.resolver,
                font_info: Arc::new(font_info),
                chapter_claims,
            },
        );
    }
    Some(prepared_dirs)
}

/// Why a spawned batch item did not complete.
enum ItemFailure {
    Cancelled,
    Failed(String),
}

/// Best-effort progress reporting for one item; a full event channel drops the update.
struct ProgressSink {
    tx: Option<EventSender>,
    item: BatchItemInfo,
}

impl ProgressSink {
    fn report(&self, progress: f64) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(JobEvent::ItemProgress {
                version: EVENT_VERSION,
                id: self.item.id.clone(),
                file: self.item.file.clone(),
                progress,
                speed: None,
                eta: None,
                extra: Default::default(),
            });
        }
    }
}

pub struct BatchPipeline {
    config: PipelineConfig,
    event_sender: Option<EventSender>,
    log_sender: Option<LogSender>,
    media_tool_runner: Arc<dyn MediaToolRunner>,
    cancel_token: CancellationToken,
}

impl BatchPipeline {
    pub fn new(
        config: PipelineConfig,
        event_sender: Option<EventSender>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            config,
            event_sender,
            log_sender: None,
            media_tool_runner: Arc::new(SubprocessRunner),
            cancel_token,
        }
    }

    pub fn with_log_sender(mut self, log_sender: LogSender) -> Self {
        self.log_sender = Some(log_sender);
        self
    }

    /// Runs the `ffmpeg`, `mkvmerge`, and `mkvextract` invocations of remux and extract items
    /// through `runner` instead of spawning child processes. The Fallback Inspector's
    /// `mkvmerge -J` is not routed through it, so a run that must not spawn a tool needs an
    /// inspection cache that already holds every video.
    pub fn with_media_tool_runner(mut self, runner: Arc<dyn MediaToolRunner>) -> Self {
        self.media_tool_runner = runner;
        self
    }

    async fn log(&self, msg: impl Into<String>) {
        if let Some(tx) = &self.log_sender {
            let _ = tx.send(msg.into()).await;
        }
    }

    async fn log_batch(&self, msgs: &[String]) {
        if let Some(tx) = &self.log_sender {
            for msg in msgs {
                let _ = tx.send(msg.clone()).await;
            }
        }
    }

    async fn emit(&self, event: JobEvent) {
        if let Some(tx) = &self.event_sender {
            let _ = tx.send(event).await;
        }
    }

    async fn emit_batch_complete(&self, success: bool, cancelled: bool) {
        self.emit(JobEvent::BatchComplete {
            version: EVENT_VERSION,
            success,
            cancelled,
            extra: Default::default(),
        })
        .await;
    }

    /// Emits the terminal event for a batch that ran to its end or observed a stop.
    async fn finish(&self, summary: PipelineSummary) -> PipelineSummary {
        self.emit_batch_complete(summary.errors == 0 && !summary.cancelled, summary.cancelled)
            .await;
        summary
    }

    fn inspect_cache(&self) -> Arc<PersistentInspectCache> {
        self.config
            .cache
            .clone()
            .unwrap_or_else(|| Arc::new(PersistentInspectCache::new(None, !self.config.no_cache)))
    }

    fn max_workers(&self) -> usize {
        self.config.workers.max(1)
    }

    /// Bounds concurrent disk-heavy work to `io_jobs`. `BatchRequest::resolve` has already turned
    /// a requested `0` into the worker count; the `max(1)` keeps a hand-built config's `0` from
    /// creating a semaphore nothing can acquire.
    fn io_semaphore(&self) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(self.config.io_jobs.max(1)))
    }

    pub async fn run(&self) -> Result<PipelineSummary, String> {
        match self.config.mode {
            PipelineMode::Extract => self.run_extract().await,
            PipelineMode::Remux => self.run_remux().await,
        }
    }

    async fn prepare_batch_and_emit_start(
        &self,
    ) -> Result<(Vec<ParsedFile>, PipelineSummary), String> {
        let mut summary = PipelineSummary::default();
        let files = scan_mkv_files(&self.config.input_dir, true, None);
        let files = apply_selected_items(
            files,
            &self.config.input_dir,
            self.config.selected_items.as_deref(),
        )?;

        if self.config.mode == PipelineMode::Remux {
            // Fail closed before any batch or media work: a supplied assignment that no discovered
            // item can consume aborts the run instead of falling back to automatic resolution.
            validate_assignment_targets(
                self.config.track_assignments.as_deref().unwrap_or_default(),
                &files,
                &self.config.input_dir,
                self.config.selected_items.as_deref(),
            )?;
        }
        summary.total = files.len();

        let items: Vec<BatchItemInfo> = files
            .iter()
            .map(|f| BatchItemInfo::new(f, &self.config.input_dir))
            .collect();
        self.emit(JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: summary.total,
            input_dir: self.config.input_dir.to_string_lossy().into_owned(),
            items: Some(items),
            extra: Default::default(),
        })
        .await;

        if files.is_empty() {
            let msg = format!(
                "No MKV files found in '{}'",
                self.config.input_dir.display()
            );
            self.log(&msg).await;
            summary.log.push(msg);
            self.emit_batch_complete(true, false).await;
        }

        Ok((files, summary))
    }

    /// Runs `items` as `(item, payload)` on at most `max_workers` concurrent tasks,
    /// emitting each item's start and terminal events.
    ///
    /// `start` builds an item's work future, which observes `cancel_token` itself. Once a stop
    /// is observed, in-flight tasks are aborted and every item still in flight or unstarted is
    /// settled as cancelled.
    async fn run_items<T, F>(
        &self,
        summary: &mut PipelineSummary,
        // A Vec rather than `impl Iterator`: a borrowing `map` adapter held across the awaits
        // below makes the batch future fail the `Send` check callers spawn it under.
        items: Vec<(BatchItemInfo, T)>,
        mut start: impl FnMut(T, ProgressSink) -> F,
        failure_label: &str,
    ) where
        F: Future<Output = Result<Vec<String>, ItemFailure>> + Send + 'static,
    {
        let max_workers = self.max_workers();
        let mut pending = items.into_iter().peekable();
        let mut join_set = tokio::task::JoinSet::new();
        // Keyed by task id so a panicked task, whose output is lost, still resolves to its item.
        let mut in_flight: HashMap<tokio::task::Id, (BatchItemInfo, Instant)> = HashMap::new();

        while pending.peek().is_some() || !join_set.is_empty() {
            if self.cancel_token.is_cancelled() {
                summary.cancelled = true;
                break;
            }

            while join_set.len() < max_workers && pending.peek().is_some() {
                if self.cancel_token.is_cancelled() {
                    summary.cancelled = true;
                    break;
                }
                let Some((item, payload)) = pending.next() else {
                    break;
                };

                let work = start(
                    payload,
                    ProgressSink {
                        tx: self.event_sender.clone(),
                        item: item.clone(),
                    },
                );
                let started = Instant::now();
                self.emit(JobEvent::ItemStart {
                    version: EVENT_VERSION,
                    id: item.id.clone(),
                    file: item.file.clone(),
                    extra: Default::default(),
                })
                .await;

                let cancel_token = self.cancel_token.clone();
                let task = join_set.spawn(async move {
                    let outcome = if cancel_token.is_cancelled() {
                        Err(ItemFailure::Cancelled)
                    } else {
                        work.await
                    };
                    (outcome, started.elapsed().as_secs_f64())
                });
                in_flight.insert(task.id(), (item, started));
            }

            let Some(joined) = join_set.join_next_with_id().await else {
                continue;
            };
            let (task_id, joined) = match joined {
                Ok((task_id, result)) => (task_id, Ok(result)),
                Err(join_err) => (join_err.id(), Err(join_err)),
            };
            let Some((item, started)) = in_flight.remove(&task_id) else {
                continue;
            };
            let (outcome, duration) = joined.unwrap_or_else(|join_err| {
                let failure = if join_err.is_panic() {
                    ItemFailure::Failed(format!("worker panicked: {join_err}"))
                } else {
                    ItemFailure::Cancelled
                };
                (Err(failure), started.elapsed().as_secs_f64())
            });
            match outcome {
                Ok(logs) => {
                    summary.completed += 1;
                    self.log_batch(&logs).await;
                    summary.log.extend(logs);
                    let BatchItemInfo { id, file } = item;
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id,
                        file,
                        status: ItemOutcome::Completed,
                        reason: None,
                        error: None,
                        duration_sec: Some(duration),
                        extra: Default::default(),
                    })
                    .await;
                }
                Err(ItemFailure::Cancelled) => {
                    summary.cancelled = true;
                    self.emit(cancelled_item(item, Some(duration))).await;
                }
                Err(ItemFailure::Failed(err)) => {
                    summary.errors += 1;
                    let BatchItemInfo { id, file } = item;
                    let msg = format!("  ERROR: {failure_label} failed for '{file}': {err}");
                    self.log(&msg).await;
                    summary.log.push(msg);
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id,
                        file,
                        status: ItemOutcome::Error,
                        reason: None,
                        error: Some(err),
                        duration_sec: Some(duration),
                        extra: Default::default(),
                    })
                    .await;
                }
            }
        }

        if summary.cancelled {
            join_set.abort_all();
            while join_set.join_next().await.is_some() {}
            for (_, (item, started)) in in_flight.drain() {
                self.emit(cancelled_item(item, Some(started.elapsed().as_secs_f64())))
                    .await;
            }
            for (item, _) in pending {
                self.emit(cancelled_item(item, None)).await;
            }
        }
    }

    async fn run_extract(&self) -> Result<PipelineSummary, String> {
        let (files, mut summary) = self.prepare_batch_and_emit_start().await?;
        if files.is_empty() {
            return Ok(summary);
        }

        let cache = self.inspect_cache();

        if self.config.dry_run {
            for (idx, file) in files.iter().enumerate() {
                if self.cancel_token.is_cancelled() {
                    summary.cancelled = true;
                    for rem in &files[idx..] {
                        let item = BatchItemInfo::new(rem, &self.config.input_dir);
                        self.emit(cancelled_item(item, None)).await;
                    }
                    break;
                }

                let msg = format!(
                    "[DRY-RUN] Would extract assets from: {}",
                    file.path.display()
                );
                self.log(&msg).await;
                summary.log.push(msg);
                self.emit(planned_item(BatchItemInfo::new(
                    file,
                    &self.config.input_dir,
                )))
                .await;
                summary.completed += 1;
            }

            if !self.config.no_cache {
                cache.save();
            }
            return Ok(self.finish(summary).await);
        }

        let io_semaphore = self.io_semaphore();
        let mut font_registries: HashMap<PathBuf, Arc<FontRegistry>> = HashMap::new();
        let font_memo: Option<Arc<parking_lot::Mutex<FontHashMemo>>> = (!self.config.no_cache)
            .then(|| {
                self.config.font_hash_memo.clone().unwrap_or_else(|| {
                    Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
                        crate::scan::get_default_cache_dir().join("font_hash_memo.json"),
                    )))
                })
            });

        let items: Vec<_> = files
            .iter()
            .map(|file| (BatchItemInfo::new(file, &self.config.input_dir), file))
            .collect();
        self.run_items(
            &mut summary,
            items,
            |file, progress| {
                let mkv_dir = file.path.parent().unwrap_or(Path::new("."));
                let fonts_dir = fonts_dir_for(
                    mkv_dir,
                    &self.config.input_dir,
                    self.config.out_dir.as_deref(),
                    self.config.fonts_dir.as_deref(),
                );
                let font_registry = font_registries
                    .entry(fonts_dir.clone())
                    .or_insert_with(|| Arc::new(FontRegistry::new(fonts_dir, font_memo.clone())))
                    .clone();
                let target_out_dir =
                    self.config.out_dir.as_deref().map(|out| {
                        resolve_output_dir(&file.path, &self.config.input_dir, Some(out))
                    });
                let file_path = file.path.clone();
                let cancel_token = self.cancel_token.clone();
                let io_semaphore = Arc::clone(&io_semaphore);
                let runner = Arc::clone(&self.media_tool_runner);
                let cache = Arc::clone(&cache);
                let no_cache = self.config.no_cache;

                async move {
                    let cache = if no_cache { None } else { Some(&*cache) };
                    let inspected = match get_or_probe_video(&file_path, cache).await {
                        Ok(meta) => InspectedVideo::new(meta),
                        Err(err) => return Err(ItemFailure::Failed(err)),
                    };

                    let _io_permit = tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => return Err(ItemFailure::Cancelled),
                        permit = io_semaphore.acquire() => permit
                            .map_err(|_| ItemFailure::Failed(IO_GOVERNOR_CLOSED.into()))?,
                    };

                    let on_progress = |pct: f64| progress.report(pct);
                    tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => Err(ItemFailure::Cancelled),
                        res = extract_mkv_file(
                            &file_path,
                            &inspected,
                            &font_registry,
                            target_out_dir.as_deref(),
                            Some(&on_progress),
                            &*runner,
                        ) => res.map(|extracted| extracted.log).map_err(ItemFailure::Failed),
                    }
                }
            },
            "Extraction",
        )
        .await;

        for registry in font_registries.values() {
            registry.cleanup_staging();
        }
        if let Some(memo) = &font_memo {
            memo.lock().save();
        }
        if !self.config.no_cache {
            cache.save();
        }

        Ok(self.finish(summary).await)
    }

    /// `abort_log` holds the lines planning collected before the abort that still describe
    /// what happened (skips and errors, not dry-run predictions). They are sent ahead of the
    /// abort line and kept in the summary.
    async fn settle_planning_failure(
        &self,
        files: &[ParsedFile],
        failed_idx: usize,
        err: String,
        mut abort_log: Vec<String>,
    ) -> PipelineSummary {
        let msg = format!(
            "  ERROR: Remux planning failed for '{}': {err}",
            files[failed_idx].name
        );
        abort_log.push(msg);
        self.log_batch(&abort_log).await;

        let cancelled = self.cancel_token.is_cancelled();
        let aborted = format!(
            "Batch planning aborted because '{}': {err}",
            files[failed_idx].name
        );

        let summary = PipelineSummary {
            total: files.len(),
            errors: if cancelled { 0 } else { files.len() },
            cancelled,
            log: abort_log,
            ..Default::default()
        };
        for (idx, file) in files.iter().enumerate() {
            let item = BatchItemInfo::new(file, &self.config.input_dir);
            let event = if cancelled {
                cancelled_item(item, None)
            } else {
                JobEvent::ItemComplete {
                    version: EVENT_VERSION,
                    id: item.id,
                    file: item.file,
                    status: ItemOutcome::Error,
                    reason: None,
                    error: Some(if idx == failed_idx {
                        err.clone()
                    } else {
                        aborted.clone()
                    }),
                    duration_sec: None,
                    extra: Default::default(),
                }
            };
            self.emit(event).await;
        }
        self.emit_batch_complete(false, cancelled).await;
        summary
    }

    /// Settles every item as cancelled after a stop that lands before planning.
    async fn cancel_before_planning(
        &self,
        files: &[ParsedFile],
        mut summary: PipelineSummary,
    ) -> PipelineSummary {
        summary.cancelled = true;
        for file in files {
            let item = BatchItemInfo::new(file, &self.config.input_dir);
            self.emit(cancelled_item(item, None)).await;
        }
        self.emit_batch_complete(false, true).await;
        summary
    }

    async fn run_remux(&self) -> Result<PipelineSummary, String> {
        let (files, mut summary) = self.prepare_batch_and_emit_start().await?;
        if files.is_empty() {
            return Ok(summary);
        }

        let mut videos_by_dir: BTreeMap<&Path, Vec<ParsedFile>> = BTreeMap::new();
        for file in &files {
            let parent = file.path.parent().unwrap_or(Path::new("."));
            videos_by_dir.entry(parent).or_default().push(file.clone());
        }

        let cache = self.inspect_cache();

        // Probe every batch video concurrently, across release directories, before planning.
        let Some(inspections) = inspect_videos(
            &files,
            (!self.config.no_cache).then_some(&cache),
            self.max_workers(),
            &self.cancel_token,
        )
        .await
        else {
            return Ok(self.cancel_before_planning(&files, summary).await);
        };
        let all_inspections: HashMap<PathBuf, InspectionResult> = files
            .iter()
            .map(|file| file.path.clone())
            .zip(inspections)
            .collect();

        let Some(prepared_dirs) = prepare_release_dirs(
            &videos_by_dir,
            &all_inspections,
            &self.config,
            &self.cancel_token,
        ) else {
            return Ok(self.cancel_before_planning(&files, summary).await);
        };

        let mut remux_work: Vec<(BatchItemInfo, RemuxWork)> = Vec::new();
        let mut planning_outcomes = Vec::new();
        let mut planning_error: Option<(usize, String)> = None;
        let mut abort_log: Vec<String> = Vec::new();

        let planner = RemuxPlanner::new(
            &self.config.input_dir,
            self.config.out_dir.as_deref(),
            self.config.assets_dir.as_deref(),
            self.config.track_assignments.as_deref().unwrap_or_default(),
            &prepared_dirs,
            &all_inspections,
        );

        // Planning follows batch order (selection order when selected), not directory order.
        for (file_idx, video_file) in files.iter().enumerate() {
            if self.cancel_token.is_cancelled() {
                summary.cancelled = true;
                break;
            }

            let item = BatchItemInfo::new(video_file, &self.config.input_dir);
            match planner.plan(video_file) {
                RemuxPlan::Abort { message } => {
                    planning_error = Some((file_idx, message));
                    break;
                }
                RemuxPlan::Skip { reason } => {
                    summary.skipped += 1;
                    let line = format!("Skipping '{}': {reason}", video_file.name);
                    abort_log.push(line.clone());
                    summary.log.push(line);
                    planning_outcomes.push(skipped_item(item, reason));
                }
                RemuxPlan::ProbeFailed { message } => {
                    summary.errors += 1;
                    let line = format!(
                        "  ERROR: Failed to probe '{}': {message}",
                        video_file.path.display()
                    );
                    abort_log.push(line.clone());
                    summary.log.push(line);
                    planning_outcomes.push(errored_item(item, message));
                }
                RemuxPlan::Internal { message } => {
                    summary.errors += 1;
                    let line = format!("  ERROR: '{}': {message}", video_file.path.display());
                    abort_log.push(line.clone());
                    summary.log.push(line);
                    planning_outcomes.push(errored_item(item, message));
                }
                RemuxPlan::Planned(work) if self.config.dry_run => {
                    summary.log.push(format!(
                        "[DRY-RUN] Would remux '{}' -> '{}'",
                        video_file.name,
                        work.job.output_file.display()
                    ));
                    summary.log.extend(work.job.chapter_warning);
                    planning_outcomes.push(planned_item(item));
                    summary.completed += 1;
                }
                RemuxPlan::Planned(work) => remux_work.push((item, *work)),
            }
        }

        if let Some((failed_idx, err)) = planning_error {
            return Ok(self
                .settle_planning_failure(&files, failed_idx, err, abort_log)
                .await);
        }

        if summary.cancelled {
            planning_outcomes =
                settle_cancelled_planning_items(&files, &self.config.input_dir, planning_outcomes);
        }

        self.log_batch(&summary.log).await;
        for outcome in planning_outcomes {
            self.emit(outcome).await;
        }

        if !summary.cancelled && !remux_work.is_empty() {
            let io_semaphore = self.io_semaphore();
            let bitrate: Arc<str> = Arc::from(self.config.bitrate.as_str());
            let complexity = self.config.complexity;
            let ffmpeg_threads = self.config.ffmpeg_threads;
            let default_sub_offset_ms = self.config.default_sub_offset_ms;

            self.run_items(
                &mut summary,
                remux_work,
                |work, progress| {
                    let cancel_token = self.cancel_token.clone();
                    let io_semaphore = Arc::clone(&io_semaphore);
                    let runner = Arc::clone(&self.media_tool_runner);
                    let bitrate = Arc::clone(&bitrate);

                    async move {
                        let options = RemuxOptions {
                            opus_bitrate: &bitrate,
                            complexity,
                            ffmpeg_threads,
                            default_sub_offset_ms,
                        };
                        let on_progress = |pct: f64| progress.report(pct);
                        tokio::select! {
                            biased;
                            _ = cancel_token.cancelled() => Err(ItemFailure::Cancelled),
                            res = execute_remux_job(
                                &work.job,
                                &work.selection,
                                &work.font_info,
                                &options,
                                Some(&on_progress),
                                &io_semaphore,
                                &*runner,
                            ) => res.map_err(ItemFailure::Failed),
                        }
                    }
                },
                "Remux",
            )
            .await;
        }

        if !self.config.no_cache {
            cache.save();
        }

        Ok(self.finish(summary).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::create_parsed_file;

    fn parsed_file(name: &str) -> ParsedFile {
        let path = Path::new("/anime").join(name);
        ParsedFile {
            stem: path.file_stem().unwrap().to_string_lossy().into_owned(),
            name: name.into(),
            path,
            parsed: None,
        }
    }

    fn pipeline_with(io_jobs: usize, workers: usize) -> BatchPipeline {
        let config = PipelineConfig {
            io_jobs,
            workers,
            ..PipelineConfig::default()
        };
        BatchPipeline::new(config, None, CancellationToken::new())
    }

    #[test]
    fn test_io_semaphore_uses_io_jobs() {
        assert_eq!(pipeline_with(3, 4).io_semaphore().available_permits(), 3);
        // `BatchRequest::resolve` never yields 0; a hand-built config's 0 still gets a permit.
        assert_eq!(pipeline_with(0, 4).io_semaphore().available_permits(), 1);
    }

    #[test]
    fn test_fonts_dir_for() {
        let input_dir = Path::new("/anime");
        let mkv_dir = Path::new("/anime/Season 1");
        let out_dir = Path::new("/output");
        let fonts_override = Path::new("/custom/fonts");

        // 1. Fonts override takes precedence
        let res1 = fonts_dir_for(mkv_dir, input_dir, Some(out_dir), Some(fonts_override));
        assert_eq!(res1, fonts_override);

        // 2. Out override resolves nested release
        let res2 = fonts_dir_for(mkv_dir, input_dir, Some(out_dir), None);
        assert_eq!(res2, PathBuf::from("/output/Season 1/fonts"));

        // 3. Default joins mkv_dir / "fonts"
        let res3 = fonts_dir_for(mkv_dir, input_dir, None, None);
        assert_eq!(res3, PathBuf::from("/anime/Season 1/fonts"));
    }

    #[test]
    fn release_dir_preparation_stops_before_a_directory_once_cancelled() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = PipelineConfig {
            input_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut video = parsed_file("ep01.mkv");
        video.path = temp_dir.path().join("ep01.mkv");
        let videos_by_dir = BTreeMap::from([(temp_dir.path(), vec![video])]);
        let inspections = HashMap::new();
        let cancel_token = CancellationToken::new();

        let prepared = prepare_release_dirs(&videos_by_dir, &inspections, &config, &cancel_token);
        assert_eq!(prepared.map(|dirs| dirs.len()), Some(1));

        cancel_token.cancel();
        let prepared = prepare_release_dirs(&videos_by_dir, &inspections, &config, &cancel_token);
        assert!(prepared.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn planning_failure_after_stop_settles_every_item_as_cancelled() {
        let cancel_token = CancellationToken::new();
        let cancel_for_test = cancel_token.clone();
        let (event_sender, mut event_receiver) = create_event_channel(8);
        let (log_sender, mut log_receiver) = create_log_channel(1);
        log_sender.try_send("barrier".into()).unwrap();
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                input_dir: PathBuf::from("/anime"),
                ..Default::default()
            },
            Some(event_sender),
            cancel_token,
        )
        .with_log_sender(log_sender);
        let files = [parsed_file("one.mkv"), parsed_file("two.mkv")];
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let settlement = tokio::spawn(async move {
            let _ = started_sender.send(());
            pipeline
                .settle_planning_failure(&files, 0, "assignment disappeared".into(), Vec::new())
                .await
        });

        started_receiver.await.unwrap();
        assert_eq!(log_receiver.recv().await.as_deref(), Some("barrier"));
        cancel_for_test.cancel();
        assert!(log_receiver
            .recv()
            .await
            .unwrap()
            .contains("assignment disappeared"));

        let summary = settlement.await.unwrap();
        let mut events = Vec::new();
        while let Ok(event) = event_receiver.try_recv() {
            events.push(event);
        }

        assert!(summary.cancelled);
        assert_eq!(summary.errors, 0);
        assert_eq!(summary.total, 2);
        assert!(summary.log[0].contains("assignment disappeared"));
        assert_eq!(events.len(), 3);
        assert!(events[..2].iter().all(|event| matches!(
            event,
            JobEvent::ItemComplete {
                status: ItemOutcome::Cancelled,
                reason: Some(reason),
                error: None,
                ..
            } if reason == CANCELLED_REASON
        )));
        assert!(matches!(
            events[2],
            JobEvent::BatchComplete {
                success: false,
                cancelled: true,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panicking_worker_settles_its_item_as_error() {
        let (event_sender, mut event_receiver) = create_event_channel(8);
        let pipeline = BatchPipeline::new(
            PipelineConfig::default(),
            Some(event_sender),
            CancellationToken::new(),
        );
        let input_dir = Path::new("/anime");
        let items = vec![
            (BatchItemInfo::new(&parsed_file("one.mkv"), input_dir), true),
            (
                BatchItemInfo::new(&parsed_file("two.mkv"), input_dir),
                false,
            ),
        ];
        let mut summary = PipelineSummary::default();
        pipeline
            .run_items(
                &mut summary,
                items,
                |panics, _| async move {
                    assert!(!panics, "boom");
                    Ok(Vec::new())
                },
                "Remux",
            )
            .await;

        let mut outcomes = Vec::new();
        while let Ok(event) = event_receiver.try_recv() {
            if let JobEvent::ItemComplete {
                id,
                status,
                error,
                duration_sec,
                ..
            } = event
            {
                assert!(duration_sec.is_some());
                outcomes.push((
                    id.to_string(),
                    status,
                    error.is_some_and(|e| e.contains("boom")),
                ));
            }
        }
        outcomes.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            outcomes,
            [
                ("one.mkv".to_string(), ItemOutcome::Error, true),
                ("two.mkv".to_string(), ItemOutcome::Completed, false),
            ]
        );
        assert_eq!((summary.completed, summary.errors), (1, 1));
        assert!(summary.log[0].contains("worker panicked"));
    }

    #[test]
    fn batch_item_info_pairs_canonical_id_with_scanned_name() {
        let input_dir = Path::new("/anime");
        let file = create_parsed_file(input_dir.join("Season 1").join("【字幕】 Show - 01 ✓.mkv"));
        let item = BatchItemInfo::new(&file, input_dir);
        assert_eq!(
            item,
            BatchItemInfo {
                id: ItemId::parse("Season 1/【字幕】 Show - 01 ✓.mkv"),
                file: "【字幕】 Show - 01 ✓.mkv".into(),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn planning_failure_errors_pair_each_nested_id_with_its_file_name() {
        let input_dir = Path::new("/anime");
        let (event_sender, mut event_receiver) = create_event_channel(8);
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                input_dir: input_dir.to_path_buf(),
                ..Default::default()
            },
            Some(event_sender),
            CancellationToken::new(),
        );
        let files = ["Season 1/one.mkv", "Season 2/two.mkv"]
            .map(|rel| create_parsed_file(input_dir.join(rel)));
        pipeline
            .settle_planning_failure(&files, 0, "assignment disappeared".into(), Vec::new())
            .await;

        let mut outcomes = Vec::new();
        while let Ok(event) = event_receiver.try_recv() {
            if let JobEvent::ItemComplete {
                id, file, status, ..
            } = event
            {
                outcomes.push((id.to_string(), file, status));
            }
        }
        assert_eq!(
            outcomes,
            [
                (
                    "Season 1/one.mkv".to_string(),
                    "one.mkv".to_string(),
                    ItemOutcome::Error
                ),
                (
                    "Season 2/two.mkv".to_string(),
                    "two.mkv".to_string(),
                    ItemOutcome::Error
                ),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn planning_failure_sends_the_collected_lines_before_the_abort_line() {
        let (log_sender, mut log_receiver) = create_log_channel(8);
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                input_dir: PathBuf::from("/anime"),
                ..Default::default()
            },
            None,
            CancellationToken::new(),
        )
        .with_log_sender(log_sender);
        let files = [parsed_file("one.mkv"), parsed_file("two.mkv")];
        let collected = vec![
            "Skipping 'a.mkv': no episode".to_string(),
            "  ERROR: Failed to probe '/anime/b.mkv': bad header".to_string(),
            "  ERROR: '/anime/c.mkv': no prepared Release Directory".to_string(),
        ];
        let summary = pipeline
            .settle_planning_failure(
                &files,
                1,
                "assignment disappeared".into(),
                collected.clone(),
            )
            .await;

        let mut expected = collected;
        expected
            .push("  ERROR: Remux planning failed for 'two.mkv': assignment disappeared".into());
        let mut sent = Vec::new();
        while let Ok(line) = log_receiver.try_recv() {
            sent.push(line);
        }
        assert_eq!(sent, expected);
        assert_eq!(summary.log, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn item_events_pair_each_item_id_with_its_file_name() {
        let input_dir = Path::new("/anime");
        let items: Vec<(BatchItemInfo, usize)> = [
            "Season 1/Show - 01.mkv",
            "Season 1/Show - 02.mkv",
            "Season 2/Show - 03.mkv",
        ]
        .into_iter()
        .enumerate()
        .map(|(idx, rel)| {
            let file = create_parsed_file(input_dir.join(rel));
            (BatchItemInfo::new(&file, input_dir), idx)
        })
        .collect();
        // Nested IDs never equal their file names, so a swapped pair misses this map.
        let expected: HashMap<ItemId, String> = items
            .iter()
            .map(|(item, _)| (item.id.clone(), item.file.clone()))
            .collect();

        let (event_sender, mut event_receiver) = create_event_channel(64);
        let pipeline = BatchPipeline::new(
            PipelineConfig::default(),
            Some(event_sender),
            CancellationToken::new(),
        );
        let mut summary = PipelineSummary::default();
        pipeline
            .run_items(
                &mut summary,
                items,
                |idx, progress| async move {
                    progress.report(50.0);
                    match idx {
                        0 => Ok(Vec::new()),
                        1 => Err(ItemFailure::Failed("bad".into())),
                        _ => Err(ItemFailure::Cancelled),
                    }
                },
                "Remux",
            )
            .await;

        let mut seen = Vec::new();
        while let Ok(event) = event_receiver.try_recv() {
            let (kind, id, file) = match event {
                JobEvent::ItemStart { id, file, .. } => ("start", id, file),
                JobEvent::ItemProgress { id, file, .. } => ("progress", id, file),
                JobEvent::ItemComplete {
                    id, file, status, ..
                } => {
                    let kind = match status {
                        ItemOutcome::Completed => "completed",
                        ItemOutcome::Error => "error",
                        ItemOutcome::Cancelled => "cancelled",
                        other => panic!("unexpected outcome {other:?}"),
                    };
                    (kind, id, file)
                }
                other => panic!("unexpected event {other:?}"),
            };
            assert_eq!(expected.get(&id), Some(&file), "{kind} event for '{id}'");
            seen.push((id.to_string(), kind));
        }
        seen.sort();
        let seen: Vec<(&str, &str)> = seen.iter().map(|(id, k)| (id.as_str(), *k)).collect();
        assert_eq!(
            seen,
            [
                ("Season 1/Show - 01.mkv", "completed"),
                ("Season 1/Show - 01.mkv", "progress"),
                ("Season 1/Show - 01.mkv", "start"),
                ("Season 1/Show - 02.mkv", "error"),
                ("Season 1/Show - 02.mkv", "progress"),
                ("Season 1/Show - 02.mkv", "start"),
                ("Season 2/Show - 03.mkv", "cancelled"),
                ("Season 2/Show - 03.mkv", "progress"),
                ("Season 2/Show - 03.mkv", "start"),
            ]
        );
        assert!(summary.log[0].contains("failed for 'Show - 02.mkv'"));
    }

    #[test]
    fn cancellation_settles_unplanned_items_and_preserves_prior_outcomes() {
        let files = [
            parsed_file("one.mkv"),
            parsed_file("two.mkv"),
            parsed_file("three.mkv"),
        ];
        let resolved = JobEvent::ItemComplete {
            version: EVENT_VERSION,
            id: ItemId::parse("two.mkv"),
            file: "two.mkv".into(),
            status: ItemOutcome::Skipped,
            reason: Some("No subtitle match".into()),
            error: None,
            duration_sec: None,
            extra: Default::default(),
        };

        let events = settle_cancelled_planning_items(&files, Path::new("/anime"), vec![resolved]);
        let outcomes: Vec<(&str, ItemOutcome)> = events
            .iter()
            .filter_map(|event| match event {
                JobEvent::ItemComplete { id, status, .. } => Some((id.as_str(), *status)),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes,
            [
                ("one.mkv", ItemOutcome::Cancelled),
                ("two.mkv", ItemOutcome::Skipped),
                ("three.mkv", ItemOutcome::Cancelled)
            ]
        );
    }

    #[test]
    fn selection_orders_batch_and_ignores_repeated_selections() {
        let files = vec![
            parsed_file("one.mkv"),
            parsed_file("two.mkv"),
            parsed_file("three.mkv"),
        ];
        let selected = ["three.mkv", "./one.mkv", "three.mkv"].map(String::from);
        let ordered: Vec<String> =
            apply_selected_items(files, Path::new("/anime"), Some(&selected))
                .unwrap()
                .into_iter()
                .map(|file| file.name)
                .collect();
        assert_eq!(ordered, ["three.mkv", "one.mkv"]);
    }
}
