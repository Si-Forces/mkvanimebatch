//! Strongly-typed batch execution requests and track assignments.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::ops::RangeInclusive;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cpu::{default_workers_for, physical_core_count, transcode_governor_threads};
use crate::item_id::ItemId;
use crate::pipeline::{PipelineConfig, PipelineMode};
use crate::preset::{lookup_preset, PresetProfile};

/// Accepted explicit `workers` values.
pub const WORKERS_RANGE: RangeInclusive<usize> = 1..=64;
/// Accepted explicit `io_jobs` values; `0` resolves to the worker count.
pub const IO_JOBS_RANGE: RangeInclusive<usize> = 0..=64;
/// Accepted explicit `ffmpeg_threads` values.
pub const FFMPEG_THREADS_RANGE: RangeInclusive<usize> = 1..=32;
/// Accepted explicit Opus `complexity` values.
pub const COMPLEXITY_RANGE: RangeInclusive<u32> = 0..=10;
/// The `io_jobs` value a request that leaves it unset resolves to.
pub const DEFAULT_IO_JOBS: usize = 2;
/// The Preset Profile whose bitrate and complexity apply when a request sets neither a preset
/// nor those values.
pub const DEFAULT_PRESET_ID: &str = "balanced";

/// The kind of track a [`TrackAssignment`] attaches. Only subtitles are supported; the enum keeps
/// the `track_type` wire field typed, so any value other than `"subtitle"` fails deserialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignmentKind {
    #[default]
    Subtitle,
}

/// Explicit manual track assignment attaching an asset to a target video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackAssignment {
    /// Target video file path, relative to `input_dir` (e.g. "Season 1/Ep01.mkv")
    pub target_video: String,
    /// Absolute or relative path to the subtitle file
    pub asset_path: PathBuf,
    /// Track kind; defaults to subtitle when absent.
    #[serde(default)]
    pub track_type: AssignmentKind,
    /// Optional ISO-639-2 language tag (e.g. "ind", "eng", "jpn")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional track title string
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional millisecond time shift
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
}

/// Returns true if a path string begins with a Windows drive prefix (e.g. `C:` or `D:\`).
fn is_drive_prefix(s: &str) -> bool {
    s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic()
}

/// True when a forward-slash-normalized path stays inside the directory it is joined to:
/// no root, no drive prefix, no `..` component.
fn is_contained_relative_path(norm: &str) -> bool {
    let path = Path::new(norm);
    !path.has_root()
        && !is_drive_prefix(norm)
        && !path.components().any(|c| matches!(c, Component::ParentDir))
}

fn has_mkv_extension(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("mkv"))
}

/// A positive integer with an optional `k`/`K` or `M` suffix, as ffmpeg's `-b:a` reads it.
/// Lowercase `m` is rejected: ffmpeg treats it as milli, so `1m` would silently encode at the
/// encoder's floor instead of failing.
pub fn is_valid_bitrate(raw: &str) -> bool {
    let trimmed = raw.trim();
    let digits = trimmed.strip_suffix(['k', 'K', 'M']).unwrap_or(trimmed);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits.parse::<u64>().is_ok_and(|n| n > 0)
}

/// Words a range for a validation error, e.g. `between 1 and 64`.
fn between<T: Display>(range: &RangeInclusive<T>) -> String {
    format!("between {} and {}", range.start(), range.end())
}

/// Maps each selection to the index its [`ItemId`] has in `index_by_id`, in selection order
/// with repeats dropped. Fails naming every selection that matches no discovered item.
pub(crate) fn resolve_selected_items(
    index_by_id: &HashMap<ItemId, usize>,
    selected: &[String],
) -> Result<Vec<usize>, String> {
    let mut seen = HashSet::with_capacity(selected.len());
    let mut ordered = Vec::with_capacity(selected.len());
    let mut unmatched = Vec::new();
    for item in selected {
        match index_by_id.get(&ItemId::parse(item)) {
            Some(&idx) => {
                if seen.insert(idx) {
                    ordered.push(idx);
                }
            }
            None => unmatched.push(item.as_str()),
        }
    }
    if unmatched.is_empty() {
        Ok(ordered)
    } else {
        Err(format!(
            "Selected items not found among discovered MKV files: {}",
            unmatched.join(", ")
        ))
    }
}

impl TrackAssignment {
    /// The [`ItemId`] of the video this assignment targets.
    pub(crate) fn target_id(&self) -> ItemId {
        ItemId::parse(&self.target_video)
    }

    /// True if the asset path is relative to the release/input directory
    /// (not absolute, not root-relative, and not drive-prefixed).
    fn is_relative_asset(&self) -> bool {
        let s = self.asset_path.to_string_lossy().replace('\\', "/");
        !is_drive_prefix(&s) && !Path::new(&s).has_root()
    }

    /// Resolves a relative asset path against `input_dir`, then `assets_dir`, falling back
    /// to the `input_dir` candidate when neither exists; absolute paths are returned as-is.
    pub fn resolve_asset_path(&self, input_dir: &Path, assets_dir: Option<&Path>) -> PathBuf {
        if !self.is_relative_asset() {
            return self.asset_path.clone();
        }
        let clean_asset = PathBuf::from(self.asset_path.to_string_lossy().replace('\\', "/"));
        let from_input = input_dir.join(&clean_asset);
        if from_input.is_file() {
            return from_input;
        }
        assets_dir
            .map(|assets| assets.join(&clean_asset))
            .filter(|from_assets| from_assets.is_file())
            .unwrap_or(from_input)
    }
}

/// Canonical serializable specification to configure and initiate batch execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Pipeline execution mode (Extract or Remux)
    pub mode: PipelineMode,
    /// Root directory containing MKV media files
    pub input_dir: PathBuf,
    /// Optional output directory for remuxed / extracted outputs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_dir: Option<PathBuf>,
    /// Optional directory containing external fonts
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts_dir: Option<PathBuf>,
    /// Optional fallback directory for external assets
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets_dir: Option<PathBuf>,
    /// Preset profile identifier (e.g. "balanced", "fast", "hifi", "custom")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Audio transcode bitrate override (e.g. "192k")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<String>,
    /// Opus encoder complexity override (0-10)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<u32>,
    /// Concurrency worker count override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<usize>,
    /// Concurrency I/O jobs override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_jobs: Option<usize>,
    /// Per-job FFmpeg `-threads` override. It precedes `-i`, so it caps the FLAC decoder's threads;
    /// libopus is single-threaded (ADR-0013, 2026-10-05 correction, #36).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg_threads: Option<usize>,
    /// Global subtitle sync offset in whole milliseconds. `None` and `Some(0)` both mean no
    /// global shift; a Track Assignment's `offset_ms` replaces it (ADR-0012).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_sub_offset_ms: Option<i64>,
    /// Whether to perform a dry run (plan without writing files)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    /// Whether to bypass and ignore persistent inspect cache
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_cache: Option<bool>,
    /// Explicit track assignments
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<Vec<TrackAssignment>>,
    /// Optional ordered subset of relative video paths or item IDs designating active execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_items: Option<Vec<String>>,
}

impl Default for BatchRequest {
    fn default() -> Self {
        Self {
            mode: PipelineMode::Remux,
            input_dir: PathBuf::new(),
            out_dir: None,
            fonts_dir: None,
            assets_dir: None,
            preset: None,
            bitrate: None,
            complexity: None,
            workers: None,
            io_jobs: None,
            ffmpeg_threads: None,
            default_sub_offset_ms: None,
            dry_run: None,
            no_cache: None,
            assignments: None,
            selected_items: None,
        }
    }
}

/// The Effective Settings of a Batch Pipeline: the concurrency and audio transcoding values it
/// runs with, after explicit request values are layered over the Preset Profile and the
/// hardware-derived defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveSettings {
    pub workers: usize,
    /// The effective limit: `BatchRequest::resolve` expands an explicit `0` to `workers`, so a
    /// resolved config never holds `0`.
    pub io_jobs: usize,
    pub ffmpeg_threads: usize,
    pub bitrate: String,
    pub complexity: u32,
}

impl EffectiveSettings {
    /// The Effective Settings of a request that leaves every setting unset, on this machine.
    pub fn defaults() -> Self {
        Self::defaults_with_cores(physical_core_count())
    }

    /// [`Self::defaults`] for `cores` physical cores, through the same layering as a resolved
    /// request.
    fn defaults_with_cores(cores: usize) -> Self {
        let request = BatchRequest::default();
        let preset = request
            .preset_profile()
            .expect("the default preset is built in");
        request.layer_effective_settings(preset, cores)
    }
}

impl BatchRequest {
    /// Validates the request, then converts it into the `PipelineConfig` a `BatchPipeline` runs:
    /// explicit values layered over the Preset Profile and the hardware-derived defaults, the
    /// Effective Settings. The first failure wins.
    ///
    /// This is the only production path from a request to a config. It is synchronous so a
    /// caller can reject a bad request before it reserves a job slot or starts any task.
    pub fn resolve(self) -> Result<PipelineConfig, String> {
        self.resolve_with_cores(physical_core_count())
    }

    /// [`Self::resolve`] on a machine with `cores` physical cores, so tests can fix the
    /// hardware-derived defaults.
    fn resolve_with_cores(self, cores: usize) -> Result<PipelineConfig, String> {
        let preset = self.validate()?;
        let EffectiveSettings {
            workers,
            io_jobs,
            ffmpeg_threads,
            bitrate,
            complexity,
        } = self.layer_effective_settings(preset, cores);

        Ok(PipelineConfig {
            mode: self.mode,
            input_dir: self.input_dir,
            out_dir: self.out_dir,
            fonts_dir: self.fonts_dir,
            assets_dir: self.assets_dir,
            bitrate,
            complexity,
            workers,
            io_jobs,
            ffmpeg_threads,
            default_sub_offset_ms: self.default_sub_offset_ms.unwrap_or_default(),
            dry_run: self.dry_run.unwrap_or_default(),
            no_cache: self.no_cache.unwrap_or_default(),
            cache: None,
            font_hash_memo: None,
            track_assignments: self.assignments,
            selected_items: self.selected_items,
        })
    }

    /// The Preset Profile the request names, else the `DEFAULT_PRESET_ID` one.
    fn preset_profile(&self) -> Result<PresetProfile, String> {
        let id = self.preset.as_deref().unwrap_or(DEFAULT_PRESET_ID);
        lookup_preset(id).ok_or_else(|| format!("Unknown preset profile: '{id}'"))
    }

    /// Layers the explicit settings over `preset` and the defaults for a machine with `cores`
    /// physical cores. With `preset_profile`, which picks the default preset, the one home of the
    /// default rules; it does not check ranges, which `validate` does.
    fn layer_effective_settings(&self, preset: PresetProfile, cores: usize) -> EffectiveSettings {
        let workers = self.workers.unwrap_or_else(|| default_workers_for(cores));
        let io_jobs = match self.io_jobs {
            None => DEFAULT_IO_JOBS,
            Some(0) => workers,
            Some(io_jobs) => io_jobs,
        };
        EffectiveSettings {
            workers,
            io_jobs,
            ffmpeg_threads: self
                .ffmpeg_threads
                .unwrap_or_else(|| transcode_governor_threads(cores, workers)),
            bitrate: self
                .bitrate
                .as_deref()
                .map_or(preset.bitrate, |b| b.trim().to_owned()),
            complexity: self.complexity.unwrap_or(preset.complexity),
        }
    }

    /// Checks the directories, numeric ranges, bitrate format, preset, track assignments and
    /// selected items, in that order, and returns the Preset Profile it checked.
    fn validate(&self) -> Result<PresetProfile, String> {
        if !self.input_dir.is_dir() {
            return Err(format!(
                "Input directory does not exist or is not a directory: '{}'",
                self.input_dir.display()
            ));
        }

        if let Some(d) = &self.out_dir {
            if d.is_file() {
                return Err(format!(
                    "Output directory path points to an existing file: '{}'",
                    d.display()
                ));
            }
        }

        if let Some(d) = &self.fonts_dir {
            if !d.is_dir() {
                return Err(format!(
                    "Fonts directory does not exist or is not a directory: '{}'",
                    d.display()
                ));
            }
        }

        if let Some(d) = &self.assets_dir {
            if !d.is_dir() {
                return Err(format!(
                    "Assets directory does not exist or is not a directory: '{}'",
                    d.display()
                ));
            }
        }

        if let Some(w) = self.workers {
            if !WORKERS_RANGE.contains(&w) {
                return Err(format!(
                    "Worker thread count must be {}",
                    between(&WORKERS_RANGE)
                ));
            }
        }

        if let Some(io) = self.io_jobs {
            if !IO_JOBS_RANGE.contains(&io) {
                return Err(format!("I/O job count must be {}", between(&IO_JOBS_RANGE)));
            }
        }

        if let Some(t) = self.ffmpeg_threads {
            if !FFMPEG_THREADS_RANGE.contains(&t) {
                return Err(format!(
                    "FFmpeg thread count must be {}",
                    between(&FFMPEG_THREADS_RANGE)
                ));
            }
        }

        if let Some(b) = self.bitrate.as_deref().filter(|b| !is_valid_bitrate(b)) {
            return Err(format!(
                "Invalid bitrate format: '{b}'. Must be a positive integer with an optional k/K/M suffix, like 128k or 192k."
            ));
        }

        if let Some(c) = self.complexity {
            if !COMPLEXITY_RANGE.contains(&c) {
                return Err(format!(
                    "Encoder complexity must be {}, got {c}",
                    between(&COMPLEXITY_RANGE)
                ));
            }
        }

        let preset = self.preset_profile()?;

        if let Some(assignments) = &self.assignments {
            let mut seen_videos: HashSet<ItemId> = HashSet::new();

            for assignment in assignments {
                let target_id = assignment.target_id();
                // The containment and drive-prefix checks must see the raw spelling, since
                // `ItemId::parse` reshapes a leading root or drive; the two forms are deliberate.
                let target_norm = assignment.target_video.trim().replace('\\', "/");
                if target_id.as_str().is_empty() || !is_contained_relative_path(&target_norm) {
                    return Err(format!(
                        "Invalid relative target video path: '{}'",
                        assignment.target_video
                    ));
                }

                if !has_mkv_extension(target_id.as_str()) {
                    return Err(format!(
                        "Assigned target video is not an MKV file: '{target_id}'"
                    ));
                }

                let raw_asset = assignment.asset_path.to_string_lossy();
                let root_relative_backslash =
                    raw_asset.starts_with('\\') && !raw_asset.starts_with("\\\\");
                let escapes_root = assignment.is_relative_asset()
                    && Path::new(&raw_asset.replace('\\', "/"))
                        .components()
                        .any(|c| matches!(c, Component::ParentDir));
                if root_relative_backslash || escapes_root {
                    return Err(format!(
                        "Invalid relative asset path: '{}'",
                        assignment.asset_path.display()
                    ));
                }

                let target_full = self.input_dir.join(target_id.as_str());
                if !seen_videos.insert(target_id.clone()) {
                    return Err(format!(
                        "Multiple track assignments for target video '{target_id}' are not supported; multi-track muxing is deferred"
                    ));
                }

                if !target_full.is_file() {
                    return Err(format!(
                        "Assigned target video file does not exist: '{}'",
                        target_full.display()
                    ));
                }

                // Fail-fast preflight per ADR-0008: the assigned asset must exist on disk.
                let asset =
                    assignment.resolve_asset_path(&self.input_dir, self.assets_dir.as_deref());
                if !asset.is_file() {
                    return Err(format!(
                        "Assigned asset file does not exist: '{}'",
                        asset.display()
                    ));
                }
            }
        }

        if let Some(items) = &self.selected_items {
            if items.is_empty() {
                return Err("selected_items cannot be empty when specified".into());
            }

            let mut seen_items: HashSet<ItemId> = HashSet::new();
            for item in items {
                // Raw spelling for the containment check, as for `target_norm` above.
                let norm = item.trim().replace('\\', "/");
                if norm.is_empty() || !is_contained_relative_path(&norm) {
                    return Err(format!("Invalid relative path in selected_items: '{item}'"));
                }

                let item_id = ItemId::parse(item);
                if !seen_items.insert(item_id.clone()) {
                    return Err(format!("Duplicate entry in selected_items: '{item_id}'"));
                }
                if !has_mkv_extension(item_id.as_str()) {
                    return Err(format!("Selected item is not an MKV file: '{item_id}'"));
                }
                let full_path = self.input_dir.join(item_id.as_str());
                if !full_path.is_file() {
                    return Err(format!(
                        "Selected item does not exist: '{}'",
                        full_path.display()
                    ));
                }
            }

            if let Some(assignments) = &self.assignments {
                for assignment in assignments {
                    let target_id = assignment.target_id();
                    if !seen_items.contains(&target_id) {
                        return Err(format!(
                            "Track assignment targets video '{target_id}' which is not in selected_items"
                        ));
                    }
                }
            }
        }

        Ok(preset)
    }
}

/// Parses track assignments from JSON: a `Vec<TrackAssignment>` array, an object whose
/// `assignments` key holds such an array or a map, or a legacy flat
/// `{ "video.mkv": "sub.ass" }` map of subtitle assignments.
pub fn parse_assignments_json(json_str: &str) -> Result<Vec<TrackAssignment>, String> {
    let trimmed = json_str.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Invalid assignments JSON: {e}"))?;

    match value {
        Value::Array(_) => assignment_array(value),
        Value::Object(obj) => match obj.get("assignments") {
            Some(Value::Object(inner)) => legacy_assignment_map(inner),
            Some(inner @ Value::Array(_)) => assignment_array(inner.clone()),
            _ => legacy_assignment_map(&obj),
        },
        _ => Err("Unrecognized assignments JSON schema".into()),
    }
}

/// Deserializes a `Vec<TrackAssignment>` array, reporting serde's reason on failure.
fn assignment_array(value: Value) -> Result<Vec<TrackAssignment>, String> {
    serde_json::from_value(value).map_err(|e| format!("Invalid track assignment: {e}"))
}

/// Parses a flat legacy map (`{ "video.mkv": "sub.ass" }`) into subtitle assignments.
fn legacy_assignment_map(
    map: &serde_json::Map<String, Value>,
) -> Result<Vec<TrackAssignment>, String> {
    map.iter()
        .map(|(target_video, asset)| match asset {
            Value::String(asset_path) => Ok(TrackAssignment {
                target_video: target_video.clone(),
                asset_path: PathBuf::from(asset_path),
                track_type: AssignmentKind::Subtitle,
                language: None,
                title: None,
                offset_ms: None,
            }),
            other => Err(format!(
                "Expected string asset path for '{target_video}', got {other}"
            )),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn minimal_request(dir: &Path) -> BatchRequest {
        BatchRequest {
            input_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    fn subtitle(target_video: &str, asset_path: impl Into<PathBuf>) -> TrackAssignment {
        TrackAssignment {
            target_video: target_video.into(),
            asset_path: asset_path.into(),
            track_type: AssignmentKind::Subtitle,
            language: None,
            title: None,
            offset_ms: None,
        }
    }

    /// Creates an empty file at `rel` under `dir`, including parent directories.
    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        path
    }

    /// Physical cores of the machine `resolved` pretends to run on.
    const CORES: usize = 4;

    /// Resolves a copy of `req` as on a machine with `CORES` physical cores.
    fn resolved(req: &BatchRequest) -> Result<PipelineConfig, String> {
        req.clone().resolve_with_cores(CORES)
    }

    fn resolve_with_assignments(
        dir: &Path,
        assignments: Vec<TrackAssignment>,
    ) -> Result<(), String> {
        let mut req = minimal_request(dir);
        req.assignments = Some(assignments);
        resolved(&req).map(drop)
    }

    #[test]
    fn test_batch_request_validation_success() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        let sub_path = touch(dir.path(), "ep01.ass");

        let mut req = minimal_request(dir.path());
        req.preset = Some("balanced".into());
        req.bitrate = Some("256k".into());
        req.complexity = Some(8);
        req.workers = Some(4);
        req.io_jobs = Some(2);
        req.assignments = Some(vec![TrackAssignment {
            language: Some("ind".into()),
            ..subtitle("ep01.mkv", sub_path)
        }]);
        let config = resolved(&req).unwrap();
        assert_eq!(config.bitrate, "256k");
        assert_eq!(config.complexity, 8);
        assert_eq!(config.workers, 4);
        assert_eq!(config.io_jobs, 2);
        assert_eq!(config.track_assignments.map(|a| a.len()), Some(1));
    }

    #[test]
    fn test_resolve_layers_overrides_over_preset() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.preset = Some("hifi".into());
        let config = resolved(&req).unwrap();
        assert!(!config.dry_run);
        assert_eq!(config.bitrate, "320k");
        assert_eq!(config.complexity, 10);

        req.bitrate = Some("256k".into());
        req.dry_run = Some(true);
        let config = resolved(&req).unwrap();
        assert_eq!(config.bitrate, "256k");
        assert_eq!(config.complexity, 10);
        assert!(config.dry_run);

        req.complexity = Some(3);
        assert_eq!(resolved(&req).unwrap().complexity, 3);
    }

    #[test]
    fn test_resolve_forwards_trimmed_bitrate() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.bitrate = Some(" 320K ".into());
        assert_eq!(resolved(&req).unwrap().bitrate, "320K");
    }

    #[test]
    fn test_resolve_defaults_follow_the_core_count() {
        let dir = tempdir().unwrap();
        // (cores, default workers, default ffmpeg_threads)
        for (cores, workers, ffmpeg_threads) in [(1, 1, 1), (4, 4, 1), (32, 16, 2)] {
            let config = minimal_request(dir.path())
                .resolve_with_cores(cores)
                .unwrap();
            assert_eq!(config.workers, workers, "cores {cores}");
            assert_eq!(config.ffmpeg_threads, ffmpeg_threads, "cores {cores}");
            assert_eq!(config.io_jobs, DEFAULT_IO_JOBS, "cores {cores}");

            // The governor follows an explicit worker count, not the default one.
            let req = BatchRequest {
                workers: Some(2),
                ..minimal_request(dir.path())
            };
            let config = req.resolve_with_cores(cores).unwrap();
            assert_eq!(config.workers, 2, "cores {cores}");
            assert_eq!(
                config.ffmpeg_threads,
                (cores / 2).clamp(1, 4),
                "cores {cores}"
            );

            let req = BatchRequest {
                ffmpeg_threads: Some(7),
                ..minimal_request(dir.path())
            };
            assert_eq!(req.resolve_with_cores(cores).unwrap().ffmpeg_threads, 7);
        }
    }

    #[test]
    fn test_resolve_io_jobs_is_the_effective_limit() {
        let dir = tempdir().unwrap();
        for cores in [1, 4, 32] {
            let io_jobs = |io_jobs: Option<usize>, workers: Option<usize>| {
                let req = BatchRequest {
                    io_jobs,
                    workers,
                    ..minimal_request(dir.path())
                };
                req.resolve_with_cores(cores).unwrap().io_jobs
            };
            let default_workers = default_workers_for(cores);
            assert_eq!(io_jobs(None, None), 2, "cores {cores}");
            assert_eq!(io_jobs(Some(0), None), default_workers, "cores {cores}");
            assert_eq!(io_jobs(Some(0), Some(6)), 6, "cores {cores}");
            assert_eq!(io_jobs(Some(3), None), 3, "cores {cores}");
            assert_eq!(io_jobs(Some(64), Some(6)), 64, "cores {cores}");
        }
    }

    #[test]
    fn test_resolve_defaults_equal_the_balanced_preset() {
        let dir = tempdir().unwrap();
        let balanced = lookup_preset(DEFAULT_PRESET_ID).unwrap();
        let config = resolved(&minimal_request(dir.path())).unwrap();
        assert_eq!(config.bitrate, balanced.bitrate);
        assert_eq!(config.complexity, balanced.complexity);

        let mut req = minimal_request(dir.path());
        req.preset = Some(DEFAULT_PRESET_ID.into());
        assert_eq!(
            resolved(&req).unwrap().effective_settings(),
            config.effective_settings()
        );
    }

    #[test]
    fn test_pipeline_config_fixture_matches_the_balanced_preset() {
        let balanced = lookup_preset(DEFAULT_PRESET_ID).unwrap();
        let fixture = PipelineConfig::default();
        assert_eq!(fixture.bitrate, balanced.bitrate);
        assert_eq!(fixture.complexity, balanced.complexity);
    }

    #[test]
    fn test_effective_settings_defaults_match_a_resolved_minimal_request() {
        let dir = tempdir().unwrap();
        for cores in [1, 4, 32] {
            let config = minimal_request(dir.path())
                .resolve_with_cores(cores)
                .unwrap();
            assert_eq!(
                EffectiveSettings::defaults_with_cores(cores),
                config.effective_settings(),
                "cores {cores}"
            );
        }
        // On this host, through the public entry points.
        assert_eq!(
            EffectiveSettings::defaults(),
            minimal_request(dir.path())
                .resolve()
                .unwrap()
                .effective_settings()
        );
    }

    #[test]
    fn test_effective_settings_serde_shape() {
        let settings = EffectiveSettings {
            workers: 4,
            io_jobs: 2,
            ffmpeg_threads: 1,
            bitrate: "192k".into(),
            complexity: 5,
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "workers": 4,
                "io_jobs": 2,
                "ffmpeg_threads": 1,
                "bitrate": "192k",
                "complexity": 5,
            })
        );
        assert_eq!(
            serde_json::from_value::<EffectiveSettings>(json).unwrap(),
            settings
        );
    }

    #[test]
    fn test_resolve_default_workers_hardware_concurrency() {
        let dir = tempdir().unwrap();
        let config = minimal_request(dir.path()).resolve().unwrap();
        // The preflight probe sizes itself from `default_workers()`, so an unset `workers` must
        // resolve to the same value.
        assert_eq!(config.workers, crate::cpu::default_workers());
        // Derived from the host's physical core count, so check the ranges, not the values.
        assert!(
            (1..=16).contains(&config.workers),
            "workers out of range: {}",
            config.workers
        );
        assert_eq!(config.complexity, 5);
        assert!(
            (1..=4).contains(&config.ffmpeg_threads),
            "ffmpeg_threads out of range: {}",
            config.ffmpeg_threads
        );
    }

    #[test]
    fn test_batch_request_validation_workers() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for bad in [0, 65] {
            req.workers = Some(bad);
            assert_eq!(
                resolved(&req).unwrap_err(),
                "Worker thread count must be between 1 and 64"
            );
        }
        for ok in [1, 64] {
            req.workers = Some(ok);
            assert!(resolved(&req).is_ok());
        }
    }

    #[test]
    fn test_batch_request_validation_io_jobs() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        // 0 is accepted: it resolves to the worker count.
        for ok in [0, 64] {
            req.io_jobs = Some(ok);
            assert!(resolved(&req).is_ok());
        }
        req.io_jobs = Some(65);
        assert_eq!(
            resolved(&req).unwrap_err(),
            "I/O job count must be between 0 and 64"
        );
    }

    #[test]
    fn test_batch_request_validation_ffmpeg_threads() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for bad in [0, 33] {
            req.ffmpeg_threads = Some(bad);
            assert_eq!(
                resolved(&req).unwrap_err(),
                "FFmpeg thread count must be between 1 and 32"
            );
        }
        for ok in [1, 32] {
            req.ffmpeg_threads = Some(ok);
            assert!(resolved(&req).is_ok());
        }
    }

    #[test]
    fn test_batch_request_validation_complexity() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for ok in [0, 10] {
            req.complexity = Some(ok);
            assert!(resolved(&req).is_ok());
        }
        req.complexity = Some(11);
        assert_eq!(
            resolved(&req).unwrap_err(),
            "Encoder complexity must be between 0 and 10, got 11"
        );
    }

    /// The wire type is the only check: whole milliseconds deserialize, a fraction does
    /// not, and the request resolves `None` to the same no-shift `0` as an explicit zero.
    #[test]
    fn test_default_sub_offset_ms_is_whole_milliseconds() {
        let parse = |offset: &str| {
            serde_json::from_str::<BatchRequest>(&format!(
                r#"{{"mode":"remux","input_dir":"/a"{offset}}}"#
            ))
        };
        for (offset, expected) in [
            (r#","default_sub_offset_ms":1500"#, Some(1500)),
            (r#","default_sub_offset_ms":-500"#, Some(-500)),
            (r#","default_sub_offset_ms":0"#, Some(0)),
            (r#","default_sub_offset_ms":null"#, None),
            ("", None),
            // No alias: the pre-rename key is an unknown field, so it is ignored.
            (r#","sub_offset":1.5"#, None),
        ] {
            let parsed = parse(offset).unwrap();
            assert_eq!(parsed.default_sub_offset_ms, expected, "{offset}");
        }
        assert!(
            parse(r#","default_sub_offset_ms":1.5"#).is_err(),
            "a fractional millisecond must not deserialize"
        );

        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for (offset, expected) in [(None, 0), (Some(0), 0), (Some(-1500), -1500)] {
            req.default_sub_offset_ms = offset;
            let config = resolved(&req).unwrap();
            assert_eq!(config.default_sub_offset_ms, expected, "{offset:?}");
        }
    }

    /// The concurrency fields are `usize` on the wire, so a fraction or a negative number fails
    /// to deserialize before `resolve` can check its range. The desktop relies on that: it has
    /// no range check of its own (ADR-0008 Amendment 2026-10-06).
    #[test]
    fn test_concurrency_fields_are_whole_non_negative_numbers() {
        let parse = |field: &str, value: &str| {
            serde_json::from_str::<BatchRequest>(&format!(
                r#"{{"mode":"remux","input_dir":"/a","{field}":{value}}}"#
            ))
        };
        for field in ["workers", "io_jobs", "ffmpeg_threads"] {
            for bad in ["1.5", "64.9", "-1"] {
                assert!(
                    parse(field, bad).is_err(),
                    "{field}: {bad} must not deserialize"
                );
            }
            // A whole number deserializes even when its range is wrong: `resolve` rejects that.
            assert!(parse(field, "100").is_ok(), "{field}: 100");
        }
    }

    #[test]
    fn test_batch_request_validation_bitrate() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for ok in ["192k", " 320K ", "1M", "96000"] {
            req.bitrate = Some(ok.into());
            assert!(resolved(&req).is_ok(), "bitrate {ok:?} should be valid");
        }
        for bad in ["", "k", "0k", "12.5k", "-192k", "192kb", "1m"] {
            req.bitrate = Some(bad.into());
            assert_eq!(
                resolved(&req).unwrap_err(),
                format!(
                    "Invalid bitrate format: '{bad}'. Must be a positive integer with an optional k/K/M suffix, like 128k or 192k."
                )
            );
        }
    }

    #[test]
    fn test_batch_request_validation_path_traversal() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        let sub_path = touch(dir.path(), "ep01.ass");

        let cases = [
            (
                subtitle("../outside.mkv", &sub_path),
                "Invalid relative target video path: '../outside.mkv'",
            ),
            (
                subtitle("/absolute/path.mkv", &sub_path),
                "Invalid relative target video path: '/absolute/path.mkv'",
            ),
            (
                subtitle(r"..\outside.mkv", &sub_path),
                r"Invalid relative target video path: '..\outside.mkv'",
            ),
            (
                subtitle("ep01.mkv", "../outside.ass"),
                "Invalid relative asset path: '../outside.ass'",
            ),
            (
                subtitle("ep01.mkv", r"sub\..\..\outside.ass"),
                r"Invalid relative asset path: 'sub\..\..\outside.ass'",
            ),
            // A root-relative backslash asset must not bypass root containment.
            (
                subtitle("ep01.mkv", r"\nonexistent\outside.ass"),
                r"Invalid relative asset path: '\nonexistent\outside.ass'",
            ),
        ];
        for (assignment, expected) in cases {
            assert_eq!(
                resolve_with_assignments(dir.path(), vec![assignment]).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn test_batch_request_validation_root_escape() {
        let dir = tempdir().unwrap();
        let sub_path = touch(dir.path(), "ep01.ass");

        for bad_target in ["/ep01.mkv", r"\ep01.mkv", "", "./", r".\", "././"] {
            let err = resolve_with_assignments(dir.path(), vec![subtitle(bad_target, &sub_path)])
                .unwrap_err();
            assert!(
                err.contains("Invalid relative target video path"),
                "expected error for '{bad_target}', got '{err}'"
            );
        }
    }

    #[test]
    fn test_validate_assignment_rejects_existing_non_mkv_target() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "notes.txt");
        touch(dir.path(), "ep1.ass");

        assert_eq!(
            resolve_with_assignments(dir.path(), vec![subtitle("notes.txt", "ep1.ass")])
                .unwrap_err(),
            "Assigned target video is not an MKV file: 'notes.txt'"
        );

        touch(dir.path(), "EP2.MKV");
        assert!(resolve_with_assignments(dir.path(), vec![subtitle("EP2.MKV", "ep1.ass")]).is_ok());
    }

    #[test]
    fn test_batch_request_fail_fast_missing_target_and_asset() {
        let dir = tempdir().unwrap();
        let sub_path = touch(dir.path(), "ep01.ass");
        let err = resolve_with_assignments(dir.path(), vec![subtitle("ghost.mkv", &sub_path)])
            .unwrap_err();
        assert!(
            err.contains("Assigned target video file does not exist"),
            "{err}"
        );

        touch(dir.path(), "ep01.mkv");
        let missing = dir.path().join("missing_sub.ass");
        let err =
            resolve_with_assignments(dir.path(), vec![subtitle("ep01.mkv", missing)]).unwrap_err();
        assert!(err.contains("Assigned asset file does not exist"), "{err}");
    }

    #[test]
    fn test_batch_request_asset_in_assets_dir() {
        let dir = tempdir().unwrap();
        let assets_dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        touch(assets_dir.path(), "ep01.ass");

        let mut req = minimal_request(dir.path());
        req.assets_dir = Some(assets_dir.path().to_path_buf());
        req.assignments = Some(vec![subtitle("ep01.mkv", "ep01.ass")]);
        assert!(resolved(&req).is_ok());
    }

    #[test]
    fn test_batch_request_duplicate_assignment_after_normalization() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        touch(dir.path(), "ep02.mkv");
        touch(dir.path(), "Season 1/ep01.mkv");
        let sub_path = touch(dir.path(), "ep01.ass");

        // Leading `./` and `.\` are accepted.
        assert!(resolve_with_assignments(
            dir.path(),
            vec![
                subtitle("./ep01.mkv", &sub_path),
                subtitle(r".\ep02.mkv", &sub_path),
            ],
        )
        .is_ok());

        for (first, second, expected) in [
            ("ep01.mkv", "ep01.mkv", "ep01.mkv"),
            ("./ep01.mkv", "ep01.mkv", "ep01.mkv"),
            ("./ep01.mkv", r".\ep01.mkv", "ep01.mkv"),
            (
                "Season 1/ep01.mkv",
                "Season 1/./ep01.mkv",
                "Season 1/ep01.mkv",
            ),
        ] {
            let err = resolve_with_assignments(
                dir.path(),
                vec![subtitle(first, &sub_path), subtitle(second, &sub_path)],
            )
            .unwrap_err();
            assert_eq!(
                err,
                format!(
                    "Multiple track assignments for target video '{expected}' are not supported; multi-track muxing is deferred"
                )
            );
        }
    }

    #[test]
    fn test_track_assignment_asset_path_resolution() {
        let dir = tempdir().unwrap();
        let assets_dir = tempdir().unwrap();
        touch(dir.path(), "ep01.ass");
        touch(assets_dir.path(), "subs/ep08.ass");

        for relative in ["ep01.ass", r"subs\ep08.ass", "missing.ass"] {
            assert!(
                subtitle("v.mkv", relative).is_relative_asset(),
                "{relative}"
            );
        }
        for absolute in [
            "/abs/ep03.ass",
            "C:/ep04.ass",
            r"D:\subs\ep05.ass",
            r"\abs\ep06.ass",
            r"\\server\share\ep07.ass",
        ] {
            let assignment = subtitle("v.mkv", absolute);
            assert!(!assignment.is_relative_asset(), "{absolute}");
            assert_eq!(
                assignment.resolve_asset_path(dir.path(), Some(assets_dir.path())),
                PathBuf::from(absolute)
            );
        }

        let resolve = |asset: &str| {
            subtitle("v.mkv", asset).resolve_asset_path(dir.path(), Some(assets_dir.path()))
        };
        assert_eq!(resolve("ep01.ass"), dir.path().join("ep01.ass"));
        assert_eq!(
            resolve(r"subs\ep08.ass"),
            assets_dir.path().join("subs/ep08.ass")
        );
        // Neither location has it: report the input_dir candidate.
        assert_eq!(resolve("missing.ass"), dir.path().join("missing.ass"));
    }

    #[test]
    fn test_parse_assignments_json_formats() {
        assert!(parse_assignments_json("  ").unwrap().is_empty());

        let array =
            r#"[{"target_video": "ep01.mkv", "asset_path": "/tmp/ep01.ass", "offset_ms": 250}]"#;
        let parsed = parse_assignments_json(array).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target_video, "ep01.mkv");
        assert_eq!(parsed[0].offset_ms, Some(250));
        assert_eq!(parsed[0].track_type, AssignmentKind::Subtitle);

        let wrapped_array = format!(r#"{{"assignments": {array}}}"#);
        assert_eq!(parse_assignments_json(&wrapped_array).unwrap(), parsed);

        let legacy = vec![subtitle("ep02.mkv", "/tmp/ep02.ass")];
        assert_eq!(
            parse_assignments_json(r#"{"ep02.mkv": "/tmp/ep02.ass"}"#).unwrap(),
            legacy
        );
        assert_eq!(
            parse_assignments_json(r#"{"assignments": {"ep02.mkv": "/tmp/ep02.ass"}}"#).unwrap(),
            legacy
        );
        assert_eq!(
            parse_assignments_json(r#"{"assignments": "x.ass"}"#).unwrap(),
            vec![subtitle("assignments", "x.ass")]
        );

        assert_eq!(
            parse_assignments_json("42").unwrap_err(),
            "Unrecognized assignments JSON schema"
        );
        assert_eq!(
            parse_assignments_json(r#"{"assignments": 1}"#).unwrap_err(),
            "Expected string asset path for 'assignments', got 1"
        );
        // A `TrackAssignment` array, bare or wrapped, surfaces its serde error, with no legacy-map fallback.
        for json in [
            r#"[{"asset_path": "x.ass"}]"#,
            r#"{"assignments": [{"asset_path": "x.ass"}]}"#,
        ] {
            let err = parse_assignments_json(json).unwrap_err();
            assert!(
                err.starts_with("Invalid track assignment: missing field `target_video`"),
                "{json}: {err}"
            );
        }
        // Only lowercase "subtitle" is a valid track_type.
        for (track_type, reason) in [
            (r#""audio""#, "unknown variant `audio`"),
            (r#""Subtitle""#, "unknown variant `Subtitle`"),
            (r#"" subtitle ""#, "unknown variant ` subtitle `"),
            ("null", "invalid type: null"),
        ] {
            let json = format!(
                r#"[{{"target_video": "a.mkv", "asset_path": "a.ass", "track_type": {track_type}}}]"#
            );
            let err = parse_assignments_json(&json).unwrap_err();
            assert!(
                err.starts_with(&format!("Invalid track assignment: {reason}")),
                "{json}: {err}"
            );
        }
        assert!(parse_assignments_json("{")
            .unwrap_err()
            .starts_with("Invalid assignments JSON:"));
    }

    #[test]
    fn test_track_assignment_wire_format_round_trips() {
        let canonical = r#"[{"target_video":"Season 1/ep01.mkv","asset_path":"subs/ep01.ass","track_type":"subtitle","language":"ind","title":"Indonesian","offset_ms":-250}]"#;
        let parsed = parse_assignments_json(canonical).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), canonical);

        // Omitted fields take their defaults; track_type is always written back.
        let minimal = r#"[{"target_video":"ep01.mkv","asset_path":"ep01.ass"}]"#;
        assert_eq!(
            serde_json::to_string(&parse_assignments_json(minimal).unwrap()).unwrap(),
            r#"[{"target_video":"ep01.mkv","asset_path":"ep01.ass","track_type":"subtitle"}]"#
        );
    }

    #[test]
    fn test_parse_assignments_json_reject_non_string() {
        for (json, expected) in [
            (
                r#"{"assignments": {"ep01.mkv": 123}}"#,
                "Expected string asset path for 'ep01.mkv', got 123",
            ),
            (
                r#"{"assignments": {"ep02.mkv": false}}"#,
                "Expected string asset path for 'ep02.mkv', got false",
            ),
            (
                r#"{"ep01.mkv": 456}"#,
                "Expected string asset path for 'ep01.mkv', got 456",
            ),
            (
                r#"{"ep01.mkv": {"sub": "ep01.ass"}}"#,
                r#"Expected string asset path for 'ep01.mkv', got {"sub":"ep01.ass"}"#,
            ),
        ] {
            assert_eq!(parse_assignments_json(json).unwrap_err(), expected);
        }
    }

    #[test]
    fn test_batch_request_preset_validation() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.preset = Some("balanced".into());
        assert!(resolved(&req).is_ok());

        req.preset = Some("non_existent_preset".into());
        assert_eq!(
            resolved(&req).unwrap_err(),
            "Unknown preset profile: 'non_existent_preset'"
        );

        // The preset is checked after the numeric settings and before the assignments and
        // selected items, so the first failure is the same as it was for `validate`.
        req.assignments = Some(vec![subtitle("ghost.mkv", "ghost.ass")]);
        req.selected_items = Some(vec![]);
        assert_eq!(
            resolved(&req).unwrap_err(),
            "Unknown preset profile: 'non_existent_preset'"
        );
        req.complexity = Some(11);
        assert!(resolved(&req)
            .unwrap_err()
            .starts_with("Encoder complexity must be"));
    }

    #[test]
    fn test_batch_request_auxiliary_directory_validation() {
        let dir = tempdir().unwrap();
        let file_path = touch(dir.path(), "dummy.txt");

        let mut req = minimal_request(dir.path());
        req.fonts_dir = Some(dir.path().join("non_existent_fonts"));
        let err_fonts = resolved(&req).unwrap_err();
        assert!(err_fonts.contains("Fonts directory does not exist or is not a directory"));

        req.fonts_dir = None;
        req.assets_dir = Some(dir.path().join("non_existent_assets"));
        let err_assets = resolved(&req).unwrap_err();
        assert!(err_assets.contains("Assets directory does not exist or is not a directory"));

        req.assets_dir = None;
        req.out_dir = Some(file_path);
        let err_out = resolved(&req).unwrap_err();
        assert!(err_out.contains("Output directory path points to an existing file"));
    }

    #[test]
    fn test_validate_selected_items() {
        let dir = tempdir().unwrap();
        for name in ["ep1.mkv", "ep2.mkv", "EP3.MKV", "notes.txt"] {
            touch(dir.path(), name);
        }
        let mut req = minimal_request(dir.path());

        req.selected_items = Some(vec!["ep2.mkv".into(), "ep1.mkv".into(), "EP3.MKV".into()]);
        assert!(resolved(&req).is_ok());

        let cases: [(&[&str], &str); 5] = [
            (&[], "selected_items cannot be empty when specified"),
            (
                &["ep1.mkv", "./ep1.mkv"],
                "Duplicate entry in selected_items: 'ep1.mkv'",
            ),
            (
                &["../secret.mkv"],
                "Invalid relative path in selected_items: '../secret.mkv'",
            ),
            (
                &["ep1.mkv", "notes.txt"],
                "Selected item is not an MKV file: 'notes.txt'",
            ),
            (&["ghost.mkv"], "Selected item does not exist:"),
        ];
        for (items, expected) in cases {
            req.selected_items = Some(items.iter().map(|s| s.to_string()).collect());
            let err = resolved(&req).unwrap_err();
            assert!(err.starts_with(expected), "{items:?}: {err}");
        }
    }

    #[test]
    fn test_validate_assignment_not_in_selected_items_fails() {
        let dir = tempdir().unwrap();
        for name in ["ep1.mkv", "ep2.mkv", "ep1.ass", "ep2.ass"] {
            touch(dir.path(), name);
        }

        let mut req = minimal_request(dir.path());
        req.selected_items = Some(vec!["ep1.mkv".into()]);
        req.assignments = Some(vec![
            subtitle("ep1.mkv", "ep1.ass"),
            subtitle("ep2.mkv", "ep2.ass"),
        ]);
        assert_eq!(
            resolved(&req).unwrap_err(),
            "Track assignment targets video 'ep2.mkv' which is not in selected_items"
        );
    }
}
