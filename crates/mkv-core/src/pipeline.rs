//! Batch pipeline execution, event streams, and cancellation coordination.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::domain::ItemOutcome;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
pub use tokio_util::sync::CancellationToken;

use crate::assets::{
    claim_leftover_chapters, prepare_release_directory, ReleasePreparationOptions,
};
use crate::domain::{output_stem, resolve_output_dir};
use crate::extract::extract_mkv_file;
use crate::fonts::{scan_fonts, FontInfo, FontRegistry};
use crate::manifest::get_item_id;
use crate::remux::{execute_remux_job, select_tracks, PlannedJob, RemuxOptions, TrackSelection};
use crate::scan::{
    get_or_probe_video, scan_mkv_files, InspectedVideo, InspectionResult, ParsedFile,
    PersistentInspectCache, ScanResults,
};

pub const EVENT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchItemInfo {
    pub id: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    BatchStart {
        version: u64,
        total: usize,
        input_dir: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<Vec<BatchItemInfo>>,
        #[serde(default, flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemStart {
        version: u64,
        id: String,
        file: String,
        #[serde(default, flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemProgress {
        version: u64,
        id: String,
        file: String,
        progress: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eta: Option<String>,
        #[serde(default, flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ItemComplete {
        version: u64,
        id: String,
        file: String,
        status: ItemOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_sec: Option<f64>,
        #[serde(default, flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    },
    BatchComplete {
        version: u64,
        success: bool,
        #[serde(default)]
        cancelled: bool,
        #[serde(default, flatten)]
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
    pub cache_file: Option<PathBuf>,
    pub workers: usize,
    pub io_jobs: usize,
    pub bitrate: String,
    pub complexity: u32,
    pub ffmpeg_threads: usize,
    pub sub_offset: f64,
    pub keep_fonts: bool,
    pub sub_assignments: Option<HashMap<String, PathBuf>>,
    /// Retained for forward-compatibility with multi-track remuxing (dialogue + signs/songs).
    pub track_assignments: Option<Vec<crate::request::TrackAssignment>>,
}

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
            cache_file: None,
            workers: 4,
            io_jobs: 2,
            bitrate: "192k".to_string(),
            complexity: 10,
            ffmpeg_threads: 2,
            sub_offset: 0.0,
            keep_fonts: false,
            track_assignments: None,
            sub_assignments: None,
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

pub struct BatchPipeline {
    pub config: PipelineConfig,
    pub event_sender: Option<EventSender>,
    pub log_sender: Option<LogSender>,
    pub cancel_token: CancellationToken,
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
            cancel_token,
        }
    }

    pub fn from_request(
        request: crate::request::BatchRequest,
        event_sender: Option<EventSender>,
        cancel_token: CancellationToken,
    ) -> Result<Self, String> {
        let config = request.into_pipeline_config()?;
        Ok(Self::new(config, event_sender, cancel_token))
    }

    pub fn with_log_sender(mut self, log_sender: Option<LogSender>) -> Self {
        self.log_sender = log_sender;
        self
    }

    async fn log(&self, msg: impl Into<String>) {
        let s = msg.into();
        if let Some(tx) = &self.log_sender {
            let _ = tx.send(s).await;
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

    pub async fn run(&self) -> Result<PipelineSummary, String> {
        match self.config.mode {
            PipelineMode::Extract => self.run_extract().await,
            PipelineMode::Remux => self.run_remux().await,
        }
    }

    async fn run_extract(&self) -> Result<PipelineSummary, String> {
        let mut summary = PipelineSummary::default();
        let files = scan_mkv_files(&self.config.input_dir, true, None);
        summary.total = files.len();

        let items: Vec<BatchItemInfo> = files
            .iter()
            .map(|f| BatchItemInfo {
                id: get_item_id(&f.path, &self.config.input_dir),
                file: f.name.clone(),
            })
            .collect();
        let input_dir_str = self.config.input_dir.to_string_lossy().to_string();
        self.emit(JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: summary.total,
            input_dir: input_dir_str,
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
            self.emit(JobEvent::BatchComplete {
                version: EVENT_VERSION,
                success: true,
                cancelled: false,
                extra: Default::default(),
            })
            .await;
            return Ok(summary);
        }

        let cache = self.config.cache.clone().unwrap_or_else(|| {
            Arc::new(PersistentInspectCache::new(
                self.config.cache_file.clone(),
                !self.config.no_cache,
            ))
        });
        let cache_opt = if self.config.no_cache {
            None
        } else {
            Some(&*cache)
        };

        if self.config.dry_run {
            for (idx, file) in files.iter().enumerate() {
                if self.cancel_token.is_cancelled() {
                    summary.cancelled = true;
                    for rem in &files[idx..] {
                        let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: rem_id,
                            file: rem.name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            duration_sec: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                    break;
                }

                let item_id = get_item_id(&file.path, &self.config.input_dir);
                let msg = format!(
                    "[DRY-RUN] Would extract assets from: {}",
                    file.path.display()
                );
                self.log(&msg).await;
                summary.log.push(msg);
                self.emit(JobEvent::ItemComplete {
                    version: EVENT_VERSION,
                    id: item_id,
                    file: file.name.clone(),
                    status: ItemOutcome::Planned,
                    reason: None,
                    error: None,
                    duration_sec: None,
                    extra: Default::default(),
                })
                .await;
                summary.completed += 1;
            }

            if let Some(c) = cache_opt {
                c.save();
            }

            let success = summary.errors == 0 && !summary.cancelled;
            self.emit(JobEvent::BatchComplete {
                version: EVENT_VERSION,
                success,
                cancelled: summary.cancelled,
                extra: Default::default(),
            })
            .await;
            return Ok(summary);
        }

        let max_workers = self.config.workers.max(1);
        let io_limit = if self.config.io_jobs > 0 {
            self.config.io_jobs
        } else {
            max_workers
        };
        let io_semaphore = Arc::new(tokio::sync::Semaphore::new(io_limit));
        let mut join_set = tokio::task::JoinSet::new();
        let mut in_flight: HashMap<String, (String, std::time::Instant)> = HashMap::new();
        let mut font_registries: HashMap<PathBuf, Arc<FontRegistry>> = HashMap::new();
        let mut file_idx = 0;

        while file_idx < files.len() || !join_set.is_empty() {
            if self.cancel_token.is_cancelled() {
                summary.cancelled = true;
                break;
            }

            while file_idx < files.len() && join_set.len() < max_workers {
                if self.cancel_token.is_cancelled() {
                    summary.cancelled = true;
                    break;
                }

                let file = &files[file_idx];
                file_idx += 1;

                let item_id = get_item_id(&file.path, &self.config.input_dir);
                let file_path = file.path.clone();
                let file_name = file.name.clone();
                let mkv_dir = file_path.parent().unwrap_or(Path::new("."));
                let fonts_dir = fonts_dir_for(
                    mkv_dir,
                    &self.config.input_dir,
                    self.config.out_dir.as_deref(),
                    self.config.fonts_dir.as_deref(),
                );
                let font_registry = font_registries
                    .entry(fonts_dir.clone())
                    .or_insert_with(|| Arc::new(FontRegistry::new(fonts_dir, None)))
                    .clone();
                let target_out_dir =
                    self.config.out_dir.as_deref().map(|out| {
                        resolve_output_dir(&file_path, &self.config.input_dir, Some(out))
                    });
                let cancel_token = self.cancel_token.clone();
                let event_sender = self.event_sender.clone();
                let io_sem = io_semaphore.clone();
                let task_cache = cache.clone();
                let no_cache = self.config.no_cache;

                let start_instant = std::time::Instant::now();
                in_flight.insert(item_id.clone(), (file_name.clone(), start_instant));
                self.emit(JobEvent::ItemStart {
                    version: EVENT_VERSION,
                    id: item_id.clone(),
                    file: file_name.clone(),
                    extra: Default::default(),
                })
                .await;

                join_set.spawn(async move {
                    if cancel_token.is_cancelled() {
                        return (
                            item_id,
                            file_name,
                            Err("Operation cancelled".into()),
                            true,
                            start_instant.elapsed().as_secs_f64(),
                        );
                    }

                    let cache_opt = if no_cache { None } else { Some(&*task_cache) };
                    let meta = match get_or_probe_video(&file_path, cache_opt) {
                        Ok(m) => m,
                        Err(err) => {
                            return (item_id, file_name, Err(err), false, start_instant.elapsed().as_secs_f64());
                        }
                    };
                    let inspected = InspectedVideo::new(meta);

                    let progress_tx = event_sender.clone();
                    let progress_item_id = item_id.clone();
                    let progress_file_name = file_name.clone();
                    let on_progress = move |pct: f64| {
                        if let Some(tx) = &progress_tx {
                            let _ = tx.try_send(JobEvent::ItemProgress {
                                version: EVENT_VERSION,
                                id: progress_item_id.clone(),
                                file: progress_file_name.clone(),
                                progress: pct,
                                speed: None,
                                eta: None,
                                extra: Default::default(),
                            });
                        }
                    };

                    let _io_permit = tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => {
                            return (item_id, file_name, Err("Operation cancelled".into()), true, start_instant.elapsed().as_secs_f64());
                        }
                        permit = io_sem.acquire() => permit.ok(),
                    };

                    let extract_future = extract_mkv_file(
                        &file_path,
                        &inspected,
                        &font_registry,
                        target_out_dir.as_deref(),
                        Some(&on_progress),
                    );

                    tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => {
                            let duration = start_instant.elapsed().as_secs_f64();
                            (item_id, file_name, Err("Operation cancelled".into()), true, duration)
                        }
                        res = extract_future => {
                            let duration = start_instant.elapsed().as_secs_f64();
                            match res {
                                Ok(file_summary) => (item_id, file_name, Ok(file_summary.log), false, duration),
                                Err(err) => (item_id, file_name, Err(err), false, duration),
                            }
                        }
                    }
                });
            }

            if let Some(res) = join_set.join_next().await {
                match res {
                    Ok((item_id, file_name, task_res, is_cancelled, duration)) => {
                        in_flight.remove(&item_id);
                        if is_cancelled {
                            summary.cancelled = true;
                            self.emit(JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: item_id,
                                file: file_name,
                                status: ItemOutcome::Cancelled,
                                reason: Some("Operation cancelled".into()),
                                error: None,
                                duration_sec: Some(duration),
                                extra: Default::default(),
                            })
                            .await;
                        } else {
                            match task_res {
                                Ok(logs) => {
                                    summary.completed += 1;
                                    self.log_batch(&logs).await;
                                    summary.log.extend(logs);
                                    self.emit(JobEvent::ItemComplete {
                                        version: EVENT_VERSION,
                                        id: item_id,
                                        file: file_name,
                                        status: ItemOutcome::Completed,
                                        reason: None,
                                        error: None,
                                        duration_sec: Some(duration),
                                        extra: Default::default(),
                                    })
                                    .await;
                                }
                                Err(err) => {
                                    summary.errors += 1;
                                    let msg = format!(
                                        "  ERROR: Extraction failed for '{file_name}': {err}"
                                    );
                                    self.log(&msg).await;
                                    summary.log.push(msg);
                                    self.emit(JobEvent::ItemComplete {
                                        version: EVENT_VERSION,
                                        id: item_id,
                                        file: file_name,
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
                    }
                    Err(join_err) => {
                        if !join_err.is_cancelled() {
                            summary.errors += 1;
                            let msg = format!("  ERROR: Task panic: {join_err}");
                            self.log(&msg).await;
                            summary.log.push(msg);
                        }
                    }
                }
            }
        }

        if summary.cancelled {
            join_set.abort_all();
            while join_set.join_next().await.is_some() {}
            for (item_id, (file_name, start_instant)) in in_flight.drain() {
                let duration = start_instant.elapsed().as_secs_f64();
                self.emit(JobEvent::ItemComplete {
                    version: EVENT_VERSION,
                    id: item_id,
                    file: file_name,
                    status: ItemOutcome::Cancelled,
                    reason: Some("Operation cancelled".into()),
                    error: None,
                    duration_sec: Some(duration),
                    extra: Default::default(),
                })
                .await;
            }
            for rem in &files[file_idx..] {
                let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                self.emit(JobEvent::ItemComplete {
                    version: EVENT_VERSION,
                    id: rem_id,
                    file: rem.name.clone(),
                    status: ItemOutcome::Cancelled,
                    reason: Some("Operation cancelled".into()),
                    error: None,
                    duration_sec: None,
                    extra: Default::default(),
                })
                .await;
            }
        }

        for registry in font_registries.values() {
            registry.cleanup_staging();
        }

        if !self.config.no_cache {
            cache.save();
        }

        let success = summary.errors == 0 && !summary.cancelled;
        self.emit(JobEvent::BatchComplete {
            version: EVENT_VERSION,
            success,
            cancelled: summary.cancelled,
            extra: Default::default(),
        })
        .await;

        Ok(summary)
    }

    async fn run_remux(&self) -> Result<PipelineSummary, String> {
        let mut summary = PipelineSummary::default();
        let files = scan_mkv_files(&self.config.input_dir, true, None);
        summary.total = files.len();

        let items: Vec<BatchItemInfo> = files
            .iter()
            .map(|f| BatchItemInfo {
                id: get_item_id(&f.path, &self.config.input_dir),
                file: f.name.clone(),
            })
            .collect();
        let input_dir_str = self.config.input_dir.to_string_lossy().to_string();
        self.emit(JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: summary.total,
            input_dir: input_dir_str,
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
            self.emit(JobEvent::BatchComplete {
                version: EVENT_VERSION,
                success: true,
                cancelled: false,
                extra: Default::default(),
            })
            .await;
            return Ok(summary);
        }

        let mut videos_by_dir: BTreeMap<PathBuf, Vec<ParsedFile>> = BTreeMap::new();
        for file in files {
            let parent = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();
            videos_by_dir.entry(parent).or_default().push(file);
        }

        let cache = self.config.cache.clone().unwrap_or_else(|| {
            Arc::new(PersistentInspectCache::new(
                self.config.cache_file.clone(),
                !self.config.no_cache,
            ))
        });
        let cache_opt = if self.config.no_cache {
            None
        } else {
            Some(&*cache)
        };

        let mut scan_cache: HashMap<PathBuf, ScanResults> = HashMap::new();
        let mut fonts_dir_cache: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut font_cache: HashMap<PathBuf, FontInfo> = HashMap::new();

        let max_workers = self.config.workers.max(1);
        let io_limit = if self.config.io_jobs > 0 {
            self.config.io_jobs
        } else {
            max_workers
        };
        let io_semaphore = Arc::new(tokio::sync::Semaphore::new(io_limit));

        let dir_list: Vec<(PathBuf, Vec<ParsedFile>)> = videos_by_dir.into_iter().collect();
        'dir_loop: for (d_idx, (mkv_dir, videos)) in dir_list.iter().enumerate() {
            if self.cancel_token.is_cancelled() {
                summary.cancelled = true;
                for (_, remaining_videos) in &dir_list[d_idx..] {
                    for rem in remaining_videos {
                        let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: rem_id,
                            file: rem.name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            duration_sec: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                }
                break;
            }

            let resolved_assets_dir = self.config.assets_dir.as_deref().map(|assets| {
                let rel_buf: Option<PathBuf> = mkv_dir
                    .strip_prefix(&self.config.input_dir)
                    .map(|p| p.to_path_buf())
                    .ok()
                    .or_else(|| {
                        if let (Ok(m_canon), Ok(r_canon)) =
                            (mkv_dir.canonicalize(), self.config.input_dir.canonicalize())
                        {
                            m_canon.strip_prefix(&r_canon).map(|p| p.to_path_buf()).ok()
                        } else {
                            None
                        }
                    });
                match rel_buf {
                    Some(rel) => {
                        let candidate = assets.join(rel);
                        if candidate.is_dir() {
                            candidate
                        } else {
                            assets.to_path_buf()
                        }
                    }
                    None => assets.to_path_buf(),
                }
            });
            let prep_opts = ReleasePreparationOptions {
                fonts_override: self.config.fonts_dir.as_deref(),
                assets_dir: resolved_assets_dir.as_deref(),
                scan_cache: Some(&mut scan_cache),
                fonts_dir_cache: Some(&mut fonts_dir_cache),
                font_cache: Some(&mut font_cache),
            };
            let prepared =
                prepare_release_directory(mkv_dir, &self.config.input_dir, videos, prep_opts);
            let font_info = scan_fonts(&prepared.fonts_dir, Some(&mut font_cache));

            let mut dir_inspections: HashMap<PathBuf, InspectionResult> = HashMap::new();
            for v in videos {
                match get_or_probe_video(&v.path, cache_opt) {
                    Ok(meta) => {
                        dir_inspections.insert(v.path.clone(), InspectionResult::success(meta));
                    }
                    Err(e) => {
                        dir_inspections.insert(v.path.clone(), InspectionResult::failure(e));
                    }
                }
            }
            let chapter_claims =
                claim_leftover_chapters(videos, &dir_inspections, &prepared.resolver);

            let mut planned_jobs: Vec<(PlannedJob, TrackSelection)> = Vec::new();

            for (v_idx, video_file) in videos.iter().enumerate() {
                if self.cancel_token.is_cancelled() {
                    summary.cancelled = true;
                    for rem in &videos[v_idx..] {
                        let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: rem_id,
                            file: rem.name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            duration_sec: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                    for (_, remaining_videos) in &dir_list[(d_idx + 1)..] {
                        for rem in remaining_videos {
                            let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                            self.emit(JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: rem_id,
                                file: rem.name.clone(),
                                status: ItemOutcome::Cancelled,
                                reason: Some("Operation cancelled".into()),
                                error: None,
                                duration_sec: None,
                                extra: Default::default(),
                            })
                            .await;
                        }
                    }
                    break 'dir_loop;
                }

                let item_id = get_item_id(&video_file.path, &self.config.input_dir);

                let Some(parsed) = &video_file.parsed else {
                    summary.skipped += 1;
                    let reason = "Could not extract episode number from filename";
                    summary
                        .log
                        .push(format!("Skipping '{}': {reason}", video_file.name));
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: video_file.name.clone(),
                        status: ItemOutcome::Skipped,
                        reason: Some(reason.into()),
                        error: None,
                        duration_sec: None,
                        extra: Default::default(),
                    })
                    .await;
                    continue;
                };

                let base = video_file
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "video".to_string());
                let out_stem = output_stem(&base, &parsed.ep_type, &parsed.ep);
                let out_dir = resolve_output_dir(
                    &video_file.path,
                    &self.config.input_dir,
                    self.config.out_dir.as_deref(),
                );
                let output_file = if self.config.out_dir.is_none() {
                    out_dir.join("Output").join(format!("{out_stem}.mkv"))
                } else {
                    out_dir.join(format!("{out_stem}.mkv"))
                };

                let inspected = match dir_inspections
                    .get(&video_file.path)
                    .and_then(|r| r.video.as_ref())
                {
                    Some(v) => v.clone(),
                    None => {
                        summary.errors += 1;
                        let err = dir_inspections
                            .get(&video_file.path)
                            .and_then(|r| r.error.clone())
                            .unwrap_or_else(|| "Failed to probe video".to_string());
                        let msg = format!(
                            "  ERROR: Failed to probe '{}': {err}",
                            video_file.path.display()
                        );
                        self.log(&msg).await;
                        summary.log.push(msg);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: video_file.name.clone(),
                            status: ItemOutcome::Error,
                            reason: None,
                            error: Some(err),
                            duration_sec: None,
                            extra: Default::default(),
                        })
                        .await;
                        continue;
                    }
                };

                let assigned_sub = self
                    .config
                    .sub_assignments
                    .as_ref()
                    .and_then(|m| m.get(&item_id).map(|p| p.as_path()));

                let claimed_chapter = chapter_claims.get(&video_file.path).map(|p| p.as_path());
                let res_assets = prepared.resolver.resolve_for(
                    video_file,
                    Some(&inspected),
                    None,
                    claimed_chapter,
                    assigned_sub,
                );

                if !res_assets.matched_subtitle {
                    summary.skipped += 1;
                    let reason = res_assets
                        .skip_reason
                        .unwrap_or_else(|| "No matching subtitle found".into());
                    let log_msg = format!("Skipping '{}': {reason}", video_file.name);
                    self.log(&log_msg).await;
                    summary.log.push(log_msg);
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: video_file.name.clone(),
                        status: ItemOutcome::Skipped,
                        reason: Some(reason),
                        error: None,
                        duration_sec: None,
                        extra: Default::default(),
                    })
                    .await;
                    continue;
                }

                let job = PlannedJob {
                    video: video_file.path.clone(),
                    video_name: video_file.name.clone(),
                    base,
                    output_file,
                    subtitle_file: res_assets.subtitle_file,
                    subtitle_label: res_assets.subtitle_label,
                    subtitle_mkv_lang: res_assets.subtitle_mkv_lang,
                    chapter_file: res_assets.chapter_file,
                    cover_file: res_assets.cover_file,
                    embedded_sub_id: res_assets.embedded_sub_id,
                    use_source_fonts: font_info.count == 0,
                    release_dir: Some(mkv_dir.clone()),
                    item_id: item_id.clone(),
                };

                let selection = select_tracks(&inspected);

                if self.config.dry_run {
                    let msg = format!(
                        "[DRY-RUN] Would remux '{}' -> '{}'",
                        video_file.name,
                        job.output_file.display()
                    );
                    self.log(&msg).await;
                    summary.log.push(msg);
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: video_file.name.clone(),
                        status: ItemOutcome::Planned,
                        reason: None,
                        error: None,
                        duration_sec: None,
                        extra: Default::default(),
                    })
                    .await;
                    summary.completed += 1;
                    continue;
                }

                planned_jobs.push((job, selection));
            }

            if !planned_jobs.is_empty() {
                let mut join_set = tokio::task::JoinSet::new();
                let mut in_flight: HashMap<String, (String, std::time::Instant)> = HashMap::new();
                let mut job_idx = 0;

                while job_idx < planned_jobs.len() || !join_set.is_empty() {
                    if self.cancel_token.is_cancelled() {
                        summary.cancelled = true;
                        break;
                    }

                    while job_idx < planned_jobs.len() && join_set.len() < max_workers {
                        if self.cancel_token.is_cancelled() {
                            summary.cancelled = true;
                            break;
                        }

                        let (job, selection) = planned_jobs[job_idx].clone();
                        job_idx += 1;

                        let start_instant = std::time::Instant::now();
                        in_flight
                            .insert(job.item_id.clone(), (job.video_name.clone(), start_instant));
                        self.emit(JobEvent::ItemStart {
                            version: EVENT_VERSION,
                            id: job.item_id.clone(),
                            file: job.video_name.clone(),
                            extra: Default::default(),
                        })
                        .await;

                        let cancel_token = self.cancel_token.clone();
                        let event_sender = self.event_sender.clone();
                        let io_sem = io_semaphore.clone();
                        let font_info_clone = font_info.clone();
                        let bitrate = self.config.bitrate.clone();
                        let complexity = self.config.complexity;
                        let ffmpeg_threads = self.config.ffmpeg_threads;
                        let sub_offset = self.config.sub_offset;

                        join_set.spawn(async move {
                            if cancel_token.is_cancelled() {
                                return (
                                    job.item_id,
                                    job.video_name,
                                    Err("Operation cancelled".into()),
                                    true,
                                    start_instant.elapsed().as_secs_f64(),
                                );
                            }

                            let progress_tx = event_sender.clone();
                            let progress_item_id = job.item_id.clone();
                            let progress_file_name = job.video_name.clone();
                            let on_progress = move |pct: f64| {
                                if let Some(tx) = &progress_tx {
                                    let _ = tx.try_send(JobEvent::ItemProgress {
                                        version: EVENT_VERSION,
                                        id: progress_item_id.clone(),
                                        file: progress_file_name.clone(),
                                        progress: pct,
                                        speed: None,
                                        eta: None,
                                        extra: Default::default(),
                                    });
                                }
                            };

                            let remux_opts = RemuxOptions {
                                opus_bitrate: &bitrate,
                                complexity,
                                ffmpeg_threads,
                                sub_offset,
                            };
                            let remux_future = execute_remux_job(
                                &job,
                                &selection,
                                &font_info_clone,
                                &remux_opts,
                                Some(&on_progress),
                                Some(&io_sem),
                            );

                            tokio::select! {
                                biased;
                                _ = cancel_token.cancelled() => {
                                    let duration = start_instant.elapsed().as_secs_f64();
                                    (job.item_id, job.video_name, Err("Operation cancelled".into()), true, duration)
                                }
                                res = remux_future => {
                                    let duration = start_instant.elapsed().as_secs_f64();
                                    match res {
                                        Ok(res_summary) => (job.item_id, job.video_name, Ok(res_summary.log), false, duration),
                                        Err(err) => (job.item_id, job.video_name, Err(err), false, duration),
                                    }
                                }
                            }
                        });
                    }

                    if let Some(res) = join_set.join_next().await {
                        match res {
                            Ok((item_id, file_name, task_res, is_cancelled, duration)) => {
                                in_flight.remove(&item_id);
                                if is_cancelled {
                                    summary.cancelled = true;
                                    self.emit(JobEvent::ItemComplete {
                                        version: EVENT_VERSION,
                                        id: item_id,
                                        file: file_name,
                                        status: ItemOutcome::Cancelled,
                                        reason: Some("Operation cancelled".into()),
                                        error: None,
                                        duration_sec: Some(duration),
                                        extra: Default::default(),
                                    })
                                    .await;
                                } else {
                                    match task_res {
                                        Ok(logs) => {
                                            summary.completed += 1;
                                            self.log_batch(&logs).await;
                                            summary.log.extend(logs);
                                            self.emit(JobEvent::ItemComplete {
                                                version: EVENT_VERSION,
                                                id: item_id,
                                                file: file_name,
                                                status: ItemOutcome::Completed,
                                                reason: None,
                                                error: None,
                                                duration_sec: Some(duration),
                                                extra: Default::default(),
                                            })
                                            .await;
                                        }
                                        Err(err) => {
                                            summary.errors += 1;
                                            let msg = format!(
                                                "  ERROR: Remux failed for '{file_name}': {err}"
                                            );
                                            self.log(&msg).await;
                                            summary.log.push(msg);
                                            self.emit(JobEvent::ItemComplete {
                                                version: EVENT_VERSION,
                                                id: item_id,
                                                file: file_name,
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
                            }
                            Err(join_err) => {
                                if !join_err.is_cancelled() {
                                    summary.errors += 1;
                                    let msg = format!("  ERROR: Task panic: {join_err}");
                                    self.log(&msg).await;
                                    summary.log.push(msg);
                                }
                            }
                        }
                    }
                }

                if summary.cancelled {
                    join_set.abort_all();
                    while join_set.join_next().await.is_some() {}
                    for (item_id, (file_name, start_instant)) in in_flight.drain() {
                        let duration = start_instant.elapsed().as_secs_f64();
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: file_name,
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            duration_sec: Some(duration),
                            extra: Default::default(),
                        })
                        .await;
                    }
                    for (rem_job, _) in &planned_jobs[job_idx..] {
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: rem_job.item_id.clone(),
                            file: rem_job.video_name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            duration_sec: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                    for (_, remaining_videos) in &dir_list[(d_idx + 1)..] {
                        for rem in remaining_videos {
                            let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                            self.emit(JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: rem_id,
                                file: rem.name.clone(),
                                status: ItemOutcome::Cancelled,
                                reason: Some("Operation cancelled".into()),
                                error: None,
                                duration_sec: None,
                                extra: Default::default(),
                            })
                            .await;
                        }
                    }
                    break 'dir_loop;
                }
            }
        }

        if !self.config.no_cache {
            cache.save();
        }

        let success = summary.errors == 0 && !summary.cancelled;
        self.emit(JobEvent::BatchComplete {
            version: EVENT_VERSION,
            success,
            cancelled: summary.cancelled,
            extra: Default::default(),
        })
        .await;

        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
