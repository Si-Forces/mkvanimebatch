//! Strongly-typed batch execution requests and track assignments.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pipeline::{PipelineConfig, PipelineMode};
use crate::preset::lookup_preset;

/// Explicit manual track assignment attaching an asset to a target video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackAssignment {
    /// Target video file path, relative to `input_dir` (e.g. "Season 1/Ep01.mkv")
    pub target_video: String,
    /// Absolute or relative path to the asset file (e.g. subtitles or audio)
    pub asset_path: PathBuf,
    /// Track type / role (e.g. "subtitle", "audio", "signs_songs"). Defaults to "subtitle".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_type: Option<String>,
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

impl TrackAssignment {
    /// Returns the normalized target video path relative to input directory,
    /// with backslashes converted to forward slashes and leading relative prefixes (`./`) stripped.
    pub fn clean_target_video(&self) -> String {
        let norm = self.target_video.trim().replace('\\', "/");
        let mut clean = norm.as_str();
        while let Some(rest) = clean.strip_prefix("./") {
            clean = rest;
        }
        clean.to_string()
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
    /// Per-job FFmpeg threads override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg_threads: Option<usize>,
    /// Global subtitle offset in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_offset: Option<f64>,
    /// Whether to retain extracted/embedded source fonts
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_fonts: Option<bool>,
    /// Whether to perform a dry run (plan without writing files)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    /// Whether to bypass and ignore persistent inspect cache
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_cache: Option<bool>,
    /// Explicit track assignments
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<Vec<TrackAssignment>>,
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
            sub_offset: None,
            keep_fonts: None,
            dry_run: None,
            no_cache: None,
            assignments: None,
        }
    }
}

impl BatchRequest {
    /// Synchronously validate the batch request parameters and track assignments
    /// before acquiring any locks or starting execution tasks.
    pub fn validate(&self) -> Result<(), String> {
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
            if !(1..=64).contains(&w) {
                return Err("Worker thread count must be between 1 and 64".into());
            }
        }

        if let Some(io) = self.io_jobs {
            if io > 64 {
                return Err("I/O job count must be between 0 and 64".into());
            }
        }

        if let Some(t) = self.ffmpeg_threads {
            if !(1..=32).contains(&t) {
                return Err("FFmpeg thread count must be between 1 and 32".into());
            }
        }

        if let Some(b) = &self.bitrate {
            let trimmed = b.trim();
            let digits = trimmed
                .strip_suffix(['k', 'K', 'm', 'M'])
                .unwrap_or(trimmed);
            if digits.is_empty()
                || !digits.chars().all(|c| c.is_ascii_digit())
                || digits.parse::<u64>().unwrap_or(0) == 0
            {
                return Err(format!("Invalid audio bitrate format: '{b}'"));
            }
        }

        if let Some(c) = self.complexity {
            if c > 10 {
                return Err(format!(
                    "Encoder complexity must be between 0 and 10, got {c}"
                ));
            }
        }

        if let Some(p) = &self.preset {
            if lookup_preset(p).is_none() {
                return Err(format!("Unknown preset profile: '{p}'"));
            }
        }

        if let Some(assignments) = &self.assignments {
            let mut seen_videos: HashSet<String> = HashSet::new();

            for assignment in assignments {
                let clean = assignment.clean_target_video();
                if clean.is_empty() {
                    return Err(format!(
                        "Invalid relative target video path: '{}'",
                        assignment.target_video
                    ));
                }

                let target_norm = assignment.target_video.trim().replace('\\', "/");
                let target_path = Path::new(&target_norm);
                let is_target_drive = target_norm.len() >= 2
                    && target_norm.as_bytes()[1] == b':'
                    && target_norm.as_bytes()[0].is_ascii_alphabetic();
                if target_norm.starts_with('/')
                    || target_path.has_root()
                    || is_target_drive
                    || !target_path.is_relative()
                    || target_path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(format!(
                        "Invalid relative target video path: '{}'",
                        assignment.target_video
                    ));
                }

                let asset_str = assignment.asset_path.to_string_lossy().replace('\\', "/");
                let asset_norm_path = Path::new(&asset_str);
                let is_asset_drive = asset_str.len() >= 2
                    && asset_str.as_bytes()[1] == b':'
                    && asset_str.as_bytes()[0].is_ascii_alphabetic();
                if !is_asset_drive
                    && asset_norm_path.is_relative()
                    && asset_norm_path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(format!(
                        "Invalid relative asset path: '{}'",
                        assignment.asset_path.display()
                    ));
                }

                if let Some(tt) = &assignment.track_type {
                    if !tt.trim().eq_ignore_ascii_case("subtitle") {
                        return Err(
                            "Only 'subtitle' track assignments are currently supported".into()
                        );
                    }
                }

                if !seen_videos.insert(clean.to_string()) {
                    return Err(format!(
                        "Multiple track assignments for target video '{}' are not supported; multi-track muxing is deferred",
                        assignment.target_video
                    ));
                }

                // Check that the target video file exists
                let target_full = self.input_dir.join(clean);
                if !target_full.is_file() {
                    return Err(format!(
                        "Assigned target video file does not exist: '{}'",
                        target_full.display()
                    ));
                }

                // Fail-fast preflight validation per ADR-0008: assigned asset must exist on disk
                // Checks relative to input_dir, or relative to assets_dir if configured.
                let asset_exists = if !is_asset_drive && asset_norm_path.is_relative() {
                    let in_input = self.input_dir.join(asset_norm_path);
                    in_input.is_file()
                        || self
                            .assets_dir
                            .as_ref()
                            .map(|d| d.join(asset_norm_path).is_file())
                            .unwrap_or(false)
                } else {
                    assignment.asset_path.is_file()
                };

                if !asset_exists {
                    return Err(format!(
                        "Assigned asset file does not exist: '{}'",
                        if !is_asset_drive && asset_norm_path.is_relative() {
                            self.input_dir.join(asset_norm_path).display().to_string()
                        } else {
                            assignment.asset_path.display().to_string()
                        }
                    ));
                }
            }
        }

        Ok(())
    }

    /// Converts the `BatchRequest` into internal `PipelineConfig`, applying
    /// preset profile baseline defaults and layering explicit parameter overrides.
    pub fn into_pipeline_config(self) -> Result<PipelineConfig, String> {
        let (preset_bitrate, preset_complexity, preset_keep_fonts) =
            if let Some(preset_id) = &self.preset {
                if let Some(preset) = lookup_preset(preset_id) {
                    (
                        Some(preset.bitrate),
                        Some(preset.complexity),
                        Some(preset.keep_fonts),
                    )
                } else {
                    return Err(format!("Unknown preset profile: '{preset_id}'"));
                }
            } else {
                (None, None, None)
            };

        let default_workers = std::thread::available_parallelism()
            .map(|p| p.get().clamp(1, 16))
            .unwrap_or(4);

        let mut config = PipelineConfig {
            mode: self.mode,
            input_dir: self.input_dir.clone(),
            out_dir: self.out_dir,
            fonts_dir: self.fonts_dir,
            assets_dir: self.assets_dir.clone(),
            bitrate: self
                .bitrate
                .or(preset_bitrate)
                .unwrap_or_else(|| "192k".to_string()),
            complexity: self.complexity.or(preset_complexity).unwrap_or(10),
            keep_fonts: self.keep_fonts.or(preset_keep_fonts).unwrap_or(false),
            workers: self.workers.unwrap_or(default_workers),
            io_jobs: self.io_jobs.unwrap_or(2),
            ffmpeg_threads: self.ffmpeg_threads.unwrap_or(2),
            sub_offset: self.sub_offset.unwrap_or(0.0),
            dry_run: self.dry_run.unwrap_or(false),
            no_cache: self.no_cache.unwrap_or(false),
            ..Default::default()
        };

        if let Some(assignments) = self.assignments {
            config.track_assignments = Some(assignments.clone());
            config.sub_assignments = Some(assignments_to_sub_map(
                &assignments,
                &self.input_dir,
                self.assets_dir.as_deref(),
            ));
        }

        Ok(config)
    }
}

/// Helper to convert a list of `TrackAssignment`s into a lookup map keyed by normalized
/// target path and file name, resolving relative asset paths against input_dir (or assets_dir fallback).
pub fn assignments_to_track_map(
    assignments: &[TrackAssignment],
    input_dir: &Path,
    assets_dir: Option<&Path>,
) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for a in assignments {
        let resolved = if a.asset_path.is_relative() {
            let from_input = input_dir.join(&a.asset_path);
            if from_input.is_file() {
                from_input
            } else if let Some(assets) = assets_dir {
                let from_assets = assets.join(&a.asset_path);
                if from_assets.is_file() {
                    from_assets
                } else {
                    from_input
                }
            } else {
                from_input
            }
        } else {
            a.asset_path.clone()
        };

        let clean = a.clean_target_video();
        let file_name = Path::new(&clean)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| clean.clone());

        map.insert(clean, resolved.clone());
        map.insert(file_name, resolved);
    }
    map
}

/// Backward-compatible alias for `assignments_to_track_map`.
pub use assignments_to_track_map as assignments_to_sub_map;

/// Parse track assignments from JSON, supporting both `Vec<TrackAssignment>` and
/// legacy `{ "video.mkv": "sub.ass" }` map format.
pub fn parse_assignments_json(json_str: &str) -> Result<Vec<TrackAssignment>, String> {
    let trimmed = json_str.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let parsed_val: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Invalid assignments JSON: {e}"))?;

    // Format 1: Direct JSON array of TrackAssignment
    if parsed_val.is_array() {
        if let Ok(vec) = serde_json::from_value::<Vec<TrackAssignment>>(parsed_val) {
            return Ok(vec);
        }
        return Err("Unrecognized assignments JSON schema".into());
    }

    // Format 2: Object with "assignments" key
    if let Some(obj_val) = parsed_val.get("assignments") {
        if obj_val.is_array() {
            if let Ok(vec) = serde_json::from_value::<Vec<TrackAssignment>>(obj_val.clone()) {
                return Ok(vec);
            }
        }
        if let Some(map) = obj_val.as_object() {
            let mut list = Vec::new();
            for (k, v) in map {
                let p = v
                    .as_str()
                    .ok_or_else(|| format!("Expected string asset path for '{k}', got {v}"))?;
                list.push(TrackAssignment {
                    target_video: k.clone(),
                    asset_path: PathBuf::from(p),
                    track_type: Some("subtitle".into()),
                    language: None,
                    title: None,
                    offset_ms: None,
                });
            }
            return Ok(list);
        }
    }

    // Format 3: Flat legacy map: { "video.mkv": "sub.ass" }
    if let Some(map) = parsed_val.as_object() {
        let mut list = Vec::new();
        for (k, v) in map {
            let p = v
                .as_str()
                .ok_or_else(|| format!("Expected string asset path for '{k}', got {v}"))?;
            list.push(TrackAssignment {
                target_video: k.clone(),
                asset_path: PathBuf::from(p),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            });
        }
        return Ok(list);
    }

    Err("Unrecognized assignments JSON schema".into())
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

    #[test]
    fn test_batch_request_validation_success() {
        let dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        let sub_path = dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy subtitle").unwrap();

        let mut req = minimal_request(dir.path());
        req.preset = Some("balanced".into());
        req.bitrate = Some("256k".into());
        req.complexity = Some(8);
        req.workers = Some(4);
        req.io_jobs = Some(2);
        req.keep_fonts = Some(true);
        req.dry_run = Some(false);
        req.no_cache = Some(false);
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: sub_path,
            track_type: Some("subtitle".into()),
            language: Some("ind".into()),
            title: None,
            offset_ms: None,
        }]);

        assert!(req.validate().is_ok());

        let config = req.into_pipeline_config().unwrap();
        assert_eq!(config.bitrate, "256k"); // overridden
        assert_eq!(config.complexity, 8); // overridden
        assert!(config.keep_fonts); // preset + override match
        assert_eq!(config.workers, 4);
    }

    #[test]
    fn test_batch_request_validation_workers() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());

        req.workers = Some(0);
        assert_eq!(
            req.validate().unwrap_err(),
            "Worker thread count must be between 1 and 64"
        );

        req.workers = Some(65);
        assert_eq!(
            req.validate().unwrap_err(),
            "Worker thread count must be between 1 and 64"
        );

        req.workers = Some(1);
        assert!(req.validate().is_ok());

        req.workers = Some(64);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_into_pipeline_config_default_workers_hardware_concurrency() {
        let dir = tempdir().unwrap();
        let req = minimal_request(dir.path());
        assert!(req.workers.is_none());
        let config = req.into_pipeline_config().unwrap();
        let expected = std::thread::available_parallelism()
            .map(|p| p.get().clamp(1, 16))
            .unwrap_or(4);
        assert_eq!(config.workers, expected);
    }

    #[test]
    fn test_batch_request_validation_io_jobs() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());

        // io_jobs == Some(0) succeeds validation (unbounded I/O)
        req.io_jobs = Some(0);
        assert!(req.validate().is_ok());

        req.io_jobs = Some(64);
        assert!(req.validate().is_ok());

        req.io_jobs = Some(65);
        assert_eq!(
            req.validate().unwrap_err(),
            "I/O job count must be between 0 and 64"
        );
    }

    #[test]
    fn test_batch_request_validation_ffmpeg_threads() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());

        req.ffmpeg_threads = Some(0);
        assert_eq!(
            req.validate().unwrap_err(),
            "FFmpeg thread count must be between 1 and 32"
        );

        req.ffmpeg_threads = Some(33);
        assert_eq!(
            req.validate().unwrap_err(),
            "FFmpeg thread count must be between 1 and 32"
        );

        req.ffmpeg_threads = Some(1);
        assert!(req.validate().is_ok());

        req.ffmpeg_threads = Some(32);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_batch_request_validation_path_traversal() {
        let dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        let sub_path = dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy sub").unwrap();

        // 1. Target video with parent directory traversal
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "../outside.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            "Invalid relative target video path: '../outside.mkv'"
        );

        // 2. Target video with absolute path
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "/absolute/path.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            "Invalid relative target video path: '/absolute/path.mkv'"
        );

        // 3. Asset path relative traversal
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: PathBuf::from("../outside.ass"),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            "Invalid relative asset path: '../outside.ass'"
        );

        // 4. Backslash parent traversal (cross-platform check)
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: r"..\outside.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            r"Invalid relative target video path: '..\outside.mkv'"
        );

        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: PathBuf::from(r"sub\..\..\outside.ass"),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            r"Invalid relative asset path: 'sub\..\..\outside.ass'"
        );
    }

    #[test]
    fn test_batch_request_validation_track_type() {
        let dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        let sub_path = dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy sub").unwrap();

        // Unsupported track type "audio"
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("audio".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert_eq!(
            req.validate().unwrap_err(),
            "Only 'subtitle' track assignments are currently supported"
        );

        // Valid "subtitle"
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert!(req.validate().is_ok());

        // Valid "SUBTITLE" (case-insensitive)
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: sub_path.clone(),
            track_type: Some("SUBTITLE".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert!(req.validate().is_ok());

        // Valid None (defaults to subtitle)
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: sub_path,
            track_type: None,
            language: None,
            title: None,
            offset_ms: None,
        }]);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_batch_request_fail_fast_missing_asset() {
        let dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: dir.path().join("missing_sub.ass"),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);

        let err = req.validate().unwrap_err();
        assert!(err.contains("Assigned asset file does not exist"));
    }

    #[test]
    fn test_batch_request_duplicate_assignment_error() {
        let dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        let sub_path = dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy sub").unwrap();

        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![
            TrackAssignment {
                target_video: "ep01.mkv".into(),
                asset_path: sub_path.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
            TrackAssignment {
                target_video: "ep01.mkv".into(),
                asset_path: sub_path,
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
        ]);

        let err = req.validate().unwrap_err();
        assert!(err.contains(
            "Multiple track assignments for target video 'ep01.mkv' are not supported; multi-track muxing is deferred"
        ));
    }

    #[test]
    fn test_parse_assignments_json_formats() {
        // Format 1: Vec<TrackAssignment>
        let json1 = r#"[{"target_video": "ep01.mkv", "asset_path": "/tmp/ep01.ass", "track_type": "subtitle"}]"#;
        let res1 = parse_assignments_json(json1).unwrap();
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0].target_video, "ep01.mkv");

        // Format 2: Flat map
        let json2 = r#"{"ep02.mkv": "/tmp/ep02.ass"}"#;
        let res2 = parse_assignments_json(json2).unwrap();
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0].target_video, "ep02.mkv");
        assert_eq!(res2[0].asset_path, PathBuf::from("/tmp/ep02.ass"));
    }

    #[test]
    fn test_batch_request_preset_validation() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.preset = Some("balanced".into());
        assert!(req.validate().is_ok());

        req.preset = Some("non_existent_preset".into());
        let err = req.validate().unwrap_err();
        assert!(err.contains("Unknown preset profile: 'non_existent_preset'"));
    }

    #[test]
    fn test_batch_request_asset_in_assets_dir() {
        let dir = tempdir().unwrap();
        let assets_dir = tempdir().unwrap();
        let video_path = dir.path().join("ep01.mkv");
        std::fs::write(&video_path, b"dummy video").unwrap();

        // Subtitle exists in assets_dir instead of input_dir
        let sub_path = assets_dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy sub").unwrap();

        let mut req = minimal_request(dir.path());
        req.assets_dir = Some(assets_dir.path().to_path_buf());
        req.assignments = Some(vec![TrackAssignment {
            target_video: "ep01.mkv".into(),
            asset_path: PathBuf::from("ep01.ass"),
            track_type: Some("subtitle".into()),
            language: None,
            title: None,
            offset_ms: None,
        }]);

        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_batch_request_validation_root_escape() {
        let dir = tempdir().unwrap();
        let sub_path = dir.path().join("ep01.ass");
        std::fs::write(&sub_path, b"dummy sub").unwrap();

        for bad_target in ["/ep01.mkv", r"\ep01.mkv", "", "./", r".\", "././"] {
            let mut req = minimal_request(dir.path());
            req.assignments = Some(vec![TrackAssignment {
                target_video: bad_target.into(),
                asset_path: sub_path.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            }]);
            let err = req.validate().unwrap_err();
            assert!(
                err.contains("Invalid relative target video path"),
                "expected error for '{bad_target}', got '{err}'"
            );
        }
    }

    #[test]
    fn test_batch_request_target_video_normalization() {
        let dir = tempdir().unwrap();
        let video_path1 = dir.path().join("ep01.mkv");
        let video_path2 = dir.path().join("ep02.mkv");
        let sub_path1 = dir.path().join("ep01.ass");
        let sub_path2 = dir.path().join("ep02.ass");
        std::fs::write(&video_path1, b"dummy video 1").unwrap();
        std::fs::write(&video_path2, b"dummy video 2").unwrap();
        std::fs::write(&sub_path1, b"dummy sub 1").unwrap();
        std::fs::write(&sub_path2, b"dummy sub 2").unwrap();

        // 1. Validation passes with leading ./ and .\ prefixes
        let mut req = minimal_request(dir.path());
        req.assignments = Some(vec![
            TrackAssignment {
                target_video: "./ep01.mkv".into(),
                asset_path: sub_path1.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
            TrackAssignment {
                target_video: r".\ep02.mkv".into(),
                asset_path: sub_path2.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
        ]);
        assert!(req.validate().is_ok());

        // 2. Duplicate detection catches normalized collisions between ./ep01.mkv and ep01.mkv
        let mut dup_req = minimal_request(dir.path());
        dup_req.assignments = Some(vec![
            TrackAssignment {
                target_video: "./ep01.mkv".into(),
                asset_path: sub_path1.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
            TrackAssignment {
                target_video: "ep01.mkv".into(),
                asset_path: sub_path1.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
        ]);
        let err = dup_req.validate().unwrap_err();
        assert!(err
            .contains("Multiple track assignments for target video 'ep01.mkv' are not supported"));

        // 3. Duplicate detection catches normalized collisions between .\ep01.mkv and ./ep01.mkv
        let mut dup_req2 = minimal_request(dir.path());
        dup_req2.assignments = Some(vec![
            TrackAssignment {
                target_video: "./ep01.mkv".into(),
                asset_path: sub_path1.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
            TrackAssignment {
                target_video: r".\ep01.mkv".into(),
                asset_path: sub_path1.clone(),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
        ]);
        let err2 = dup_req2.validate().unwrap_err();
        assert!(err2.contains(
            r"Multiple track assignments for target video '.\ep01.mkv' are not supported"
        ));

        // 4. assignments_to_sub_map normalizes ./ and .\ and inserts clean and file_name
        let assignments = vec![
            TrackAssignment {
                target_video: "././ep01.mkv".into(),
                asset_path: PathBuf::from("ep01.ass"),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
            TrackAssignment {
                target_video: r".\Season 1\ep02.mkv".into(),
                asset_path: PathBuf::from("ep02.ass"),
                track_type: Some("subtitle".into()),
                language: None,
                title: None,
                offset_ms: None,
            },
        ];
        let map = assignments_to_sub_map(&assignments, dir.path(), None);
        assert_eq!(map.get("ep01.mkv"), Some(&dir.path().join("ep01.ass")));
        assert_eq!(
            map.get("Season 1/ep02.mkv"),
            Some(&dir.path().join("ep02.ass"))
        );
        assert_eq!(map.get("ep02.mkv"), Some(&dir.path().join("ep02.ass")));
    }

    #[test]
    fn test_parse_assignments_json_reject_non_string() {
        // Format 2: Object with assignments key
        let json_fmt2_int = r#"{"assignments": {"ep01.mkv": 123}}"#;
        let err2 = parse_assignments_json(json_fmt2_int).unwrap_err();
        assert_eq!(err2, "Expected string asset path for 'ep01.mkv', got 123");

        let json_fmt2_bool = r#"{"assignments": {"ep02.mkv": false}}"#;
        let err2_bool = parse_assignments_json(json_fmt2_bool).unwrap_err();
        assert_eq!(
            err2_bool,
            "Expected string asset path for 'ep02.mkv', got false"
        );

        // Format 3: Flat legacy map
        let json_fmt3_int = r#"{"ep01.mkv": 456}"#;
        let err3 = parse_assignments_json(json_fmt3_int).unwrap_err();
        assert_eq!(err3, "Expected string asset path for 'ep01.mkv', got 456");

        let json_fmt3_obj = r#"{"ep01.mkv": {"sub": "ep01.ass"}}"#;
        let err3_obj = parse_assignments_json(json_fmt3_obj).unwrap_err();
        assert_eq!(
            err3_obj,
            r#"Expected string asset path for 'ep01.mkv', got {"sub":"ep01.ass"}"#
        );
    }

    #[test]
    fn test_batch_request_auxiliary_directory_validation() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("dummy.txt");
        std::fs::write(&file_path, b"not a dir").unwrap();

        let mut req = minimal_request(dir.path());
        req.fonts_dir = Some(dir.path().join("non_existent_fonts"));
        let err_fonts = req.validate().unwrap_err();
        assert!(err_fonts.contains("Fonts directory does not exist or is not a directory"));

        req.fonts_dir = None;
        req.assets_dir = Some(dir.path().join("non_existent_assets"));
        let err_assets = req.validate().unwrap_err();
        assert!(err_assets.contains("Assets directory does not exist or is not a directory"));

        req.assets_dir = None;
        req.out_dir = Some(file_path);
        let err_out = req.validate().unwrap_err();
        assert!(err_out.contains("Output directory path points to an existing file"));
    }
}
