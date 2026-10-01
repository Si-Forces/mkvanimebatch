use std::path::{Path, PathBuf};

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::fonts::FontInfo;
use mkv_core::remux::{
    build_ffmpeg_args, build_mkvmerge_args, execute_remux_job, select_tracks, PlannedJob,
    RemuxOptions, TrackSelection,
};
use mkv_core::scan::{InspectedVideo, MkvmergeMetadata};

fn job(video: &Path, output_file: PathBuf, subtitle_file: Option<PathBuf>) -> PlannedJob {
    let video_name = video.file_name().unwrap().to_string_lossy().into_owned();
    PlannedJob {
        video: video.to_path_buf(),
        item_id: video_name.clone(),
        video_name,
        output_file,
        subtitle_file,
        subtitle_label: "Default English".to_string(),
        subtitle_mkv_lang: "eng".to_string(),
        chapter_file: None,
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

fn mkvmerge_args(job: &PlannedJob, temp_opus: Option<&Path>, sub_offset: f64) -> Vec<String> {
    build_mkvmerge_args(
        job,
        &selection(),
        temp_opus,
        &FontInfo::default(),
        Path::new("/dummy/out.mkv"),
        sub_offset,
    )
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|pair| pair == [flag, value])
}

const REMUX_OPTIONS: RemuxOptions<'static> = RemuxOptions {
    opus_bitrate: "192k",
    complexity: 10,
    ffmpeg_threads: 1,
    sub_offset: 0.0,
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

#[test]
fn test_no_chapters_binding() {
    let mut job_with_chapters = subtitle_job();
    job_with_chapters.chapter_file = Some(PathBuf::from("/dummy/chapters.xml"));

    let args_with_chapters = mkvmerge_args(
        &job_with_chapters,
        Some(Path::new("/dummy/audio.opus")),
        0.0,
    );
    let no_chapters_count = args_with_chapters
        .iter()
        .filter(|&a| a == "--no-chapters")
        .count();
    assert_eq!(no_chapters_count, 1);

    let no_chapters_idx = args_with_chapters
        .iter()
        .position(|a| a == "--no-chapters")
        .unwrap();
    let video_idx = args_with_chapters
        .iter()
        .position(|a| a == job_with_chapters.video.to_str().unwrap())
        .unwrap();
    assert!(no_chapters_idx < video_idx);

    let job_without_chapters = subtitle_job();
    let args_without_chapters = mkvmerge_args(
        &job_without_chapters,
        Some(Path::new("/dummy/audio.opus")),
        0.0,
    );
    let no_chapters_count_absent = args_without_chapters
        .iter()
        .filter(|&a| a == "--no-chapters")
        .count();
    assert_eq!(no_chapters_count_absent, 0);
}

#[test]
fn test_no_chapters_absent_when_no_opus() {
    let args = mkvmerge_args(&subtitle_job(), None, 0.0);
    let sub_idx = args.iter().position(|a| a == "/dummy/sub.ass").unwrap();
    assert!(!args[..sub_idx].contains(&"--no-chapters".to_string()));
}

#[test]
fn test_subtitle_sync_args() {
    let job = subtitle_job();
    assert!(has_pair(
        &mkvmerge_args(&job, None, 1.5),
        "--sync",
        "0:1500"
    ));
    assert!(!mkvmerge_args(&job, None, 0.0).contains(&"--sync".to_string()));
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

    // A custom offset overrides the global sub_offset instead of adding to it.
    let args_zero_offset = mkvmerge_args(&job, None, 0.0);
    assert!(has_pair(&args_zero_offset, "--sync", "0:750"));
    assert!(has_pair(&args_zero_offset, "--language", "0:ind"));
    assert!(has_pair(
        &args_zero_offset,
        "--track-name",
        "0:Custom Translation"
    ));

    let args_diff_offset = mkvmerge_args(&job, None, 2.0);
    assert!(has_pair(&args_diff_offset, "--sync", "0:750"));
    assert!(!args_diff_offset.contains(&"0:2000".to_string()));

    // An explicit zero custom offset leaves the subtitle unshifted.
    job.custom_offset_ms = Some(0);
    assert!(!mkvmerge_args(&job, None, 2.0).contains(&"--sync".to_string()));

    // Without custom fields the global offset and resolved label/language apply.
    job.custom_offset_ms = None;
    job.custom_language = None;
    job.custom_title = None;

    let args_fallback = mkvmerge_args(&job, None, 1.25);
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
        None,
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("Output path has no file name"),
        "Unexpected error message: {err}"
    );
}

#[cfg(unix)]
mod mock_mkvmerge {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;
    use tokio::sync::{Mutex, MutexGuard};

    use super::*;

    /// Serializes the `PATH` mutation shared by every mock-mkvmerge test in this binary.
    static PATH_LOCK: Mutex<()> = Mutex::const_new(());

    /// Puts an `mkvmerge` shell script ahead of the real one on `PATH` until dropped.
    struct MockMkvmerge {
        original_path: String,
        _bin_dir: TempDir,
        _lock: MutexGuard<'static, ()>,
    }

    impl MockMkvmerge {
        async fn install(script: &str) -> Self {
            let lock = PATH_LOCK.lock().await;
            let bin_dir = tempfile::tempdir().unwrap();
            let mock = bin_dir.path().join("mkvmerge");
            std::fs::write(&mock, script).unwrap();
            std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o755)).unwrap();

            let original_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var(
                "PATH",
                format!("{}:{original_path}", bin_dir.path().display()),
            );
            Self {
                original_path,
                _bin_dir: bin_dir,
                _lock: lock,
            }
        }
    }

    impl Drop for MockMkvmerge {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.original_path);
        }
    }

    /// Remuxes an empty source video with no subtitle through the installed mock.
    ///
    /// Takes a `&Path` rather than a `&TempDir` so callers can move the directory into a
    /// spawned task. `io_semaphore` is threaded through rather than hardcoded so both sides
    /// of the ADR-0011 dual-governor branch are exercised: the product only ever passes
    /// `Some`.
    async fn remux_with(
        dir: &Path,
        io_semaphore: Option<&tokio::sync::Semaphore>,
    ) -> Result<Vec<String>, String> {
        let video = dir.join("test.mkv");
        std::fs::File::create(&video).unwrap();
        let out = dir.join("out.mkv");
        execute_remux_job(
            &job(&video, out, None),
            &selection(),
            &FontInfo::default(),
            &REMUX_OPTIONS,
            None,
            io_semaphore,
        )
        .await
    }

    async fn remux(dir: &TempDir) -> Result<Vec<String>, String> {
        remux_with(dir.path(), None).await
    }

    #[tokio::test]
    async fn test_execute_remux_job_collects_stdout_on_mkvmerge_failure() {
        let _mock = MockMkvmerge::install(
            "#!/bin/sh\necho 'Error: Simulated mkvmerge stdout failure'\nexit 2\n",
        )
        .await;
        let dir = tempfile::tempdir().unwrap();

        let err = remux(&dir).await.unwrap_err();
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
        let _mock = MockMkvmerge::install(
            "#!/bin/sh\necho 'Auxiliary stderr notice' >&2\necho 'Error: Detailed stdout failure'\nexit 2\n",
        )
        .await;
        let dir = tempfile::tempdir().unwrap();

        let err = remux(&dir).await.unwrap_err();
        assert!(
            err.contains("Auxiliary stderr notice\nError: Detailed stdout failure"),
            "Error message did not join stderr and stdout: {err}"
        );
    }

    #[tokio::test]
    async fn test_execute_remux_job_logs_warnings_on_exit_code_1() {
        let _mock = MockMkvmerge::install(
            "#!/bin/sh\necho 'Warning: QuickTime track parsing'\necho 'dummy data' > \"$2\"\nexit 1\n",
        )
        .await;
        let dir = tempfile::tempdir().unwrap();

        let log = remux(&dir).await.unwrap();
        assert!(
            log.iter()
                .any(|l| l == "  Warning: QuickTime track parsing"),
            "log did not contain the warning: {log:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("out.mkv")).unwrap(),
            b"dummy data\n"
        );
    }

    /// The product always passes `Some(&io_semaphore)` (pipeline.rs), so the governor's
    /// acquire branch is the one that runs in production. It needs coverage that would fail
    /// if acquisition were skipped: simply passing `Some` and checking the result is not
    /// enough, because an ignored semaphore still produces the output and still reports an
    /// untouched permit count. So the governor is exhausted first and the mock is observed
    /// not running until a permit is released.
    #[tokio::test]
    async fn test_execute_remux_job_waits_for_an_io_governor_permit() {
        let _mock = MockMkvmerge::install("#!/bin/sh\necho 'dummy data' > \"$2\"\nexit 0\n").await;
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let job_semaphore = std::sync::Arc::clone(&semaphore);

        let mut job =
            tokio::spawn(async move { remux_with(&dir_path, Some(&job_semaphore)).await });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut job)
                .await
                .is_err(),
            "mkvmerge ran while the I/O governor had no permits available"
        );
        assert!(
            !dir.path().join("out.mkv").exists(),
            "mkvmerge wrote its output before acquiring an I/O permit"
        );

        semaphore.add_permits(1);
        let log = job.await.unwrap().unwrap();

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

    /// A closed governor is an invariant violation, not a "carry on unguarded" case.
    /// Bypassing it would silently remove the ADR-0011 disk-write limit mid-batch.
    #[tokio::test]
    #[should_panic(expected = "io semaphore closed")]
    async fn test_execute_remux_job_refuses_a_closed_io_governor() {
        let _mock = MockMkvmerge::install("#!/bin/sh\nexit 0\n").await;
        let dir = tempfile::tempdir().unwrap();
        let semaphore = tokio::sync::Semaphore::new(1);
        semaphore.close();

        let _ = remux_with(dir.path(), Some(&semaphore)).await;
    }
}
