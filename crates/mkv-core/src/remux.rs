//! Remux track selection, command construction, and argument generation.

use std::path::{Path, PathBuf};

use tokio::sync::Semaphore;

use crate::domain::{
    cover_mime, promote_file, read_progress_stream, select_audio_track, SUBPROCESS_TIMEOUT,
};
use crate::fonts::FontInfo;
use crate::scan::InspectedVideo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedJob {
    pub video: PathBuf,
    pub video_name: String,
    pub output_file: PathBuf,
    pub subtitle_file: Option<PathBuf>,
    pub subtitle_label: String,
    pub subtitle_mkv_lang: String,
    pub chapter_file: Option<PathBuf>,
    pub cover_file: Option<PathBuf>,
    pub embedded_sub_id: Option<u32>,
    pub use_source_fonts: bool,
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
    pub audio_lang: String,
    pub audio_name: String,
    pub source_cover_id: Option<u32>,
    pub source_font_ids: Vec<u32>,
}

/// Selects the video and audio tracks and the source cover and font attachments of an MKV.
pub fn select_tracks(inspected: &InspectedVideo) -> TrackSelection {
    let source_cover_id = inspected.cover_attachment().and_then(|c| c.id);
    let source_font_ids: Vec<u32> = inspected
        .font_attachments()
        .iter()
        .filter_map(|att| att.id)
        .collect();

    let video_id = inspected.video_track().and_then(|v| v.id);
    let video_pos = u32::try_from(inspected.video_track_pos()).unwrap_or(0);
    let audio = select_audio_track(&inspected.audio_tracks(), video_pos);

    let audio_id = audio.as_ref().map(|a| a.id);
    let (audio_index, audio_codec, audio_lang, audio_name) = match audio {
        Some(a) => (a.audio_idx, a.codec, a.lang, a.name),
        None => (0, String::new(), String::new(), String::new()),
    };

    TrackSelection {
        video_id,
        audio_id,
        audio_index,
        audio_codec,
        audio_lang,
        audio_name,
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
    let mut args: Vec<String> = vec!["-o".to_string(), output_path.to_string_lossy().into_owned()];

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

    args.push(job.video.to_string_lossy().into_owned());

    if let Some(opus) = temp_opus {
        if !selection.audio_lang.is_empty() {
            args.push("--language".to_string());
            args.push(format!("0:{}", selection.audio_lang));
        }
        if !selection.audio_name.is_empty() {
            args.push("--track-name".to_string());
            args.push(format!("0:{}", selection.audio_name));
        }
        args.push(opus.to_string_lossy().into_owned());
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

        let lang = job
            .custom_language
            .as_deref()
            .unwrap_or(&job.subtitle_mkv_lang);
        if !lang.is_empty() {
            args.push("--language".to_string());
            args.push(format!("0:{lang}"));
        }

        let title = job.custom_title.as_deref().unwrap_or(&job.subtitle_label);
        if !title.is_empty() {
            args.push("--track-name".to_string());
            args.push(format!("0:{title}"));
        }

        args.push("--default-track-flag".to_string());
        args.push("0:yes".to_string());
        args.push(sub_file.to_string_lossy().into_owned());
    }

    if let Some(chapter_file) = &job.chapter_file {
        args.push("--chapters".to_string());
        args.push(chapter_file.to_string_lossy().into_owned());
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
        args.push(cover_file.to_string_lossy().into_owned());
    }

    args.extend_from_slice(&fonts.args);
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
        video.to_string_lossy().into_owned(),
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
        temp_opus.to_string_lossy().into_owned(),
    ]
}

pub struct RemuxOptions<'a> {
    pub opus_bitrate: &'a str,
    pub complexity: u32,
    pub ffmpeg_threads: usize,
    pub sub_offset: f64,
}

/// Joins trimmed subprocess stderr and stdout, omitting whichever is empty.
fn combined_output(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    match (stderr.trim(), stdout.trim()) {
        ("", out) => out.to_owned(),
        (err, "") => err.to_owned(),
        (err, out) => format!("{err}\n{out}"),
    }
}

/// Executes audio transcoding (if required) and remuxing via `mkvmerge` into `job.output_file`,
/// returning the job's log lines.
pub async fn execute_remux_job(
    job: &PlannedJob,
    selection: &TrackSelection,
    fonts: &FontInfo,
    options: &RemuxOptions<'_>,
    on_progress: Option<&(dyn Fn(f64) + Send + Sync)>,
    io_semaphore: Option<&Semaphore>,
) -> Result<Vec<String>, String> {
    let mut log = vec![format!("Processing: {}", job.video_name)];

    let output_file_name = job.output_file.file_name().ok_or_else(|| {
        format!(
            "Output path has no file name: {}",
            job.output_file.display()
        )
    })?;

    let out_dir = job.output_file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(out_dir).map_err(|e| {
        format!(
            "Failed to create output directory '{}': {e}",
            out_dir.display()
        )
    })?;

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
        log.push("  Transcoding audio to Opus with ffmpeg...".into());
        let mut ffmpeg_cmd = tokio::process::Command::new("ffmpeg");
        #[cfg(windows)]
        ffmpeg_cmd.creation_flags(crate::domain::CREATE_NO_WINDOW);
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

    log.push("  Running mkvmerge...".into());
    let mut mkvmerge_cmd = tokio::process::Command::new("mkvmerge");
    #[cfg(windows)]
    mkvmerge_cmd.creation_flags(crate::domain::CREATE_NO_WINDOW);
    mkvmerge_cmd.kill_on_drop(true);
    mkvmerge_cmd.args(&mkvmerge_args);
    mkvmerge_cmd.stdout(std::process::Stdio::piped());
    mkvmerge_cmd.stderr(std::process::Stdio::piped());

    let mut child = mkvmerge_cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn mkvmerge: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stderr_handle = tokio::spawn(async move {
        let mut stderr_bytes = Vec::new();
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
            let _ = read_progress_stream(out_stream, on_progress, &mut stdout_bytes).await;
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

    // mkvmerge exits 1 when it finished with warnings.
    match status.code() {
        Some(0) => {}
        Some(1) => {
            let warning_output = combined_output(&stderr_output, &stdout_bytes);
            if !warning_output.is_empty() {
                if warning_output.to_lowercase().starts_with("warning") {
                    log.push(format!("  {warning_output}"));
                } else {
                    log.push(format!("  Warning: {warning_output}"));
                }
            }
        }
        code => {
            let _ = std::fs::remove_file(&temp_output);
            let code_str = code
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".into());
            let output = combined_output(&stderr_output, &stdout_bytes);
            let err_msg = if output.is_empty() {
                &code_str
            } else {
                &output
            };
            return Err(format!("mkvmerge failed ({code_str}): {err_msg}"));
        }
    }

    if !temp_output.metadata().is_ok_and(|m| m.len() > 0) {
        return Err("mkvmerge succeeded but output file was not created or is empty".into());
    }

    promote_file(&temp_output, &job.output_file)
        .map_err(|e| format!("Failed to promote remuxed file to final destination: {e}"))?;

    log.push(format!("  Completed -> {}", job.output_file.display()));
    Ok(log)
}
