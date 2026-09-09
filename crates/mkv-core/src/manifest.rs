use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::assets::SubIndex;
use crate::domain::{ancestor_dirs, MkvTrack};
use crate::scan::{
    scan_assets, scan_mkv_files, InspectedVideo, PersistentInspectCache, ScanResults,
};

pub const MANIFEST_VERSION: u64 = 2;

/// Derives a root-relative POSIX path ID for sidecar contract events and manifest items.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InspectionMetadata {
    pub resolution: Option<String>,
    pub video_codec: Option<String>,
    pub video_tag: Option<String>,
    pub audio_summary: Option<String>,
    pub has_flac: Option<bool>,
    pub sub_summary: Option<String>,
    pub fonts_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestItem {
    pub id: String,
    pub name: String,
    pub release: String,
    pub metadata: InspectionMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionManifest {
    pub version: u64,
    #[serde(default)]
    pub subtitle_candidates: BTreeMap<String, Vec<String>>,
    pub items: Vec<ManifestItem>,
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
    }
}

/// Generates a complete preflight inspection manifest for an anime batch directory.
pub fn generate_inspection_manifest(
    input_dir: &Path,
    persistent_cache: Option<&PersistentInspectCache>,
) -> InspectionManifest {
    let all_mkvs = scan_mkv_files(input_dir, true, None);
    if all_mkvs.is_empty() {
        return InspectionManifest {
            version: MANIFEST_VERSION,
            subtitle_candidates: BTreeMap::new(),
            items: Vec::new(),
        };
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

    for mkv_dir in videos_by_dir.keys() {
        let release = get_item_id(mkv_dir, input_dir);
        release_by_dir.insert(mkv_dir.clone(), release.clone());

        let mut sub_index = SubIndex::new();
        let mut ancestors = ancestor_dirs(mkv_dir, input_dir);
        ancestors.reverse();

        for directory in ancestors {
            let assets = scan_assets(&directory, Some(&mut scan_cache));
            for subtitle in assets.subs {
                sub_index.add(subtitle);
            }
        }

        let candidates: Vec<String> = sub_index
            .all_subtitles()
            .iter()
            .map(|path| get_item_id(path, input_dir))
            .collect();

        if !candidates.is_empty() {
            subtitle_candidates.insert(release, candidates);
        }
    }

    let mut items = Vec::with_capacity(all_mkvs.len());
    for mkv in &all_mkvs {
        let (inspected, inspection_error) =
            match persistent_cache.and_then(|c| c.get_path(&mkv.path)) {
                Some(cached) => (Some(InspectedVideo::new(cached)), None),
                None => match crate::scan::probe_video(&mkv.path) {
                    Ok(probed) => {
                        if let Some(cache) = persistent_cache {
                            cache.put_path(&mkv.path, probed.clone());
                        }
                        (Some(InspectedVideo::new(probed)), None)
                    }
                    Err(err) => (None, Some(err)),
                },
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
        };
        items.push(item);
    }

    if let Some(cache) = persistent_cache {
        cache.save_shared();
    }

    InspectionManifest {
        version: MANIFEST_VERSION,
        subtitle_candidates,
        items,
    }
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
}
