//! Strongly-typed batch execution requests and track assignments.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cpu::{physical_core_count, transcode_governor_threads};
use crate::pipeline::{PipelineConfig, PipelineMode};
use crate::preset::lookup_preset;

/// Explicit manual track assignment attaching an asset to a target video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackAssignment {
    /// Target video file path, relative to `input_dir` (e.g. "Season 1/Ep01.mkv")
    pub target_video: String,
    /// Absolute or relative path to the asset file (e.g. subtitles or audio)
    pub asset_path: PathBuf,
    /// Track type / role. Only `"subtitle"` is accepted; any other value fails validation.
    /// Defaults to "subtitle".
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

/// Normalizes a relative path string by converting backslashes to forward slashes,
/// collapsing redundant separators, and stripping relative `.` (`CurDir`) components.
///
/// This is the canonical form for comparing item IDs, selected items, and assignment
/// targets; both sides of a comparison must go through it.
pub fn clean_relative_path(s: &str) -> String {
    let norm = s.trim().replace('\\', "/");
    let parts: Vec<_> = Path::new(&norm)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

/// Maps each selection to the index its canonical ID has in `index_by_id` (keyed by
/// [`clean_relative_path`] IDs), in selection order with repeats dropped. Fails naming every
/// selection that matches no discovered item.
pub(crate) fn resolve_selected_items(
    index_by_id: &HashMap<String, usize>,
    selected: &[String],
) -> Result<Vec<usize>, String> {
    let mut seen = HashSet::with_capacity(selected.len());
    let mut ordered = Vec::with_capacity(selected.len());
    let mut unmatched = Vec::new();
    for item in selected {
        match index_by_id.get(&clean_relative_path(item)) {
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
    fn clean_target_video(&self) -> String {
        clean_relative_path(&self.target_video)
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
    /// Per-job FFmpeg threads override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg_threads: Option<usize>,
    /// Global subtitle offset in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_offset: Option<f64>,
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
            sub_offset: None,
            dry_run: None,
            no_cache: None,
            assignments: None,
            selected_items: None,
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

        if let Some(b) = self.bitrate.as_deref().filter(|b| !is_valid_bitrate(b)) {
            return Err(format!("Invalid audio bitrate format: '{b}'"));
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
                let target_norm = assignment.target_video.trim().replace('\\', "/");
                if clean.is_empty() || !is_contained_relative_path(&target_norm) {
                    return Err(format!(
                        "Invalid relative target video path: '{}'",
                        assignment.target_video
                    ));
                }

                if !has_mkv_extension(&clean) {
                    return Err(format!(
                        "Assigned target video is not an MKV file: '{clean}'"
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

                if let Some(tt) = &assignment.track_type {
                    if !tt.trim().eq_ignore_ascii_case("subtitle") {
                        return Err(
                            "Only 'subtitle' track assignments are currently supported".into()
                        );
                    }
                }

                let target_full = self.input_dir.join(&clean);
                if !seen_videos.insert(clean.clone()) {
                    return Err(format!(
                        "Multiple track assignments for target video '{clean}' are not supported; multi-track muxing is deferred"
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

            let mut seen_items: HashSet<String> = HashSet::new();
            for item in items {
                let norm = item.trim().replace('\\', "/");
                if norm.is_empty() || !is_contained_relative_path(&norm) {
                    return Err(format!("Invalid relative path in selected_items: '{item}'"));
                }

                let clean = clean_relative_path(&norm);
                if !seen_items.insert(clean.clone()) {
                    return Err(format!("Duplicate entry in selected_items: '{clean}'"));
                }
                if !has_mkv_extension(&clean) {
                    return Err(format!("Selected item is not an MKV file: '{clean}'"));
                }
                let full_path = self.input_dir.join(&clean);
                if !full_path.is_file() {
                    return Err(format!(
                        "Selected item does not exist: '{}'",
                        full_path.display()
                    ));
                }
            }

            if let Some(assignments) = &self.assignments {
                for assignment in assignments {
                    let clean_target = assignment.clean_target_video();
                    if !seen_items.contains(&clean_target) {
                        return Err(format!(
                            "Track assignment targets video '{clean_target}' which is not in selected_items"
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Converts the `BatchRequest` into internal `PipelineConfig`, applying
    /// preset profile baseline defaults and layering explicit parameter overrides.
    pub fn into_pipeline_config(self) -> Result<PipelineConfig, String> {
        let preset = self
            .preset
            .as_deref()
            .map(|id| lookup_preset(id).ok_or_else(|| format!("Unknown preset profile: '{id}'")))
            .transpose()?;
        let (preset_bitrate, preset_complexity) = match preset {
            Some(p) => (Some(p.bitrate), Some(p.complexity)),
            None => (None, None),
        };

        let physical_cores = physical_core_count();
        let workers = self.workers.unwrap_or_else(crate::cpu::default_workers);
        let defaults = PipelineConfig::default();

        Ok(PipelineConfig {
            mode: self.mode,
            input_dir: self.input_dir,
            out_dir: self.out_dir,
            fonts_dir: self.fonts_dir,
            assets_dir: self.assets_dir,
            bitrate: self
                .bitrate
                .map(|b| b.trim().to_owned())
                .or(preset_bitrate)
                .unwrap_or(defaults.bitrate),
            complexity: self
                .complexity
                .or(preset_complexity)
                .unwrap_or(defaults.complexity),
            workers,
            io_jobs: self.io_jobs.unwrap_or(defaults.io_jobs),
            ffmpeg_threads: self
                .ffmpeg_threads
                .unwrap_or_else(|| transcode_governor_threads(physical_cores, workers)),
            sub_offset: self.sub_offset.unwrap_or(defaults.sub_offset),
            dry_run: self.dry_run.unwrap_or(defaults.dry_run),
            no_cache: self.no_cache.unwrap_or(defaults.no_cache),
            track_assignments: self.assignments,
            selected_items: self.selected_items,
            ..defaults
        })
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
        Value::Array(_) => serde_json::from_value(value)
            .map_err(|_| "Unrecognized assignments JSON schema".to_string()),
        Value::Object(obj) => match obj.get("assignments") {
            Some(Value::Object(inner)) => legacy_assignment_map(inner),
            Some(inner @ Value::Array(_)) => {
                serde_json::from_value(inner.clone()).or_else(|_| legacy_assignment_map(&obj))
            }
            _ => legacy_assignment_map(&obj),
        },
        _ => Err("Unrecognized assignments JSON schema".into()),
    }
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
                track_type: Some("subtitle".into()),
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
            track_type: Some("subtitle".into()),
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

    fn validate_with(dir: &Path, assignments: Vec<TrackAssignment>) -> Result<(), String> {
        let mut req = minimal_request(dir);
        req.assignments = Some(assignments);
        req.validate()
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
        assert!(req.validate().is_ok());

        let config = req.into_pipeline_config().unwrap();
        assert_eq!(config.bitrate, "256k");
        assert_eq!(config.complexity, 8);
        assert_eq!(config.workers, 4);
        assert_eq!(config.track_assignments.map(|a| a.len()), Some(1));
    }

    #[test]
    fn test_into_pipeline_config_layers_overrides_over_preset() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.preset = Some("hifi".into());
        let config = req.clone().into_pipeline_config().unwrap();
        assert!(!config.dry_run);
        assert_eq!(config.bitrate, "320k");
        assert_eq!(config.complexity, 10);

        req.bitrate = Some("256k".into());
        req.dry_run = Some(true);
        let config = req.into_pipeline_config().unwrap();
        assert_eq!(config.bitrate, "256k");
        assert_eq!(config.complexity, 10);
        assert!(config.dry_run);
    }

    #[test]
    fn test_into_pipeline_config_forwards_trimmed_bitrate() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        req.bitrate = Some(" 320K ".into());
        assert_eq!(req.into_pipeline_config().unwrap().bitrate, "320K");
    }

    #[test]
    fn test_into_pipeline_config_default_workers_hardware_concurrency() {
        let dir = tempdir().unwrap();
        let config = minimal_request(dir.path()).into_pipeline_config().unwrap();
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
                req.validate().unwrap_err(),
                "Worker thread count must be between 1 and 64"
            );
        }
        for ok in [1, 64] {
            req.workers = Some(ok);
            assert!(req.validate().is_ok());
        }
    }

    #[test]
    fn test_batch_request_validation_io_jobs() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        // 0 means unbounded I/O.
        for ok in [0, 64] {
            req.io_jobs = Some(ok);
            assert!(req.validate().is_ok());
        }
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
        for bad in [0, 33] {
            req.ffmpeg_threads = Some(bad);
            assert_eq!(
                req.validate().unwrap_err(),
                "FFmpeg thread count must be between 1 and 32"
            );
        }
        for ok in [1, 32] {
            req.ffmpeg_threads = Some(ok);
            assert!(req.validate().is_ok());
        }
    }

    #[test]
    fn test_batch_request_validation_complexity() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for ok in [0, 10] {
            req.complexity = Some(ok);
            assert!(req.validate().is_ok());
        }
        req.complexity = Some(11);
        assert_eq!(
            req.validate().unwrap_err(),
            "Encoder complexity must be between 0 and 10, got 11"
        );
    }

    #[test]
    fn test_batch_request_validation_bitrate() {
        let dir = tempdir().unwrap();
        let mut req = minimal_request(dir.path());
        for ok in ["192k", " 320K ", "1M", "96000"] {
            req.bitrate = Some(ok.into());
            assert!(req.validate().is_ok(), "bitrate {ok:?} should be valid");
        }
        for bad in ["", "k", "0k", "12.5k", "-192k", "192kb", "1m"] {
            req.bitrate = Some(bad.into());
            assert_eq!(
                req.validate().unwrap_err(),
                format!("Invalid audio bitrate format: '{bad}'")
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
                validate_with(dir.path(), vec![assignment]).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn test_batch_request_validation_root_escape() {
        let dir = tempdir().unwrap();
        let sub_path = touch(dir.path(), "ep01.ass");

        for bad_target in ["/ep01.mkv", r"\ep01.mkv", "", "./", r".\", "././"] {
            let err = validate_with(dir.path(), vec![subtitle(bad_target, &sub_path)]).unwrap_err();
            assert!(
                err.contains("Invalid relative target video path"),
                "expected error for '{bad_target}', got '{err}'"
            );
        }
    }

    #[test]
    fn test_batch_request_validation_track_type() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        let sub_path = touch(dir.path(), "ep01.ass");
        let with_type = |track_type: Option<&str>| TrackAssignment {
            track_type: track_type.map(Into::into),
            ..subtitle("ep01.mkv", &sub_path)
        };

        assert_eq!(
            validate_with(dir.path(), vec![with_type(Some("audio"))]).unwrap_err(),
            "Only 'subtitle' track assignments are currently supported"
        );
        // Case-insensitive; an absent type means subtitle.
        for ok in [Some("subtitle"), Some("SUBTITLE"), None] {
            assert!(validate_with(dir.path(), vec![with_type(ok)]).is_ok());
        }
    }

    #[test]
    fn test_validate_assignment_rejects_existing_non_mkv_target() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "notes.txt");
        touch(dir.path(), "ep1.ass");

        assert_eq!(
            validate_with(dir.path(), vec![subtitle("notes.txt", "ep1.ass")]).unwrap_err(),
            "Assigned target video is not an MKV file: 'notes.txt'"
        );

        touch(dir.path(), "EP2.MKV");
        assert!(validate_with(dir.path(), vec![subtitle("EP2.MKV", "ep1.ass")]).is_ok());
    }

    #[test]
    fn test_batch_request_fail_fast_missing_target_and_asset() {
        let dir = tempdir().unwrap();
        let sub_path = touch(dir.path(), "ep01.ass");
        let err = validate_with(dir.path(), vec![subtitle("ghost.mkv", &sub_path)]).unwrap_err();
        assert!(
            err.contains("Assigned target video file does not exist"),
            "{err}"
        );

        touch(dir.path(), "ep01.mkv");
        let missing = dir.path().join("missing_sub.ass");
        let err = validate_with(dir.path(), vec![subtitle("ep01.mkv", missing)]).unwrap_err();
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
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_batch_request_duplicate_assignment_after_normalization() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "ep01.mkv");
        touch(dir.path(), "ep02.mkv");
        touch(dir.path(), "Season 1/ep01.mkv");
        let sub_path = touch(dir.path(), "ep01.ass");

        // Leading `./` and `.\` are accepted.
        assert!(validate_with(
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
            let err = validate_with(
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
        assert_eq!(parsed[0].track_type, None);

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

        for unrecognized in [r#"[{"asset_path": "x.ass"}]"#, "42"] {
            assert_eq!(
                parse_assignments_json(unrecognized).unwrap_err(),
                "Unrecognized assignments JSON schema",
                "{unrecognized}"
            );
        }
        for (json, expected) in [
            (
                r#"{"assignments": 1}"#,
                "Expected string asset path for 'assignments', got 1",
            ),
            (
                r#"{"assignments": [{"asset_path": "x.ass"}]}"#,
                r#"Expected string asset path for 'assignments', got [{"asset_path":"x.ass"}]"#,
            ),
        ] {
            assert_eq!(
                parse_assignments_json(json).unwrap_err(),
                expected,
                "{json}"
            );
        }
        assert!(parse_assignments_json("{")
            .unwrap_err()
            .starts_with("Invalid assignments JSON:"));
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
        assert!(req.validate().is_ok());

        req.preset = Some("non_existent_preset".into());
        assert_eq!(
            req.validate().unwrap_err(),
            "Unknown preset profile: 'non_existent_preset'"
        );
    }

    #[test]
    fn test_batch_request_auxiliary_directory_validation() {
        let dir = tempdir().unwrap();
        let file_path = touch(dir.path(), "dummy.txt");

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

    #[test]
    fn test_validate_selected_items() {
        let dir = tempdir().unwrap();
        for name in ["ep1.mkv", "ep2.mkv", "EP3.MKV", "notes.txt"] {
            touch(dir.path(), name);
        }
        let mut req = minimal_request(dir.path());

        req.selected_items = Some(vec!["ep2.mkv".into(), "ep1.mkv".into(), "EP3.MKV".into()]);
        assert!(req.validate().is_ok());

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
            let err = req.validate().unwrap_err();
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
            req.validate().unwrap_err(),
            "Track assignment targets video 'ep2.mkv' which is not in selected_items"
        );
    }
}
