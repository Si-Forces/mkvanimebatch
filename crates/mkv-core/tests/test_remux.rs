mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{ScriptedRunner, Step};
use mkv_core::domain::{MkvTrack, TrackProperties, SUBPROCESS_TIMEOUT};
use mkv_core::fonts::FontInfo;
use mkv_core::log_line::LogLine;
use mkv_core::media_tool::MediaTool;
use mkv_core::remux::{
    build_ffmpeg_args, build_mkvmerge_args, execute_remux_job, select_tracks, PlannedJob,
    RemuxOptions, TrackSelection,
};
use mkv_core::scan::{InspectedVideo, MkvmergeMetadata};

fn job(video: &Path, output_file: PathBuf, subtitle_file: Option<PathBuf>) -> PlannedJob {
    let video_name = video.file_name().unwrap().to_string_lossy().into_owned();
    PlannedJob {
        video: video.to_path_buf(),
        video_name,
        output_file,
        subtitle_file,
        subtitle_label: "Default English".to_string(),
        subtitle_mkv_lang: "eng".to_string(),
        chapter_file: None,
        chapter_warning: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        custom_offset_ms: None,
        custom_language: None,
        custom_title: None,
    }
}

/// A job muxing `/dummy/sub.ass` into `/dummy/video.mkv`.
fn subtitle_job() -> PlannedJob {
    job(
        Path::new("/dummy/video.mkv"),
        PathBuf::from("/dummy/out.mkv"),
        Some(PathBuf::from("/dummy/sub.ass")),
    )
}

fn selection() -> TrackSelection {
    TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        source_cover_id: None,
        source_font_ids: vec![],
    }
}

fn mkvmerge_args(
    job: &PlannedJob,
    temp_opus: Option<&Path>,
    default_sub_offset_ms: i64,
) -> Vec<String> {
    build_mkvmerge_args(
        job,
        &selection(),
        temp_opus,
        &FontInfo::default(),
        Path::new("/dummy/out.mkv"),
        default_sub_offset_ms,
    )
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|pair| pair == [flag, value])
}

/// Whether `--no-chapters` applies to `input`. mkvmerge file options bind to the next input
/// file, so only the options between the previous `/dummy/` path and `input` count.
fn chapters_dropped_from(args: &[String], input: &str) -> bool {
    let at = args.iter().position(|a| a == input).unwrap();
    args[..at]
        .iter()
        .rev()
        .take_while(|a| !a.starts_with("/dummy/"))
        .any(|a| a == "--no-chapters")
}

const REMUX_OPTIONS: RemuxOptions<'static> = RemuxOptions {
    opus_bitrate: "192k",
    complexity: 10,
    ffmpeg_threads: 1,
    default_sub_offset_ms: 0,
};

#[test]
fn test_select_tracks() {
    let meta = MkvmergeMetadata {
        tracks: vec![
            MkvTrack {
                id: Some(0),
                track_type: "video".to_string(),
                codec: None,
                properties: TrackProperties::default(),
            },
            MkvTrack {
                id: Some(1),
                track_type: "audio".to_string(),
                codec: None,
                properties: TrackProperties {
                    language: Some("jpn".to_string()),
                    track_name: Some("Stereo".to_string()),
                    audio_channels: Some(2),
                    codec_id: Some("A_FLAC".to_string()),
                    ..Default::default()
                },
            },
        ],
        attachments: vec![],
        chapters: vec![],
    };
    let selection = select_tracks(&InspectedVideo::new(meta));
    assert_eq!(selection.video_id, Some(0));
    assert_eq!(selection.audio_id, Some(1));
    assert_eq!(selection.audio_codec, "A_FLAC");
}

/// ffmpeg copies the source chapters into the transcoded Ogg file, so the Opus input always
/// drops them; the source drops its own only when an external chapter file replaces them.
#[test]
fn test_no_chapters_on_transcoded_audio_and_replaced_source_chapters() {
    let opus = Some(Path::new("/dummy/audio.opus"));

    let args = mkvmerge_args(&subtitle_job(), opus, 0);
    assert!(!chapters_dropped_from(&args, "/dummy/video.mkv"));
    assert!(chapters_dropped_from(&args, "/dummy/audio.opus"));

    let mut job = subtitle_job();
    job.chapter_file = Some(PathBuf::from("/dummy/chapters.xml"));
    let args = mkvmerge_args(&job, opus, 0);
    assert!(chapters_dropped_from(&args, "/dummy/video.mkv"));
    assert!(chapters_dropped_from(&args, "/dummy/audio.opus"));
}

#[test]
fn test_source_chapters_kept_without_external_chapters() {
    let args = mkvmerge_args(&subtitle_job(), None, 0);
    assert!(!args.contains(&"--no-chapters".to_string()));
}

#[test]
fn test_subtitle_sync_args() {
    let job = subtitle_job();
    assert!(has_pair(
        &mkvmerge_args(&job, None, 1500),
        "--sync",
        "0:1500"
    ));
    assert!(has_pair(
        &mkvmerge_args(&job, None, -500),
        "--sync",
        "0:-500"
    ));
    assert!(!mkvmerge_args(&job, None, 0).contains(&"--sync".to_string()));
}

/// An embedded subtitle has no Track Assignment, so only the default offset can shift it,
/// keyed by its source track id rather than `0`.
#[test]
fn test_embedded_subtitle_sync_args() {
    let mut job = job(
        Path::new("/dummy/video.mkv"),
        PathBuf::from("/dummy/out.mkv"),
        None,
    );
    job.embedded_sub_id = Some(3);

    let args = mkvmerge_args(&job, None, 1500);
    assert!(has_pair(&args, "--subtitle-tracks", "3"));
    assert!(has_pair(&args, "--sync", "3:1500"));
    assert!(!mkvmerge_args(&job, None, 0).contains(&"--sync".to_string()));
}

#[test]
fn test_ffmpeg_args() {
    let video = PathBuf::from("/dummy/video.mkv");
    let temp_opus = PathBuf::from("/dummy/audio.opus");
    let args = build_ffmpeg_args(&video, 1, &temp_opus, "192k", 10, 4);

    assert_eq!(args[0], "-y");
    assert!(has_pair(&args, "-map", "0:a:1"));
    assert!(has_pair(&args, "-c:a", "libopus"));
    assert!(has_pair(&args, "-b:a", "192k"));
    assert!(has_pair(&args, "-compression_level", "10"));
    assert_eq!(args.last().unwrap(), "/dummy/audio.opus");
}

#[test]
fn test_custom_track_assignment_remux_args() {
    let mut job = subtitle_job();
    job.custom_offset_ms = Some(750);
    job.custom_language = Some("ind".into());
    job.custom_title = Some("Custom Translation".into());

    // A custom offset overrides default_sub_offset_ms instead of adding to it.
    let args_zero_offset = mkvmerge_args(&job, None, 0);
    assert!(has_pair(&args_zero_offset, "--sync", "0:750"));
    assert!(has_pair(&args_zero_offset, "--language", "0:ind"));
    assert!(has_pair(
        &args_zero_offset,
        "--track-name",
        "0:Custom Translation"
    ));

    let args_diff_offset = mkvmerge_args(&job, None, 2000);
    assert!(has_pair(&args_diff_offset, "--sync", "0:750"));
    assert!(!args_diff_offset.contains(&"0:2000".to_string()));
    assert!(!args_diff_offset.contains(&"0:2750".to_string()));

    // An explicit zero custom offset leaves the subtitle unshifted, even with a default set.
    job.custom_offset_ms = Some(0);
    assert!(!mkvmerge_args(&job, None, 2000).contains(&"--sync".to_string()));

    // Without custom fields the global offset and resolved label/language apply.
    job.custom_offset_ms = None;
    job.custom_language = None;
    job.custom_title = None;

    let args_fallback = mkvmerge_args(&job, None, 1250);
    assert!(has_pair(&args_fallback, "--sync", "0:1250"));
    assert!(has_pair(&args_fallback, "--language", "0:eng"));
    assert!(has_pair(
        &args_fallback,
        "--track-name",
        "0:Default English"
    ));
}

#[tokio::test]
async fn test_execute_remux_job_no_file_name_returns_err() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("test.mkv");
    std::fs::File::create(&video).unwrap();
    // Path ending in ".." has no file_name(), but a valid writable parent
    let out = dir.path().join("subdir").join("..");

    let err = execute_remux_job(
        &job(&video, out, None),
        &selection(),
        &FontInfo::default(),
        &REMUX_OPTIONS,
        None,
        &tokio::sync::Semaphore::new(1),
        &ScriptedRunner::new([]),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("Output path has no file name"),
        "Unexpected error message: {err}"
    );
}

/// Remuxes an empty source video through `runner`, with `selection` choosing the audio track.
///
/// Takes a `&Path` rather than a `&TempDir` so callers can move the directory into a spawned
/// task.
async fn remux_with(
    dir: &Path,
    selection: &TrackSelection,
    io_semaphore: &tokio::sync::Semaphore,
    runner: &ScriptedRunner,
) -> Result<Vec<LogLine>, String> {
    let video = dir.join("test.mkv");
    std::fs::File::create(&video).unwrap();
    let out = dir.join("out.mkv");
    execute_remux_job(
        &job(&video, out, None),
        selection,
        &FontInfo::default(),
        &REMUX_OPTIONS,
        None,
        io_semaphore,
        runner,
    )
    .await
}

/// Remuxes a copied-audio source with an uncontended I/O governor.
async fn remux(dir: &Path, runner: &ScriptedRunner) -> Result<Vec<LogLine>, String> {
    remux_with(dir, &selection(), &tokio::sync::Semaphore::new(1), runner).await
}

#[tokio::test]
async fn test_execute_remux_job_collects_stdout_on_mkvmerge_failure() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvmerge,
        2,
        "Error: Simulated mkvmerge stdout failure\n",
        "",
    )]);

    let err = remux(dir.path(), &runner).await.unwrap_err();
    assert!(
        err.contains("Simulated mkvmerge stdout failure"),
        "Error message did not contain stdout output: {err}"
    );
    assert!(
        err.contains("exit code 2"),
        "Error message did not contain exit code: {err}"
    );
}

#[tokio::test]
async fn test_execute_remux_job_combines_stderr_and_stdout_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvmerge,
        2,
        "Error: Detailed stdout failure\n",
        "Auxiliary stderr notice\n",
    )]);

    let err = remux(dir.path(), &runner).await.unwrap_err();
    assert!(
        err.contains("Auxiliary stderr notice\nError: Detailed stdout failure"),
        "Error message did not join stderr and stdout: {err}"
    );
}

#[tokio::test]
async fn test_execute_remux_job_logs_warnings_on_exit_code_1() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvmerge,
        1,
        "Warning: QuickTime track parsing\n",
        "",
    )
    .writes_output(b"dummy data\n")]);

    let log = remux(dir.path(), &runner).await.unwrap();
    assert!(
        log.iter()
            .any(|l| l == &LogLine::warning("  Warning: QuickTime track parsing")),
        "log did not contain the warning: {log:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("out.mkv")).unwrap(),
        b"dummy data\n"
    );
}

/// The item's chapter warning comes right after "Processing:", before any mkvmerge warning.
#[tokio::test]
async fn test_execute_remux_job_logs_the_chapter_warning_after_processing() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("test.mkv");
    std::fs::File::create(&video).unwrap();
    let mut job = job(&video, dir.path().join("out.mkv"), None);
    job.chapter_warning = Some("More than one chapter file".to_string());
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvmerge,
        1,
        "Warning: QuickTime track parsing\n",
        "",
    )
    .writes_output(b"dummy data\n")]);

    let log = execute_remux_job(
        &job,
        &selection(),
        &FontInfo::default(),
        &REMUX_OPTIONS,
        None,
        &tokio::sync::Semaphore::new(1),
        &runner,
    )
    .await
    .unwrap();

    assert_eq!(log[0], LogLine::step("Processing: test.mkv"));
    assert_eq!(
        log[1],
        LogLine::warning("  Warning: More than one chapter file")
    );
    let mkvmerge_warning = log
        .iter()
        .position(|l| l.text == "  Warning: QuickTime track parsing")
        .unwrap_or_else(|| panic!("{log:?}"));
    assert!(mkvmerge_warning > 1, "{log:?}");
}

/// The governor is exhausted first, so the runner must not be invoked until a permit is
/// released. Once it is, the remux completes and gives the permit back.
#[tokio::test]
async fn test_execute_remux_job_waits_for_an_io_governor_permit() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    let runner = Arc::new(ScriptedRunner::new([Step::exited(
        MediaTool::Mkvmerge,
        0,
        "",
        "",
    )
    .writes_output(b"dummy data\n")]));
    let semaphore = Arc::new(tokio::sync::Semaphore::new(0));

    let mut job = tokio::spawn({
        let (runner, semaphore) = (Arc::clone(&runner), Arc::clone(&semaphore));
        async move { remux_with(&dir_path, &selection(), &semaphore, &runner).await }
    });

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut job)
            .await
            .is_err(),
        "remux finished while the I/O governor had no permits available"
    );
    assert!(
        runner.calls().is_empty(),
        "mkvmerge started before acquiring an I/O permit"
    );

    semaphore.add_permits(1);
    let log = job.await.unwrap().unwrap();

    assert_eq!(runner.calls().len(), 1);
    assert_eq!(runner.timeouts(), [SUBPROCESS_TIMEOUT]);
    assert_eq!(
        std::fs::read(dir.path().join("out.mkv")).unwrap(),
        b"dummy data\n"
    );
    assert_eq!(
        semaphore.available_permits(),
        1,
        "I/O permit leaked after completion: {log:?}"
    );
}

/// Bypassing a closed governor would silently remove the ADR-0011 disk-write limit.
#[tokio::test]
async fn test_execute_remux_job_refuses_a_closed_io_governor() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new([
        Step::exited(MediaTool::Mkvmerge, 0, "", "").writes_output(b"dummy data\n")
    ]);
    let semaphore = tokio::sync::Semaphore::new(1);
    semaphore.close();

    let err = remux_with(dir.path(), &selection(), &semaphore, &runner)
        .await
        .unwrap_err();
    // Pins the exact wording users see in the item error.
    assert_eq!(
        err,
        "I/O governor closed; refusing to run without the disk-write limit"
    );
    assert!(runner.calls().is_empty());
    assert!(!dir.path().join("out.mkv").exists());
}

/// A FLAC track is transcoded by ffmpeg first; an ffmpeg failure ends the item before mkvmerge.
#[tokio::test]
async fn test_execute_remux_job_stops_when_the_ffmpeg_transcode_fails() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Ffmpeg,
        1,
        "",
        "Invalid data found\n",
    )]);
    let flac = TrackSelection {
        audio_codec: "A_FLAC".to_string(),
        ..selection()
    };

    let err = remux_with(dir.path(), &flac, &tokio::sync::Semaphore::new(1), &runner)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "Audio transcode: ffmpeg failed (exit code 1): Invalid data found"
    );
    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        1,
        "mkvmerge must not run after a failed transcode"
    );
    assert_eq!(calls[0].0, MediaTool::Ffmpeg);
    assert_eq!(runner.timeouts(), [SUBPROCESS_TIMEOUT]);
}
