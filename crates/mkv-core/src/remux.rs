//! Remux track selection, command construction, and argument generation.

use std::path::{Path, PathBuf};

use crate::domain::{cover_mime, pick_audio};
use crate::fonts::FontInfo;
use crate::scan::InspectedVideo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedJob {
    pub video: PathBuf,
    pub video_name: String,
    pub base: String,
    pub output_file: PathBuf,
    pub subtitle_file: Option<PathBuf>,
    pub subtitle_label: String,
    pub subtitle_mkv_lang: String,
    pub chapter_file: Option<PathBuf>,
    pub cover_file: Option<PathBuf>,
    pub embedded_sub_id: Option<u32>,
    pub use_source_fonts: bool,
    pub release_dir: Option<PathBuf>,
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSelection {
    pub video_id: Option<u32>,
    pub audio_id: Option<u32>,
    pub audio_index: usize,
    pub audio_codec: String,
    pub audio_channels: u32,
    pub audio_lang: String,
    pub audio_name: String,
    pub audio_reason: String,
    pub has_source_chapters: bool,
    pub source_cover_id: Option<u32>,
    pub source_font_ids: Vec<u32>,
}

/// Selects video, audio, chapter, cover, and font tracks from an inspected MKV.
pub fn select_tracks(inspected: &InspectedVideo) -> TrackSelection {
    let has_source_chapters = inspected.has_chapters();
    let source_cover_id = inspected.cover_attachment().and_then(|c| c.id);
    let source_font_ids: Vec<u32> = inspected
        .font_attachments()
        .iter()
        .filter_map(|att| att.id)
        .collect();

    let video_track = inspected.video_track();
    let video_id = video_track.and_then(|v| v.id);
    let video_pos = inspected.video_track_pos();

    let audio_tracks = inspected.audio_tracks();
    let (audio, reason) = pick_audio(
        &audio_tracks,
        if video_pos >= 0 { video_pos as u32 } else { 0 },
    );

    let audio_id = audio.as_ref().map(|a| a.id);
    let (audio_index, audio_codec, audio_channels, audio_lang, audio_name) = match &audio {
        Some(a) => (
            a.audio_idx,
            a.codec.clone(),
            a.channels,
            a.lang.clone(),
            a.name.clone(),
        ),
        None => (0, String::new(), 0, String::new(), String::new()),
    };

    TrackSelection {
        video_id,
        audio_id,
        audio_index,
        audio_codec,
        audio_channels,
        audio_lang,
        audio_name,
        audio_reason: reason,
        has_source_chapters,
        source_cover_id,
        source_font_ids,
    }
}

fn format_sync_args(track_id: u32, sub_offset: f64) -> Vec<String> {
    if sub_offset != 0.0 {
        let offset_ms = (sub_offset * 1000.0).round() as i64;
        vec!["--sync".to_string(), format!("{track_id}:{offset_ms}")]
    } else {
        Vec::new()
    }
}

/// Builds complete command-line argument list for `mkvmerge`.
pub fn build_mkvmerge_args(
    job: &PlannedJob,
    selection: &TrackSelection,
    temp_opus: Option<&Path>,
    fonts: &FontInfo,
    output_path: &Path,
    sub_offset: f64,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-o".to_string(), output_path.to_string_lossy().to_string()];

    if job.chapter_file.is_some() {
        args.push("--no-chapters".to_string());
    }

    if job.subtitle_file.is_none() {
        if let Some(sid) = job.embedded_sub_id {
            args.push("--subtitle-tracks".to_string());
            args.push(sid.to_string());
            args.extend(format_sync_args(sid, sub_offset));
        } else {
            args.push("--no-subtitles".to_string());
        }
    } else {
        args.push("--no-subtitles".to_string());
    }

    if temp_opus.is_none() {
        if let Some(audio_id) = selection.audio_id {
            args.push("--audio-tracks".to_string());
            args.push(audio_id.to_string());
        } else {
            args.push("--no-audio".to_string());
        }
    } else {
        args.push("--no-audio".to_string());
    }

    let mut keep_ids: Vec<u32> = Vec::new();
    if job.cover_file.is_none() {
        if let Some(cover_id) = selection.source_cover_id {
            keep_ids.push(cover_id);
        }
    }
    if job.use_source_fonts {
        keep_ids.extend(&selection.source_font_ids);
    }

    if !keep_ids.is_empty() {
        let ids_str = keep_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        args.push("--attachments".to_string());
        args.push(ids_str);
    } else {
        args.push("--no-attachments".to_string());
    }

    if let Some(vid) = selection.video_id {
        args.push("--video-tracks".to_string());
        args.push(vid.to_string());
    } else {
        args.push("--no-video".to_string());
    }

    args.push(job.video.to_string_lossy().to_string());

    if let Some(opus) = temp_opus {
        args.push("--no-chapters".to_string());
        if !selection.audio_lang.is_empty() {
            args.push("--language".to_string());
            args.push(format!("0:{}", selection.audio_lang));
        }
        if !selection.audio_name.is_empty() {
            args.push("--track-name".to_string());
            args.push(format!("0:{}", selection.audio_name));
        }
        args.push(opus.to_string_lossy().to_string());
    }

    if let Some(sub_file) = &job.subtitle_file {
        args.extend(format_sync_args(0, sub_offset));
        args.push("--language".to_string());
        args.push(format!("0:{}", job.subtitle_mkv_lang));
        args.push("--track-name".to_string());
        args.push(format!("0:{}", job.subtitle_label));
        args.push("--default-track-flag".to_string());
        args.push("0:yes".to_string());
        args.push(sub_file.to_string_lossy().to_string());
    }

    if let Some(chapter_file) = &job.chapter_file {
        args.push("--chapters".to_string());
        args.push(chapter_file.to_string_lossy().to_string());
    }

    if let Some(cover_file) = &job.cover_file {
        let ext = cover_file
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        let mime = cover_mime(&ext).unwrap_or("image/jpeg");
        args.push("--attachment-name".to_string());
        args.push(format!("cover{ext}"));
        args.push("--attachment-mime-type".to_string());
        args.push(mime.to_string());
        args.push("--attach-file".to_string());
        args.push(cover_file.to_string_lossy().to_string());
    }

    args.extend(fonts.args.clone());
    args
}

/// Builds command-line argument list for audio transcoding via `ffmpeg`.
pub fn build_ffmpeg_args(
    video: &Path,
    audio_index: usize,
    temp_opus: &Path,
    opus_bitrate: &str,
    complexity: u32,
    ffmpeg_threads: usize,
) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-analyzeduration".to_string(),
        "0".to_string(),
        "-probesize".to_string(),
        "1M".to_string(),
        "-v".to_string(),
        "warning".to_string(),
        "-threads".to_string(),
        ffmpeg_threads.to_string(),
        "-i".to_string(),
        video.to_string_lossy().to_string(),
        "-map".to_string(),
        format!("0:a:{audio_index}"),
        "-c:a".to_string(),
        "libopus".to_string(),
        "-b:a".to_string(),
        opus_bitrate.to_string(),
        "-vbr".to_string(),
        "on".to_string(),
        "-compression_level".to_string(),
        complexity.to_string(),
        "-application".to_string(),
        "audio".to_string(),
        temp_opus.to_string_lossy().to_string(),
    ]
}
