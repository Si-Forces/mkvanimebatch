use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{ancestor_dirs, MkvTrack};
use crate::scan::{
    scan_assets, scan_mkv_files, InspectedVideo, PersistentInspectCache, ScanResults,
};

pub const MANIFEST_VERSION: u64 = 2;

/// Derives a root-relative POSIX path ID for job events and manifest items.
/// Normalizes backslashes `\` to `/`. If stripped is empty or points to root, returns `"."`.
pub fn get_item_id(path: &Path, root_dir: &Path) -> String {
    let rel = match path.strip_prefix(root_dir) {
        Ok(p) => p.to_path_buf(),
        Err(_) => {
            if let (Ok(p), Ok(r)) = (path.canonicalize(), root_dir.canonicalize()) {
                p.strip_prefix(&r)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| path.to_path_buf())
            } else {
                path.to_path_buf()
            }
        }
    };

    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." {
        ".".to_string()
    } else {
        s
    }
}

/// Parses standard video resolution buckets from `pixel_dimensions` string (e.g. "1920x1080").
pub fn parse_resolution(pixel_dimensions: &str) -> Option<String> {
    if let Some((w_str, h_str)) = pixel_dimensions.split_once('x') {
        if let (Ok(width), Ok(height)) = (w_str.trim().parse::<u32>(), h_str.trim().parse::<u32>())
        {
            if width >= 3800 || height >= 2100 {
                return Some("4K".to_string());
            }
            if width >= 2500 || height >= 1400 {
                return Some("1440p".to_string());
            }
            if width >= 1900 || height >= 1000 {
                return Some("1080p".to_string());
            }
            if width >= 1200 || height >= 700 {
                return Some("720p".to_string());
            }
            if height >= 480 {
                return Some("480p".to_string());
            }
        }
    }

    if pixel_dimensions.contains("3840") || pixel_dimensions.contains("2160") {
        return Some("4K".to_string());
    }
    if pixel_dimensions.contains("2560") || pixel_dimensions.contains("1440") {
        return Some("1440p".to_string());
    }
    if pixel_dimensions.contains("1920") || pixel_dimensions.contains("1080") {
        return Some("1080p".to_string());
    }
    if pixel_dimensions.contains("1280") || pixel_dimensions.contains("720") {
        return Some("720p".to_string());
    }

    None
}

/// Normalizes video codec strings into standard identifiers (HEVC, AVC, AV1, VP9).
pub fn parse_video_codec(codec: &str, codec_id: &str) -> Option<String> {
    let combined = format!("{codec} {codec_id}").to_lowercase();
    if combined.contains("hevc") || combined.contains("h.265") {
        Some("HEVC".to_string())
    } else if combined.contains("avc") || combined.contains("h.264") {
        Some("AVC".to_string())
    } else if combined.contains("av1") {
        Some("AV1".to_string())
    } else if combined.contains("vp9") {
        Some("VP9".to_string())
    } else if !codec.is_empty() {
        Some(codec.to_string())
    } else {
        None
    }
}

/// Detects audio codec name and whether the track is FLAC, formatted with channel label.
pub fn parse_audio_track_info(track: &MkvTrack) -> (String, bool) {
    let codec_id = track.properties.codec_id.as_deref().unwrap_or("");
    let combined = codec_id.to_lowercase();
    let is_flac = combined.contains("flac");

    let channel_label = match track.properties.audio_channels {
        Some(8) => Some("7.1".to_string()),
        Some(6) => Some("5.1".to_string()),
        Some(2) => Some("2.0".to_string()),
        Some(1) => Some("1.0".to_string()),
        Some(ch) if ch <= 2 => Some(format!("{ch}.0")),
        Some(ch) => Some(format!("{}.1", ch - 1)),
        None => None,
    };

    let audio_name = if is_flac {
        "FLAC".to_string()
    } else if combined.contains("aac") {
        "AAC".to_string()
    } else if combined.contains("opus") {
        "Opus".to_string()
    } else if combined.contains("ac3") || combined.contains("ac-3") {
        "AC3".to_string()
    } else if !codec_id.is_empty() {
        codec_id.to_string()
    } else {
        "Audio".to_string()
    };

    let full_name = match channel_label {
        Some(label) => format!("{audio_name} {label}"),
        None => audio_name,
    };

    (full_name, is_flac)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InspectionMetadata {
    pub resolution: Option<String>,
    pub video_codec: Option<String>,
    pub video_tag: Option<String>,
    pub audio_summary: Option<String>,
    pub has_flac: Option<bool>,
    pub sub_summary: Option<String>,
    pub fonts_count: Option<usize>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestItem {
    pub id: String,
    pub name: String,
    pub release: String,
    pub metadata: InspectionMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_error: Option<String>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectionManifest {
    pub version: u64,
    #[serde(default)]
    pub subtitle_candidates: BTreeMap<String, Vec<String>>,
    pub items: Vec<ManifestItem>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl InspectionManifest {
    /// Filters and reorders items strictly matching the provided `selected_items`.
    ///
    /// Items can be matched by raw `item.id` or by its cleaned relative path representation.
    /// Deduplicates items if multiple selectors resolve to the same item ID.
    /// Prunes `subtitle_candidates` to only retain releases represented in the retained items.
    pub fn filter_selected(&self, selected_items: &[String]) -> InspectionManifest {
        let mut item_map: HashMap<String, &ManifestItem> =
            HashMap::with_capacity(self.items.len() * 2);
        for item in &self.items {
            let clean = crate::request::clean_relative_path(&item.id);
            item_map.insert(clean, item);
            item_map.insert(item.id.clone(), item);
        }

        let mut filtered_items = Vec::with_capacity(selected_items.len());
        let mut seen = HashSet::new();

        for selected in selected_items {
            let clean_selected = crate::request::clean_relative_path(selected);
            if let Some(&item) = item_map
                .get(clean_selected.as_str())
                .or_else(|| item_map.get(selected.as_str()))
            {
                if seen.insert(item.id.as_str()) {
                    filtered_items.push(item.clone());
                }
            }
        }

        let active_releases: HashSet<&str> = filtered_items
            .iter()
            .map(|item| item.release.as_str())
            .collect();

        let filtered_subtitles: BTreeMap<String, Vec<String>> = self
            .subtitle_candidates
            .iter()
            .filter(|(rel, _)| active_releases.contains(rel.as_str()))
            .map(|(rel, subs)| (rel.clone(), subs.clone()))
            .collect();

        InspectionManifest {
            version: self.version,
            subtitle_candidates: filtered_subtitles,
            items: filtered_items,
            extra: self.extra.clone(),
        }
    }
}

/// Extracts summarized video inspection metadata from an `InspectedVideo`.
pub fn extract_mkv_metadata(inspected: Option<&InspectedVideo>) -> InspectionMetadata {
    let Some(inspected) = inspected else {
        return InspectionMetadata::default();
    };

    let mut resolution: Option<String> = None;
    let mut video_codec: Option<String> = None;
    let mut audio_parts: Vec<String> = Vec::new();
    let mut has_flac = false;
    let mut has_audio = false;
    let mut subtitle_languages: Vec<String> = Vec::new();
    let mut subtitle_formats: Vec<String> = Vec::new();

    for track in inspected.tracks() {
        if track.track_type == "video" {
            let codec = track.codec.as_deref().unwrap_or("");
            let cid = track.properties.codec_id.as_deref().unwrap_or("");
            video_codec = parse_video_codec(codec, cid);
            resolution = track
                .properties
                .pixel_dimensions
                .as_deref()
                .and_then(parse_resolution);
        } else if track.track_type == "audio" {
            has_audio = true;
            let (audio, flac_found) = parse_audio_track_info(track);
            if flac_found {
                has_flac = true;
            }
            audio_parts.push(audio);
        } else if track.track_type == "subtitles" {
            let codec_id = track.properties.codec_id.as_deref().unwrap_or("");
            let combined = codec_id.to_lowercase();
            let format_name = if combined.contains("ass") || combined.contains("substation") {
                "ASS"
            } else if combined.contains("srt")
                || combined.contains("subrip")
                || combined.contains("utf8")
            {
                "SRT"
            } else if combined.contains("pgs") || combined.contains("hdmv") {
                "PGS"
            } else if combined.contains("vob") {
                "VobSub"
            } else {
                "Subtitles"
            };

            if !subtitle_formats.iter().any(|f| f == format_name) {
                subtitle_formats.push(format_name.to_string());
            }

            let lang = track
                .properties
                .language
                .as_deref()
                .unwrap_or("und")
                .to_uppercase();
            let lang = if lang.is_empty() {
                "UND".to_string()
            } else {
                lang
            };
            if !subtitle_languages.contains(&lang) {
                subtitle_languages.push(lang);
            }
        }
    }

    let video_tag = match (&resolution, &video_codec) {
        (Some(res), Some(codec)) => Some(format!("{res} {codec}")),
        _ => None,
    };

    let audio_summary = if !audio_parts.is_empty() {
        Some(audio_parts.join(" / "))
    } else if has_audio {
        Some("No Audio".to_string())
    } else {
        None
    };

    let sub_summary = if !subtitle_languages.is_empty() {
        let fmt = if subtitle_formats.len() == 1 {
            &subtitle_formats[0]
        } else {
            "Subs"
        };
        Some(format!("{fmt} ({})", subtitle_languages.join(", ")))
    } else if !subtitle_formats.is_empty() {
        let fmt = if subtitle_formats.len() == 1 {
            &subtitle_formats[0]
        } else {
            "Subtitles"
        };
        Some(fmt.to_string())
    } else {
        None
    };

    InspectionMetadata {
        resolution,
        video_codec,
        video_tag,
        audio_summary,
        has_flac: if has_audio { Some(has_flac) } else { None },
        sub_summary,
        fonts_count: inspected.fonts_count(),
        extra: Default::default(),
    }
}

/// Generates a complete preflight inspection manifest for an anime batch directory.
pub async fn generate_inspection_manifest(
    input_dir: &Path,
    persistent_cache: Option<&PersistentInspectCache>,
    cancel_token: Option<&tokio_util::sync::CancellationToken>,
) -> Result<InspectionManifest, String> {
    if let Some(token) = cancel_token {
        if token.is_cancelled() {
            if let Some(cache) = persistent_cache {
                cache.save_shared();
            }
            return Err("Inspection cancelled".into());
        }
    }

    let all_mkvs = scan_mkv_files(input_dir, true, None);
    if all_mkvs.is_empty() {
        return Ok(InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates: BTreeMap::new(),
            items: Vec::new(),
            extra: Default::default(),
        });
    }

    let mut videos_by_dir: BTreeMap<PathBuf, Vec<&crate::scan::ParsedFile>> = BTreeMap::new();
    for video in &all_mkvs {
        if let Some(parent) = video.path.parent() {
            videos_by_dir
                .entry(parent.to_path_buf())
                .or_default()
                .push(video);
        }
    }

    let mut scan_cache = HashMap::<PathBuf, ScanResults>::new();
    let mut release_by_dir = HashMap::<PathBuf, String>::new();
    let mut subtitle_candidates = BTreeMap::<String, Vec<String>>::new();

    // Derive every shared subtitle exactly once. Ancestor chains overlap heavily across
    // releases, so building one index per release re-runs per-file language/dialogue
    // detection and re-sorts the ancestor candidate set for every release directory.
    let mut all_ancestor_dirs: HashSet<PathBuf> = HashSet::new();
    for mkv_dir in videos_by_dir.keys() {
        for ancestor in ancestor_dirs(mkv_dir, input_dir) {
            all_ancestor_dirs.insert(ancestor);
        }
    }
    let mut subs_by_dir: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for directory in &all_ancestor_dirs {
        let assets = scan_assets(directory, Some(&mut scan_cache));
        let ids: Vec<String> = assets
            .subs
            .iter()
            .map(|s| get_item_id(&s.path, input_dir))
            .collect();
        if !ids.is_empty() {
            subs_by_dir.insert(directory.clone(), ids);
        }
    }

    for mkv_dir in videos_by_dir.keys() {
        let release = get_item_id(mkv_dir, input_dir);
        release_by_dir.insert(mkv_dir.clone(), release.clone());

        let mut candidates: Vec<String> = Vec::new();
        for directory in ancestor_dirs(mkv_dir, input_dir) {
            if let Some(dir_subs) = subs_by_dir.get(&directory) {
                candidates.extend(dir_subs.iter().cloned());
            }
        }
        candidates.sort_by_cached_key(|a| crate::domain::natural_sort_key(a));

        if !candidates.is_empty() {
            subtitle_candidates.insert(release, candidates);
        }
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    let probe_limit = (workers * 2).clamp(2, 16);
    let probe_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(probe_limit));
    let mut probe_set = tokio::task::JoinSet::new();

    let mut probe_results: HashMap<PathBuf, Result<crate::scan::MkvmergeMetadata, String>> =
        HashMap::new();

    for mkv in &all_mkvs {
        if let Some(cached) = persistent_cache.and_then(|c| c.get_path(&mkv.path)) {
            probe_results.insert(mkv.path.clone(), Ok(cached));
        }
    }

    if let Some(token) = cancel_token {
        if token.is_cancelled() {
            if let Some(cache) = persistent_cache {
                cache.save_shared();
            }
            return Err("Inspection cancelled".into());
        }
    }

    for mkv in &all_mkvs {
        if probe_results.contains_key(&mkv.path) {
            continue;
        }
        let path = mkv.path.clone();
        let sem = std::sync::Arc::clone(&probe_sem);
        probe_set.spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return (path, Err("Probe semaphore closed".into()));
            };
            let res = crate::scan::probe_video(&path).await;
            (path, res)
        });
    }

    while !probe_set.is_empty() {
        let next_res = if let Some(token) = cancel_token {
            tokio::select! {
                _ = token.cancelled() => {
                    probe_set.abort_all();
                    if let Some(cache) = persistent_cache {
                        cache.save_shared();
                    }
                    return Err("Inspection cancelled".into());
                }
                next = probe_set.join_next() => next,
            }
        } else {
            probe_set.join_next().await
        };

        match next_res {
            Some(Ok((path, probe_res))) => {
                if let Ok(meta) = &probe_res {
                    if let Some(cache) = persistent_cache {
                        cache.put_path(&path, meta.clone());
                    }
                }
                probe_results.insert(path, probe_res);
            }
            Some(Err(join_err)) => {
                tracing::warn!("Preflight probe task failed: {join_err}");
            }
            None => break,
        }
    }

    let mut items = Vec::with_capacity(all_mkvs.len());
    for mkv in &all_mkvs {
        let probe_res = probe_results
            .remove(&mkv.path)
            .unwrap_or_else(|| Err("Probe result missing".into()));

        let (inspected, inspection_error) = match probe_res {
            Ok(probed) => (Some(InspectedVideo::new(probed)), None),
            Err(err) => (None, Some(err)),
        };

        let parent = mkv.path.parent().unwrap_or(input_dir);
        let release = release_by_dir
            .get(parent)
            .cloned()
            .unwrap_or_else(|| get_item_id(parent, input_dir));

        let item = ManifestItem {
            id: get_item_id(&mkv.path, input_dir),
            name: mkv.name.clone(),
            release,
            metadata: extract_mkv_metadata(inspected.as_ref()),
            inspection_error,
            extra: Default::default(),
        };
        items.push(item);
    }

    if let Some(cache) = persistent_cache {
        cache.save();
    }

    Ok(InspectionManifest {
        version: MANIFEST_VERSION,
        subtitle_candidates,
        items,
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TrackProperties;
    use crate::scan::{MkvAttachment, MkvmergeMetadata};

    #[test]
    fn test_get_item_id() {
        let root = Path::new("/anime/series");
        assert_eq!(get_item_id(Path::new("/anime/series"), root), ".");
        assert_eq!(
            get_item_id(Path::new("/anime/series/ep01.mkv"), root),
            "ep01.mkv"
        );
        assert_eq!(
            get_item_id(Path::new("/anime/series/Season 1/ep01.mkv"), root),
            "Season 1/ep01.mkv"
        );
    }

    #[test]
    fn test_parse_resolution() {
        assert_eq!(parse_resolution("3840x2160"), Some("4K".to_string()));
        assert_eq!(parse_resolution("2560x1440"), Some("1440p".to_string()));
        assert_eq!(parse_resolution("1920x1080"), Some("1080p".to_string()));
        assert_eq!(parse_resolution("1280x720"), Some("720p".to_string()));
        assert_eq!(parse_resolution("720x480"), Some("480p".to_string()));
        assert_eq!(parse_resolution(""), None);
        assert_eq!(parse_resolution("1080"), Some("1080p".to_string()));
        assert_eq!(parse_resolution("invalid"), None);
    }

    #[test]
    fn test_parse_video_codec() {
        assert_eq!(
            parse_video_codec("", "V_MPEG4/ISO/AVC"),
            Some("AVC".to_string())
        );
        assert_eq!(parse_video_codec("h.264", ""), Some("AVC".to_string()));
        assert_eq!(
            parse_video_codec("", "V_MPEGH/ISO/HEVC"),
            Some("HEVC".to_string())
        );
        assert_eq!(parse_video_codec("h.265", ""), Some("HEVC".to_string()));
        assert_eq!(parse_video_codec("av1", ""), Some("AV1".to_string()));
        assert_eq!(parse_video_codec("vp9", ""), Some("VP9".to_string()));
        assert_eq!(parse_video_codec("custom", ""), Some("custom".to_string()));
        assert_eq!(parse_video_codec("", ""), None);
    }

    #[test]
    fn test_parse_audio_track_info() {
        let flac_track = MkvTrack {
            id: Some(1),
            track_type: "audio".to_string(),
            codec: None,
            properties: TrackProperties {
                codec_id: Some("A_FLAC".to_string()),
                audio_channels: Some(2),
                ..Default::default()
            },
        };
        assert_eq!(
            parse_audio_track_info(&flac_track),
            ("FLAC 2.0".to_string(), true)
        );

        let aac_track = MkvTrack {
            id: Some(2),
            track_type: "audio".to_string(),
            codec: None,
            properties: TrackProperties {
                codec_id: Some("A_AAC".to_string()),
                audio_channels: Some(6),
                ..Default::default()
            },
        };
        assert_eq!(
            parse_audio_track_info(&aac_track),
            ("AAC 5.1".to_string(), false)
        );

        let opus_track = MkvTrack {
            id: Some(3),
            track_type: "audio".to_string(),
            codec: None,
            properties: TrackProperties {
                codec_id: Some("A_OPUS".to_string()),
                audio_channels: Some(8),
                ..Default::default()
            },
        };
        assert_eq!(
            parse_audio_track_info(&opus_track),
            ("Opus 7.1".to_string(), false)
        );

        let ac3_track = MkvTrack {
            id: Some(4),
            track_type: "audio".to_string(),
            codec: None,
            properties: TrackProperties {
                codec_id: Some("A_AC3".to_string()),
                audio_channels: Some(1),
                ..Default::default()
            },
        };
        assert_eq!(
            parse_audio_track_info(&ac3_track),
            ("AC3 1.0".to_string(), false)
        );
    }

    #[test]
    fn test_extract_mkv_metadata() {
        assert_eq!(extract_mkv_metadata(None), InspectionMetadata::default());

        let video = InspectedVideo::new(MkvmergeMetadata {
            tracks: vec![
                MkvTrack {
                    id: Some(0),
                    track_type: "video".to_string(),
                    codec: None,
                    properties: TrackProperties {
                        codec_id: Some("V_MPEGH/ISO/HEVC".to_string()),
                        pixel_dimensions: Some("1920x1080".to_string()),
                        ..Default::default()
                    },
                },
                MkvTrack {
                    id: Some(1),
                    track_type: "audio".to_string(),
                    codec: None,
                    properties: TrackProperties {
                        codec_id: Some("A_FLAC".to_string()),
                        audio_channels: Some(2),
                        ..Default::default()
                    },
                },
                MkvTrack {
                    id: Some(2),
                    track_type: "subtitles".to_string(),
                    codec: None,
                    properties: TrackProperties {
                        codec_id: Some("S_TEXT/ASS".to_string()),
                        language: Some("eng".to_string()),
                        ..Default::default()
                    },
                },
            ],
            attachments: vec![MkvAttachment {
                id: Some(1),
                file_name: Some("font.ttf".to_string()),
                content_type: Some("application/x-truetype-font".to_string()),
                size: Some(1024),
            }],
            chapters: vec![],
        });

        let meta = extract_mkv_metadata(Some(&video));
        assert_eq!(meta.resolution.as_deref(), Some("1080p"));
        assert_eq!(meta.video_codec.as_deref(), Some("HEVC"));
        assert_eq!(meta.video_tag.as_deref(), Some("1080p HEVC"));
        assert_eq!(meta.audio_summary.as_deref(), Some("FLAC 2.0"));
        assert_eq!(meta.has_flac, Some(true));
        assert_eq!(meta.sub_summary.as_deref(), Some("ASS (ENG)"));
        assert_eq!(meta.fonts_count, Some(1));
    }

    #[test]
    fn test_additive_field_preservation() {
        let json = r#"{
            "version": 2,
            "subtitle_candidates": {},
            "items": [
                {
                    "id": "show_01.mkv",
                    "name": "Show - 01",
                    "release": ".",
                    "metadata": {
                        "custom_metric": 42
                    },
                    "custom_flag": true
                }
            ],
            "custom_root_field": "preserved"
        }"#;

        let manifest: InspectionManifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            manifest.extra.get("custom_root_field").unwrap(),
            "preserved"
        );
        assert_eq!(manifest.items[0].extra.get("custom_flag").unwrap(), true);
        assert_eq!(
            manifest.items[0]
                .metadata
                .extra
                .get("custom_metric")
                .unwrap(),
            42
        );

        let reserialized = serde_json::to_string(&manifest).unwrap();
        assert!(reserialized.contains("custom_root_field"));
        assert!(reserialized.contains("custom_flag"));
        assert!(reserialized.contains("custom_metric"));
    }

    #[tokio::test]
    async fn test_generate_inspection_manifest_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        let res = generate_inspection_manifest(temp_dir.path(), None, None)
            .await
            .unwrap();
        assert_eq!(res.items.len(), 0);
    }

    #[tokio::test]
    async fn test_generate_inspection_manifest_cancelled() {
        let temp_dir = tempfile::tempdir().unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let res = generate_inspection_manifest(temp_dir.path(), None, Some(&token)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "Inspection cancelled");
    }

    #[tokio::test]
    async fn test_generate_inspection_manifest_cancelled_in_loop() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("ep01.mkv"), b"dummy content").unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let res = generate_inspection_manifest(temp_dir.path(), None, Some(&token)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "Inspection cancelled");
    }

    fn make_test_item(id: &str, name: &str, release: &str) -> ManifestItem {
        ManifestItem {
            id: id.to_string(),
            name: name.to_string(),
            release: release.to_string(),
            metadata: InspectionMetadata::default(),
            inspection_error: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_filter_selected_reorders_and_filters_items() {
        let mut subtitle_candidates = BTreeMap::new();
        subtitle_candidates.insert("Show".to_string(), vec!["sub.ass".to_string()]);

        let items = vec![
            make_test_item("ep01.mkv", "Episode 1", "Show"),
            make_test_item("ep02.mkv", "Episode 2", "Show"),
            make_test_item("ep03.mkv", "Episode 3", "Show"),
        ];

        let mut extra = serde_json::Map::new();
        extra.insert("custom_key".to_string(), serde_json::Value::Bool(true));

        let manifest = InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates,
            items,
            extra,
        };

        // Select ep03 and ep01 in reverse order, using cleaned path for ep03
        let selected = vec!["./ep03.mkv".to_string(), "ep01.mkv".to_string()];
        let filtered = manifest.filter_selected(&selected);

        assert_eq!(filtered.items.len(), 2);
        assert_eq!(filtered.items[0].id, "ep03.mkv");
        assert_eq!(filtered.items[1].id, "ep01.mkv");
        assert_eq!(filtered.version, MANIFEST_VERSION);
        assert_eq!(
            filtered.subtitle_candidates.get("Show").unwrap(),
            &vec!["sub.ass".to_string()]
        );
        assert_eq!(
            filtered.extra.get("custom_key"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn test_filter_selected_prunes_subtitle_candidates() {
        let mut subtitle_candidates = BTreeMap::new();
        subtitle_candidates.insert("Season 1".to_string(), vec!["s1.ass".to_string()]);
        subtitle_candidates.insert("Season 2".to_string(), vec!["s2.ass".to_string()]);

        let items = vec![
            make_test_item("season1/ep01.mkv", "S1E01", "Season 1"),
            make_test_item("season2/ep01.mkv", "S2E01", "Season 2"),
        ];

        let manifest = InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates,
            items,
            extra: Default::default(),
        };

        // Select only item from Season 2
        let selected = vec!["season2/ep01.mkv".to_string()];
        let filtered = manifest.filter_selected(&selected);

        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].id, "season2/ep01.mkv");
        assert!(!filtered.subtitle_candidates.contains_key("Season 1"));
        assert_eq!(
            filtered.subtitle_candidates.get("Season 2").unwrap(),
            &vec!["s2.ass".to_string()]
        );
    }

    #[test]
    fn test_filter_selected_deduplicates_duplicate_keys() {
        let items = vec![
            make_test_item("ep01.mkv", "Episode 1", "Show"),
            make_test_item("ep02.mkv", "Episode 2", "Show"),
        ];

        let manifest = InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates: BTreeMap::new(),
            items,
            extra: Default::default(),
        };

        // Select ep01 multiple times (both exact and with relative path prefix), and ep02
        let selected = vec![
            "ep01.mkv".to_string(),
            "./ep01.mkv".to_string(),
            "ep01.mkv".to_string(),
            "ep02.mkv".to_string(),
            "ep01.mkv".to_string(),
        ];
        let filtered = manifest.filter_selected(&selected);

        assert_eq!(filtered.items.len(), 2);
        assert_eq!(filtered.items[0].id, "ep01.mkv");
        assert_eq!(filtered.items[1].id, "ep02.mkv");
    }

    #[tokio::test]
    async fn test_subtitle_candidates_natural_sort_ordering() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input_dir = temp_dir.path();
        std::fs::write(input_dir.join("ep1.mkv"), b"video").unwrap();
        std::fs::write(input_dir.join("ep1.ass"), b"sub").unwrap();
        std::fs::write(input_dir.join("ep2.ass"), b"sub").unwrap();
        std::fs::write(input_dir.join("ep10.ass"), b"sub").unwrap();

        let manifest = generate_inspection_manifest(input_dir, None, None)
            .await
            .unwrap();

        let subs = manifest.subtitle_candidates.get(".").unwrap();
        // Natural sort order expects ep1.ass, ep2.ass, ep10.ass
        // Ascii sort order would produce ep1.ass, ep10.ass, ep2.ass
        assert_eq!(
            subs,
            &vec![
                "ep1.ass".to_string(),
                "ep2.ass".to_string(),
                "ep10.ass".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn test_generate_inspection_manifest_concurrent_probe() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input_dir = temp_dir.path();
        for i in 1..=5 {
            std::fs::write(input_dir.join(format!("ep{:02}.mkv", i)), b"video content").unwrap();
        }

        let cache = PersistentInspectCache::default();
        cache.put_path(&input_dir.join("ep01.mkv"), MkvmergeMetadata::default());

        let manifest = generate_inspection_manifest(input_dir, Some(&cache), None)
            .await
            .unwrap();

        assert_eq!(manifest.items.len(), 5);
        assert!(manifest.items[0].inspection_error.is_none());
        for (idx, item) in manifest.items.iter().enumerate() {
            assert_eq!(item.id, format!("ep{:02}.mkv", idx + 1));
        }

        // Second pass should hit cache for all items where cache was populated and maintain ordering
        let manifest2 = generate_inspection_manifest(input_dir, Some(&cache), None)
            .await
            .unwrap();
        assert_eq!(manifest2.items.len(), 5);
        for (idx, item) in manifest2.items.iter().enumerate() {
            assert_eq!(item.id, format!("ep{:02}.mkv", idx + 1));
        }
    }
}
