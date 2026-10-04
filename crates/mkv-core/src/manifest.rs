//! Preflight inspection manifest: per-video metadata summaries and subtitle candidates.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::domain::{ancestor_dirs, natural_sort_key, MkvTrack};
use crate::request::{clean_relative_path, resolve_selected_items};
use crate::scan::{
    get_or_probe_video, scan_assets, scan_mkv_files, InspectedVideo, MkvmergeMetadata, ParsedFile,
    PersistentInspectCache, ScanCache,
};

pub const MANIFEST_VERSION: u64 = 2;

const CANCELLED_ERROR: &str = "Inspection cancelled";

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

/// Buckets a `pixel_dimensions` string (e.g. "1920x1080") into a resolution label.
fn parse_resolution(pixel_dimensions: &str) -> Option<&'static str> {
    if let Some((w_str, h_str)) = pixel_dimensions.split_once('x') {
        if let (Ok(width), Ok(height)) = (w_str.trim().parse::<u32>(), h_str.trim().parse::<u32>())
        {
            if width >= 3800 || height >= 2100 {
                return Some("4K");
            }
            if width >= 2500 || height >= 1400 {
                return Some("1440p");
            }
            if width >= 1900 || height >= 1000 {
                return Some("1080p");
            }
            if width >= 1200 || height >= 700 {
                return Some("720p");
            }
            if height >= 480 {
                return Some("480p");
            }
        }
    }

    if pixel_dimensions.contains("3840") || pixel_dimensions.contains("2160") {
        return Some("4K");
    }
    if pixel_dimensions.contains("2560") || pixel_dimensions.contains("1440") {
        return Some("1440p");
    }
    if pixel_dimensions.contains("1920") || pixel_dimensions.contains("1080") {
        return Some("1080p");
    }
    if pixel_dimensions.contains("1280") || pixel_dimensions.contains("720") {
        return Some("720p");
    }

    None
}

/// Normalizes video codec strings into standard identifiers (HEVC, AVC, AV1, VP9).
fn parse_video_codec(codec: &str, codec_id: &str) -> Option<String> {
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

/// Audio track label (codec plus channel layout, e.g. "AAC 5.1") and whether it is FLAC.
fn parse_audio_track_info(track: &MkvTrack) -> (String, bool) {
    let codec_id = track.properties.codec_id.as_deref().unwrap_or("");
    let codec_lower = codec_id.to_lowercase();
    let is_flac = codec_lower.contains("flac");

    let channel_label = track.properties.audio_channels.map(|ch| {
        if ch <= 2 {
            format!("{ch}.0")
        } else {
            format!("{}.1", ch - 1)
        }
    });

    let audio_name = if is_flac {
        "FLAC"
    } else if codec_lower.contains("aac") {
        "AAC"
    } else if codec_lower.contains("opus") {
        "Opus"
    } else if codec_lower.contains("ac3") || codec_lower.contains("ac-3") {
        "AC3"
    } else if !codec_id.is_empty() {
        codec_id
    } else {
        "Audio"
    };

    let full_name = match channel_label {
        Some(label) => format!("{audio_name} {label}"),
        None => audio_name.to_string(),
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
    /// Narrows items to `selected_items` in selection order, dropping repeated selections,
    /// and prunes `subtitle_candidates` to the releases of the retained items. Every
    /// selection must name a discovered item.
    pub fn select_items(&self, selected_items: &[String]) -> Result<InspectionManifest, String> {
        let index_by_id: HashMap<String, usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(idx, item)| (clean_relative_path(&item.id), idx))
            .collect();
        let filtered_items: Vec<ManifestItem> =
            resolve_selected_items(&index_by_id, selected_items)?
                .into_iter()
                .map(|idx| self.items[idx].clone())
                .collect();

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

        Ok(InspectionManifest {
            version: self.version,
            subtitle_candidates: filtered_subtitles,
            items: filtered_items,
            extra: self.extra.clone(),
        })
    }
}

/// Summarizes an inspected video's tracks and attachments for the manifest.
fn extract_mkv_metadata(inspected: &InspectedVideo) -> InspectionMetadata {
    let mut resolution: Option<String> = None;
    let mut video_codec: Option<String> = None;
    let mut audio_parts: Vec<String> = Vec::new();
    let mut has_flac = false;
    let mut subtitle_languages: Vec<String> = Vec::new();
    let mut subtitle_formats: Vec<&str> = Vec::new();

    for track in inspected.tracks() {
        if track.track_type == "video" {
            let codec = track.codec.as_deref().unwrap_or("");
            let cid = track.properties.codec_id.as_deref().unwrap_or("");
            video_codec = parse_video_codec(codec, cid);
            resolution = track
                .properties
                .pixel_dimensions
                .as_deref()
                .and_then(parse_resolution)
                .map(str::to_string);
        } else if track.track_type == "audio" {
            let (audio, is_flac) = parse_audio_track_info(track);
            has_flac |= is_flac;
            audio_parts.push(audio);
        } else if track.track_type == "subtitles" {
            let codec_lower = track
                .properties
                .codec_id
                .as_deref()
                .unwrap_or("")
                .to_lowercase();
            let format_name = if codec_lower.contains("ass") || codec_lower.contains("substation") {
                "ASS"
            } else if codec_lower.contains("srt")
                || codec_lower.contains("subrip")
                || codec_lower.contains("utf8")
            {
                "SRT"
            } else if codec_lower.contains("pgs") || codec_lower.contains("hdmv") {
                "PGS"
            } else if codec_lower.contains("vob") {
                "VobSub"
            } else {
                "Subtitles"
            };

            if !subtitle_formats.contains(&format_name) {
                subtitle_formats.push(format_name);
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

    let has_audio = !audio_parts.is_empty();
    let audio_summary = has_audio.then(|| audio_parts.join(" / "));

    // Every subtitle track contributes both a format and a language.
    let sub_summary = (!subtitle_languages.is_empty()).then(|| {
        let fmt = match subtitle_formats.as_slice() {
            [only] => *only,
            _ => "Subs",
        };
        format!("{fmt} ({})", subtitle_languages.join(", "))
    });

    InspectionMetadata {
        resolution,
        video_codec,
        video_tag,
        audio_summary,
        has_flac: has_audio.then_some(has_flac),
        sub_summary,
        fonts_count: Some(inspected.font_attachments().len()),
        extra: Default::default(),
    }
}

fn is_cancelled(token: Option<&CancellationToken>) -> bool {
    token.is_some_and(|t| t.is_cancelled())
}

/// Flushes pending cache writes and returns the cancellation error.
fn cancelled<T>(cache: Option<&PersistentInspectCache>) -> Result<T, String> {
    if let Some(cache) = cache {
        cache.save_shared();
    }
    Err(CANCELLED_ERROR.to_string())
}

/// Lists the subtitle IDs of each directory that has any. The scan is synchronous file I/O with
/// no `.await`, so `cancel_token` is checked before each directory; returns `None` once it is
/// cancelled.
fn list_subtitles_by_dir(
    directories: &HashSet<PathBuf>,
    input_dir: &Path,
    cancel_token: Option<&CancellationToken>,
) -> Option<HashMap<PathBuf, Vec<String>>> {
    let mut scan_cache = ScanCache::default();
    let mut subs_by_dir = HashMap::new();
    for directory in directories {
        if is_cancelled(cancel_token) {
            return None;
        }
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
    Some(subs_by_dir)
}

/// Prunes the entries of files no longer under `input_dir` and saves the cache. Only a
/// completed inspection may call this: `discovered` must be the scan's full file list.
fn prune_and_save_cache(
    cache: Option<&PersistentInspectCache>,
    input_dir: &Path,
    discovered: &[ParsedFile],
) {
    if let Some(cache) = cache {
        cache.prune_undiscovered(input_dir, discovered.iter().map(|f| f.path.as_path()));
        cache.save();
    }
}

/// Generates a complete preflight inspection manifest for an anime batch directory.
///
/// After a completed inspection it also prunes `persistent_cache` of the entries under
/// `input_dir` whose file was not discovered, then saves it; a cancelled inspection prunes nothing.
pub async fn generate_inspection_manifest(
    input_dir: &Path,
    persistent_cache: Option<Arc<PersistentInspectCache>>,
    cancel_token: Option<&CancellationToken>,
) -> Result<InspectionManifest, String> {
    if is_cancelled(cancel_token) {
        return cancelled(persistent_cache.as_deref());
    }

    let all_mkvs = scan_mkv_files(input_dir, true, None);
    if all_mkvs.is_empty() {
        prune_and_save_cache(persistent_cache.as_deref(), input_dir, &all_mkvs);
        return Ok(InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates: BTreeMap::new(),
            items: Vec::new(),
            extra: Default::default(),
        });
    }

    let release_dirs: BTreeSet<PathBuf> = all_mkvs
        .iter()
        .filter_map(|video| video.path.parent().map(Path::to_path_buf))
        .collect();

    let mut release_by_dir = HashMap::<PathBuf, String>::new();
    let mut subtitle_candidates = BTreeMap::<String, Vec<String>>::new();

    // Ancestor chains overlap across releases, so each directory's subtitles are listed once.
    let all_ancestor_dirs: HashSet<PathBuf> = release_dirs
        .iter()
        .flat_map(|mkv_dir| ancestor_dirs(mkv_dir, input_dir))
        .collect();
    let Some(subs_by_dir) = list_subtitles_by_dir(&all_ancestor_dirs, input_dir, cancel_token)
    else {
        return cancelled(persistent_cache.as_deref());
    };

    for mkv_dir in &release_dirs {
        let release = get_item_id(mkv_dir, input_dir);
        release_by_dir.insert(mkv_dir.clone(), release.clone());

        let mut candidates: Vec<String> = Vec::new();
        for directory in ancestor_dirs(mkv_dir, input_dir) {
            if let Some(dir_subs) = subs_by_dir.get(&directory) {
                candidates.extend(dir_subs.iter().cloned());
            }
        }
        candidates.sort_by_cached_key(|a| natural_sort_key(a));

        if !candidates.is_empty() {
            subtitle_candidates.insert(release, candidates);
        }
    }

    let probe_limit = crate::cpu::probe_concurrency(crate::cpu::default_workers());
    let probe_sem = Arc::new(tokio::sync::Semaphore::new(probe_limit));
    let mut probe_set = tokio::task::JoinSet::new();

    let mut probe_results: HashMap<PathBuf, Result<MkvmergeMetadata, String>> = HashMap::new();

    if is_cancelled(cancel_token) {
        return cancelled(persistent_cache.as_deref());
    }

    // The cache lookup and store run inside each probe task, so they overlap with probing
    // and are aborted with it on cancel.
    for mkv in &all_mkvs {
        let path = mkv.path.clone();
        let cache = persistent_cache.clone();
        let sem = Arc::clone(&probe_sem);
        probe_set.spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return (path, Err("Probe semaphore closed".into()));
            };
            let res = get_or_probe_video(&path, cache.as_deref()).await;
            (path, res)
        });
    }

    while !probe_set.is_empty() {
        let next_res = if let Some(token) = cancel_token {
            tokio::select! {
                _ = token.cancelled() => {
                    probe_set.abort_all();
                    return cancelled(persistent_cache.as_deref());
                }
                next = probe_set.join_next() => next,
            }
        } else {
            probe_set.join_next().await
        };

        match next_res {
            Some(Ok((path, probe_res))) => {
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

        let (metadata, inspection_error) = match probe_res {
            Ok(probed) => (extract_mkv_metadata(&InspectedVideo::new(probed)), None),
            Err(err) => (InspectionMetadata::default(), Some(err)),
        };

        let parent = mkv.path.parent().unwrap_or(input_dir);
        let release = release_by_dir
            .get(parent)
            .cloned()
            .unwrap_or_else(|| get_item_id(parent, input_dir));

        items.push(ManifestItem {
            id: get_item_id(&mkv.path, input_dir),
            name: mkv.name.clone(),
            release,
            metadata,
            inspection_error,
            extra: Default::default(),
        });
    }

    prune_and_save_cache(persistent_cache.as_deref(), input_dir, &all_mkvs);

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
    use crate::scan::MkvAttachment;

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
        assert_eq!(parse_resolution("3840x2160"), Some("4K"));
        assert_eq!(parse_resolution("2560x1440"), Some("1440p"));
        assert_eq!(parse_resolution("1920x1080"), Some("1080p"));
        assert_eq!(parse_resolution("1280x720"), Some("720p"));
        assert_eq!(parse_resolution("720x480"), Some("480p"));
        assert_eq!(parse_resolution(""), None);
        assert_eq!(parse_resolution("1080"), Some("1080p"));
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

    fn track(track_type: &str, properties: TrackProperties) -> MkvTrack {
        MkvTrack {
            id: None,
            track_type: track_type.to_string(),
            codec: None,
            properties,
        }
    }

    fn audio_track(codec_id: &str, channels: Option<u32>) -> MkvTrack {
        track(
            "audio",
            TrackProperties {
                codec_id: Some(codec_id.to_string()),
                audio_channels: channels,
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_parse_audio_track_info() {
        for (codec_id, channels, expected, is_flac) in [
            ("A_FLAC", Some(2), "FLAC 2.0", true),
            ("A_AAC", Some(6), "AAC 5.1", false),
            ("A_OPUS", Some(8), "Opus 7.1", false),
            ("A_AC3", Some(1), "AC3 1.0", false),
            ("A_DTS", Some(3), "A_DTS 2.1", false),
            ("", None, "Audio", false),
        ] {
            assert_eq!(
                parse_audio_track_info(&audio_track(codec_id, channels)),
                (expected.to_string(), is_flac),
                "{codec_id} / {channels:?}"
            );
        }
    }

    #[test]
    fn test_extract_mkv_metadata() {
        let video = InspectedVideo::new(MkvmergeMetadata {
            tracks: vec![
                track(
                    "video",
                    TrackProperties {
                        codec_id: Some("V_MPEGH/ISO/HEVC".to_string()),
                        pixel_dimensions: Some("1920x1080".to_string()),
                        ..Default::default()
                    },
                ),
                audio_track("A_FLAC", Some(2)),
                track(
                    "subtitles",
                    TrackProperties {
                        codec_id: Some("S_TEXT/ASS".to_string()),
                        language: Some("eng".to_string()),
                        ..Default::default()
                    },
                ),
            ],
            attachments: vec![MkvAttachment {
                id: Some(1),
                file_name: Some("font.ttf".to_string()),
                content_type: Some("application/x-truetype-font".to_string()),
                size: Some(1024),
            }],
            chapters: vec![],
        });

        let meta = extract_mkv_metadata(&video);
        assert_eq!(meta.resolution.as_deref(), Some("1080p"));
        assert_eq!(meta.video_codec.as_deref(), Some("HEVC"));
        assert_eq!(meta.video_tag.as_deref(), Some("1080p HEVC"));
        assert_eq!(meta.audio_summary.as_deref(), Some("FLAC 2.0"));
        assert_eq!(meta.has_flac, Some(true));
        assert_eq!(meta.sub_summary.as_deref(), Some("ASS (ENG)"));
        assert_eq!(meta.fonts_count, Some(1));
    }

    #[test]
    fn test_extract_mkv_metadata_without_audio_or_subtitles() {
        let meta = extract_mkv_metadata(&InspectedVideo::new(MkvmergeMetadata::default()));
        assert_eq!(meta.audio_summary, None);
        assert_eq!(meta.has_flac, None);
        assert_eq!(meta.sub_summary, None);
        assert_eq!(meta.fonts_count, Some(0));
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
        std::fs::write(temp_dir.path().join("ep01.mkv"), b"dummy content").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let res = generate_inspection_manifest(temp_dir.path(), None, Some(&token)).await;
        assert_eq!(res.unwrap_err(), CANCELLED_ERROR);
    }

    #[test]
    fn test_subtitle_listing_stops_before_a_directory_once_cancelled() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input_dir = temp_dir.path();
        std::fs::write(input_dir.join("ep01.ass"), b"sub").unwrap();
        let directories = HashSet::from([input_dir.to_path_buf()]);
        let token = CancellationToken::new();

        let subs = list_subtitles_by_dir(&directories, input_dir, Some(&token));
        assert_eq!(
            subs.and_then(|mut by_dir| by_dir.remove(input_dir)),
            Some(vec!["ep01.ass".to_string()])
        );

        token.cancel();
        assert!(list_subtitles_by_dir(&directories, input_dir, Some(&token)).is_none());
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

    fn make_test_manifest(
        items: Vec<ManifestItem>,
        subtitle_candidates: &[(&str, &str)],
    ) -> InspectionManifest {
        InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates: subtitle_candidates
                .iter()
                .map(|(release, sub)| (release.to_string(), vec![sub.to_string()]))
                .collect(),
            items,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_select_items_reorders_and_filters_items() {
        let mut manifest = make_test_manifest(
            vec![
                make_test_item("ep01.mkv", "Episode 1", "Show"),
                make_test_item("ep02.mkv", "Episode 2", "Show"),
                make_test_item("ep03.mkv", "Episode 3", "Show"),
            ],
            &[("Show", "sub.ass")],
        );
        manifest
            .extra
            .insert("custom_key".to_string(), serde_json::Value::Bool(true));

        // Select ep03 and ep01 in reverse order, using cleaned path for ep03
        let selected = vec!["./ep03.mkv".to_string(), "ep01.mkv".to_string()];
        let filtered = manifest.select_items(&selected).unwrap();

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
    fn test_select_items_prunes_subtitle_candidates() {
        let manifest = make_test_manifest(
            vec![
                make_test_item("season1/ep01.mkv", "S1E01", "Season 1"),
                make_test_item("season2/ep01.mkv", "S2E01", "Season 2"),
            ],
            &[("Season 1", "s1.ass"), ("Season 2", "s2.ass")],
        );

        let selected = vec!["season2/ep01.mkv".to_string()];
        let filtered = manifest.select_items(&selected).unwrap();

        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].id, "season2/ep01.mkv");
        assert!(!filtered.subtitle_candidates.contains_key("Season 1"));
        assert_eq!(
            filtered.subtitle_candidates.get("Season 2").unwrap(),
            &vec!["s2.ass".to_string()]
        );
    }

    #[test]
    fn test_select_items_deduplicates_duplicate_keys() {
        let manifest = make_test_manifest(
            vec![
                make_test_item("ep01.mkv", "Episode 1", "Show"),
                make_test_item("ep02.mkv", "Episode 2", "Show"),
            ],
            &[],
        );

        // Select ep01 multiple times (both exact and with relative path prefix), and ep02
        let selected = vec![
            "ep01.mkv".to_string(),
            "./ep01.mkv".to_string(),
            "ep01.mkv".to_string(),
            "ep02.mkv".to_string(),
            "ep01.mkv".to_string(),
        ];
        let filtered = manifest.select_items(&selected).unwrap();

        assert_eq!(filtered.items.len(), 2);
        assert_eq!(filtered.items[0].id, "ep01.mkv");
        assert_eq!(filtered.items[1].id, "ep02.mkv");
    }

    #[test]
    fn test_select_items_rejects_unknown_selection() {
        let manifest = make_test_manifest(
            vec![make_test_item("ep01.mkv", "Episode 1", "Show")],
            &[("Show", "sub.ass")],
        );

        let selected = vec!["ep01.mkv".to_string(), "missing.mkv".to_string()];

        assert_eq!(
            manifest.select_items(&selected).unwrap_err(),
            "Selected items not found among discovered MKV files: missing.mkv"
        );
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
    async fn test_generate_inspection_manifest_uses_cache_and_preserves_order() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input_dir = temp_dir.path().join("input");
        std::fs::create_dir(&input_dir).unwrap();
        for i in 1..=5 {
            std::fs::write(input_dir.join(format!("ep{i:02}.mkv")), b"video content").unwrap();
        }

        let cache =
            PersistentInspectCache::new(Some(temp_dir.path().join("inspect_cache.json")), true);
        cache.put_path(&input_dir.join("ep01.mkv"), MkvmergeMetadata::default());

        let manifest = generate_inspection_manifest(&input_dir, Some(Arc::new(cache)), None)
            .await
            .unwrap();

        let ids: Vec<&str> = manifest.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ep01.mkv", "ep02.mkv", "ep03.mkv", "ep04.mkv", "ep05.mkv"]
        );
        assert!(manifest.items[0].inspection_error.is_none());
        for item in &manifest.items[1..] {
            assert!(
                item.inspection_error.is_some(),
                "{} is not a Matroska file",
                item.id
            );
        }
    }
}
