//! Batch pipeline execution, event streams, and cancellation coordination.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::domain::ItemOutcome;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
pub use tokio_util::sync::CancellationToken;

use crate::assets::{prepare_release_directory, ReleasePreparationOptions};
use crate::domain::output_stem;
use crate::extract::extract_mkv_file;
use crate::fonts::{scan_fonts, FontRegistry};
use crate::manifest::get_item_id;
use crate::remux::{execute_remux_job, select_tracks, PlannedJob};
use crate::scan::{
    get_or_probe_video, scan_mkv_files, InspectedVideo, ParsedFile, PersistentInspectCache,
};

pub const EVENT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    BatchStart {
        version: u64,
        total: usize,
        input_dir: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub workers: usize,
    pub io_jobs: usize,
    pub bitrate: String,
    pub complexity: u32,
    pub ffmpeg_threads: usize,
    pub sub_offset: f64,
    pub keep_fonts: bool,
    pub sub_assignments: Option<HashMap<String, PathBuf>>,
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
            workers: 4,
            io_jobs: 2,
            bitrate: "192k".to_string(),
            complexity: 10,
            ffmpeg_threads: 2,
            sub_offset: 0.0,
            keep_fonts: false,
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

pub struct BatchPipeline {
    pub config: PipelineConfig,
    pub event_sender: Option<EventSender>,
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
            cancel_token,
        }
    }

    async fn emit(&self, event: JobEvent) {
        if let Some(ref tx) = self.event_sender {
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

        let input_dir_str = self.config.input_dir.to_string_lossy().to_string();
        self.emit(JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: summary.total,
            input_dir: input_dir_str,
            extra: Default::default(),
        })
        .await;

        if files.is_empty() {
            summary.log.push(format!(
                "No MKV files found in '{}'",
                self.config.input_dir.display()
            ));
            self.emit(JobEvent::BatchComplete {
                version: EVENT_VERSION,
                success: true,
                cancelled: false,
                extra: Default::default(),
            })
            .await;
            return Ok(summary);
        }

        let out_dir = self.config.out_dir.clone();
        let fonts_dir = self.config.fonts_dir.clone().unwrap_or_else(|| {
            out_dir
                .as_ref()
                .unwrap_or(&self.config.input_dir)
                .join("fonts")
        });
        let font_registry = Arc::new(FontRegistry::new(fonts_dir, None));
        let cache = PersistentInspectCache::default();
        let cache_opt = if self.config.no_cache {
            None
        } else {
            Some(&cache)
        };

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
                        extra: Default::default(),
                    })
                    .await;
                }
                break;
            }

            let item_id = get_item_id(&file.path, &self.config.input_dir);

            if self.config.dry_run {
                summary.log.push(format!(
                    "[DRY-RUN] Would extract assets from: {}",
                    file.path.display()
                ));
                self.emit(JobEvent::ItemComplete {
                    version: EVENT_VERSION,
                    id: item_id,
                    file: file.name.clone(),
                    status: ItemOutcome::Planned,
                    reason: None,
                    error: None,
                    extra: Default::default(),
                })
                .await;
                summary.completed += 1;
                continue;
            }

            self.emit(JobEvent::ItemStart {
                version: EVENT_VERSION,
                id: item_id.clone(),
                file: file.name.clone(),
                extra: Default::default(),
            })
            .await;

            let meta = match get_or_probe_video(&file.path, cache_opt) {
                Ok(m) => m,
                Err(err) => {
                    summary.errors += 1;
                    summary.log.push(format!(
                        "  ERROR: Failed to probe '{}': {err}",
                        file.path.display()
                    ));
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: file.name.clone(),
                        status: ItemOutcome::Error,
                        reason: None,
                        error: Some(err),
                        extra: Default::default(),
                    })
                    .await;
                    continue;
                }
            };

            let inspected = InspectedVideo::new(meta);
            let extract_res = tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    summary.cancelled = true;
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: file.name.clone(),
                        status: ItemOutcome::Cancelled,
                        reason: Some("Operation cancelled".into()),
                        error: None,
                        extra: Default::default(),
                    })
                    .await;
                    for rem in &files[(idx + 1)..] {
                        let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: rem_id,
                            file: rem.name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                    break;
                }
                res = extract_mkv_file(&file.path, &inspected, &font_registry, out_dir.as_deref()) => res,
            };

            match extract_res {
                Ok(file_summary) => {
                    summary.completed += 1;
                    summary.log.extend(file_summary.log);
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: file.name.clone(),
                        status: ItemOutcome::Completed,
                        reason: None,
                        error: None,
                        extra: Default::default(),
                    })
                    .await;
                }
                Err(err) => {
                    summary.errors += 1;
                    summary.log.push(format!(
                        "  ERROR: Extraction failed for '{}': {err}",
                        file.path.display()
                    ));
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: file.name.clone(),
                        status: ItemOutcome::Error,
                        reason: None,
                        error: Some(err),
                        extra: Default::default(),
                    })
                    .await;
                }
            }
        }
        if let Some(c) = cache_opt {
            c.save_shared();
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

        let input_dir_str = self.config.input_dir.to_string_lossy().to_string();
        self.emit(JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: summary.total,
            input_dir: input_dir_str,
            extra: Default::default(),
        })
        .await;

        if files.is_empty() {
            summary.log.push(format!(
                "No MKV files found in '{}'",
                self.config.input_dir.display()
            ));
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

        let cache = PersistentInspectCache::default();
        let cache_opt = if self.config.no_cache {
            None
        } else {
            Some(&cache)
        };

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
                            extra: Default::default(),
                        })
                        .await;
                    }
                }
                break;
            }
            let prep_opts = ReleasePreparationOptions {
                fonts_override: self.config.fonts_dir.as_deref(),
                assets_dir: self.config.assets_dir.as_deref(),
                ..Default::default()
            };
            let prepared =
                prepare_release_directory(mkv_dir, &self.config.input_dir, videos, prep_opts);
            let font_info = scan_fonts(&prepared.fonts_dir, None);

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
                let out_dir_base = self
                    .config
                    .out_dir
                    .clone()
                    .unwrap_or_else(|| mkv_dir.join("Output"));
                let output_file = out_dir_base.join(format!("{out_stem}.mkv"));

                let meta = match get_or_probe_video(&video_file.path, cache_opt) {
                    Ok(m) => m,
                    Err(err) => {
                        summary.errors += 1;
                        summary.log.push(format!(
                            "  ERROR: Failed to probe '{}': {err}",
                            video_file.path.display()
                        ));
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: video_file.name.clone(),
                            status: ItemOutcome::Error,
                            reason: None,
                            error: Some(err),
                            extra: Default::default(),
                        })
                        .await;
                        continue;
                    }
                };

                let inspected = InspectedVideo::new(meta);
                let assigned_sub = self
                    .config
                    .sub_assignments
                    .as_ref()
                    .and_then(|m| m.get(&item_id).map(|p| p.as_path()));

                let res_assets = prepared.resolver.resolve_for(
                    video_file,
                    Some(&inspected),
                    None,
                    None,
                    assigned_sub,
                );

                if !res_assets.matched_subtitle {
                    summary.skipped += 1;
                    let reason = res_assets
                        .skip_reason
                        .unwrap_or_else(|| "No matching subtitle found".into());
                    summary
                        .log
                        .push(format!("Skipping '{}': {reason}", video_file.name));
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: video_file.name.clone(),
                        status: ItemOutcome::Skipped,
                        reason: Some(reason),
                        error: None,
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
                    summary.log.push(format!(
                        "[DRY-RUN] Would remux '{}' -> '{}'",
                        video_file.name,
                        job.output_file.display()
                    ));
                    self.emit(JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id,
                        file: video_file.name.clone(),
                        status: ItemOutcome::Planned,
                        reason: None,
                        error: None,
                        extra: Default::default(),
                    })
                    .await;
                    summary.completed += 1;
                    continue;
                }

                self.emit(JobEvent::ItemStart {
                    version: EVENT_VERSION,
                    id: item_id.clone(),
                    file: video_file.name.clone(),
                    extra: Default::default(),
                })
                .await;

                let remux_res = tokio::select! {
                    biased;
                    _ = self.cancel_token.cancelled() => {
                        summary.cancelled = true;
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: video_file.name.clone(),
                            status: ItemOutcome::Cancelled,
                            reason: Some("Operation cancelled".into()),
                            error: None,
                            extra: Default::default(),
                        })
                        .await;
                        for rem in &videos[(v_idx + 1)..] {
                            let rem_id = get_item_id(&rem.path, &self.config.input_dir);
                            self.emit(JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: rem_id,
                                file: rem.name.clone(),
                                status: ItemOutcome::Cancelled,
                                reason: Some("Operation cancelled".into()),
                                error: None,
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
                                    extra: Default::default(),
                                })
                                .await;
                            }
                        }
                        break 'dir_loop;
                    }
                    res = execute_remux_job(
                        &job,
                        &selection,
                        &font_info,
                        &self.config.bitrate,
                        self.config.complexity,
                        self.config.ffmpeg_threads,
                        self.config.sub_offset,
                    ) => res,
                };

                match remux_res {
                    Ok(file_summary) => {
                        summary.completed += 1;
                        summary.log.extend(file_summary.log);
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: video_file.name.clone(),
                            status: ItemOutcome::Completed,
                            reason: None,
                            error: None,
                            extra: Default::default(),
                        })
                        .await;
                    }
                    Err(err) => {
                        summary.errors += 1;
                        summary.log.push(format!(
                            "  ERROR: Remux failed for '{}': {err}",
                            video_file.name
                        ));
                        self.emit(JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id,
                            file: video_file.name.clone(),
                            status: ItemOutcome::Error,
                            reason: None,
                            error: Some(err),
                            extra: Default::default(),
                        })
                        .await;
                    }
                }
            }
        }

        if let Some(c) = cache_opt {
            c.save_shared();
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
