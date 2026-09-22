//! Remux track selection, command construction, and argument generation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::domain::{
    cover_mime, promote_file, read_progress_stream, select_audio_track, SUBPROCESS_TIMEOUT,
};
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
    pub custom_offset_ms: Option<i64>,
    pub custom_language: Option<String>,
    pub custom_title: Option<String>,
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
    let (audio, reason) = select_audio_track(
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

    match (&job.subtitle_file, job.embedded_sub_id) {
        (None, Some(sid)) => {
            args.push("--subtitle-tracks".to_string());
            args.push(sid.to_string());
            args.extend(format_sync_args(sid, sub_offset));
        }
        _ => args.push("--no-subtitles".to_string()),
    }

    match (temp_opus, selection.audio_id) {
        (None, Some(audio_id)) => {
            args.push("--audio-tracks".to_string());
            args.push(audio_id.to_string());
        }
        _ => args.push("--no-audio".to_string()),
    }

    let mut keep_ids: Vec<u32> = Vec::new();
    if job.cover_file.is_none() {
        keep_ids.extend(selection.source_cover_id);
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
        if let Some(ms) = job.custom_offset_ms {
            if ms != 0 {
                args.push("--sync".to_string());
                args.push(format!("0:{ms}"));
            }
        } else {
            args.extend(format_sync_args(0, sub_offset));
        }

        let lang = match &job.custom_language {
            Some(lang) => lang.as_str(),
            None => job.subtitle_mkv_lang.as_str(),
        };
        if !lang.is_empty() {
            args.push("--language".to_string());
            args.push(format!("0:{lang}"));
        }

        let title = match &job.custom_title {
            Some(title) => title.as_str(),
            None => job.subtitle_label.as_str(),
        };
        if !title.is_empty() {
            args.push("--track-name".to_string());
            args.push(format!("0:{title}"));
        }

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

#[derive(Debug, Default, Clone)]
pub struct RemuxSummary {
    pub video_path: PathBuf,
    pub output_file: PathBuf,
    pub log: Vec<String>,
}

pub struct RemuxOptions<'a> {
    pub opus_bitrate: &'a str,
    pub complexity: u32,
    pub ffmpeg_threads: usize,
    pub sub_offset: f64,
}

/// Executes audio transcoding (if required) and remuxing via `mkvmerge` into `job.output_file`.
pub async fn execute_remux_job(
    job: &PlannedJob,
    selection: &TrackSelection,
    fonts: &FontInfo,
    options: &RemuxOptions<'_>,
    on_progress: Option<&(dyn Fn(f64) + Send + Sync)>,
    io_semaphore: Option<&Arc<tokio::sync::Semaphore>>,
) -> Result<RemuxSummary, String> {
    let mut summary = RemuxSummary {
        video_path: job.video.clone(),
        output_file: job.output_file.clone(),
        log: Vec::new(),
    };
    summary.log.push(format!("Processing: {}", job.video_name));

    let output_file_name = job.output_file.file_name().ok_or_else(|| {
        format!(
            "Output path has no file name: {}",
            job.output_file.display()
        )
    })?;

    let out_dir = job.output_file.parent().unwrap_or(Path::new("."));
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        return Err(format!(
            "Failed to create output directory '{}': {e}",
            out_dir.display()
        ));
    }

    let scratch_dir = tempfile::Builder::new()
        .prefix(".tmp_remux_")
        .tempdir_in(out_dir)
        .map_err(|e| format!("Failed to create temporary remux directory: {e}"))?;

    let mut temp_opus_file: Option<PathBuf> = None;
    if selection.audio_id.is_some() && selection.audio_codec == "A_FLAC" {
        let temp_opus = scratch_dir.path().join("audio.opus");
        let ffmpeg_args = build_ffmpeg_args(
            &job.video,
            selection.audio_index,
            &temp_opus,
            options.opus_bitrate,
            options.complexity,
            options.ffmpeg_threads,
        );
        summary
            .log
            .push("  Transcoding audio to Opus with ffmpeg...".into());
        let mut ffmpeg_cmd = tokio::process::Command::new("ffmpeg");
        #[cfg(windows)]
        ffmpeg_cmd.creation_flags(0x08000000);
        ffmpeg_cmd.kill_on_drop(true);
        ffmpeg_cmd.args(&ffmpeg_args);
        let ffmpeg_out = match tokio::time::timeout(SUBPROCESS_TIMEOUT, ffmpeg_cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("Failed to execute ffmpeg: {e}")),
            Err(_) => {
                return Err(format!(
                    "ffmpeg audio transcode timed out after {} seconds",
                    SUBPROCESS_TIMEOUT.as_secs()
                ))
            }
        };
        if !ffmpeg_out.status.success() {
            let stderr = String::from_utf8_lossy(&ffmpeg_out.stderr);
            return Err(format!("ffmpeg audio transcode failed: {stderr}"));
        }
        temp_opus_file = Some(temp_opus);
    }

    let temp_output = scratch_dir.path().join(output_file_name);
    let mkvmerge_args = build_mkvmerge_args(
        job,
        selection,
        temp_opus_file.as_deref(),
        fonts,
        &temp_output,
        options.sub_offset,
    );
    // ADR-0011 Dual-Governor: CPU-intensive Opus transcoding runs in worker slot;
    // disk-intensive remux multiplexing (mkvmerge) is guarded by io_semaphore.
    let _io_permit = match io_semaphore {
        Some(sem) => sem.acquire().await.ok(),
        None => None,
    };

    summary.log.push("  Running mkvmerge...".into());
    let mut mkvmerge_cmd = tokio::process::Command::new("mkvmerge");
    #[cfg(windows)]
    mkvmerge_cmd.creation_flags(0x08000000);
    mkvmerge_cmd.kill_on_drop(true);
    mkvmerge_cmd.args(&mkvmerge_args);
    mkvmerge_cmd.stdout(std::process::Stdio::piped());
    mkvmerge_cmd.stderr(std::process::Stdio::piped());

    let mut child = mkvmerge_cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn mkvmerge: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut stderr_bytes = Vec::new();
    let stderr_handle = tokio::spawn(async move {
        if let Some(mut stream) = stderr {
            use tokio::io::AsyncReadExt;
            let _ = stream.read_to_end(&mut stderr_bytes).await;
        }
        stderr_bytes
    });
    let stderr_abort = stderr_handle.abort_handle();
    let execution = async {
        let mut stdout_bytes = Vec::new();
        if let Some(out_stream) = stdout {
            let _ = read_progress_stream(out_stream, on_progress, Some(&mut stdout_bytes)).await;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| format!("Failed to wait on mkvmerge: {e}"))?;
        let stderr_output = stderr_handle.await.unwrap_or_default();
        Ok::<_, String>((status, stdout_bytes, stderr_output))
    };

    let (status, stdout_bytes, stderr_output) =
        match tokio::time::timeout(SUBPROCESS_TIMEOUT, execution).await {
            Ok(res) => res?,
            Err(_) => {
                stderr_abort.abort();
                let _ = std::fs::remove_file(&temp_output);
                return Err(format!(
                    "mkvmerge timed out after {} seconds",
                    SUBPROCESS_TIMEOUT.as_secs()
                ));
            }
        };

    let is_success = matches!(status.code(), Some(0) | Some(1));
    if !is_success {
        let _ = std::fs::remove_file(&temp_output);
        let stderr = String::from_utf8_lossy(&stderr_output).trim().to_string();
        let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
        let code_str = status
            .code()
            .map(|c| format!("exit code {c}"))
            .unwrap_or_else(|| "terminated by signal".into());
        let err_msg = if !stderr.is_empty() && !stdout.is_empty() {
            format!("{stderr}\n{stdout}")
        } else if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            code_str.clone()
        };
        return Err(format!("mkvmerge failed ({code_str}): {err_msg}"));
    }

    if status.code() == Some(1) {
        let stderr = String::from_utf8_lossy(&stderr_output).trim().to_string();
        let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
        let warning_output = if !stderr.is_empty() && !stdout.is_empty() {
            format!("{stderr}\n{stdout}")
        } else if !stderr.is_empty() {
            stderr
        } else {
            stdout
        };
        if !warning_output.is_empty() {
            if warning_output.to_lowercase().starts_with("warning") {
                summary.log.push(format!("  {warning_output}"));
            } else {
                summary.log.push(format!("  Warning: {warning_output}"));
            }
        }
    }

    if !temp_output.exists() || temp_output.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        return Err("mkvmerge succeeded but output file was not created or is empty".into());
    }

    promote_file(&temp_output, &job.output_file)
        .map_err(|e| format!("Failed to promote remuxed file to final destination: {e}"))?;

    summary
        .log
        .push(format!("  Completed -> {}", job.output_file.display()));
    Ok(summary)
}
