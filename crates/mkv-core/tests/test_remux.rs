use std::path::PathBuf;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::fonts::FontInfo;
use mkv_core::remux::{
    build_ffmpeg_args, build_mkvmerge_args, select_tracks, PlannedJob, TrackSelection,
};
use mkv_core::scan::{InspectedVideo, MkvmergeMetadata};

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
    let inspected = InspectedVideo::new(meta);
    let selection = select_tracks(&inspected);
    assert_eq!(selection.video_id, Some(0));
    assert_eq!(selection.audio_id, Some(1));
    assert_eq!(selection.audio_codec, "A_FLAC");
}

#[test]
fn test_no_chapters_before_temp_opus_only() {
    let job = PlannedJob {
        video: PathBuf::from("/dummy/video.mkv"),
        video_name: "video.mkv".to_string(),
        base: "video".to_string(),
        output_file: PathBuf::from("/dummy/out.mkv"),
        subtitle_file: Some(PathBuf::from("/dummy/sub.ass")),
        subtitle_label: "English".to_string(),
        subtitle_mkv_lang: "eng".to_string(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "video.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "flac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Japanese".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let temp_opus = PathBuf::from("/dummy/audio.opus");
    let output_path = PathBuf::from("/dummy/out.mkv");

    let args = build_mkvmerge_args(
        &job,
        &selection,
        Some(&temp_opus),
        &fonts,
        &output_path,
        0.0,
    );

    let temp_opus_idx = args.iter().position(|a| a == "/dummy/audio.opus").unwrap();
    assert_eq!(
        &args[temp_opus_idx - 5..temp_opus_idx],
        &[
            "--no-chapters",
            "--language",
            "0:jpn",
            "--track-name",
            "0:Japanese"
        ]
    );

    let sub_idx = args.iter().position(|a| a == "/dummy/sub.ass").unwrap();
    assert!(!args[sub_idx - 6..sub_idx].contains(&"--no-chapters".to_string()));
}

#[test]
fn test_no_chapters_absent_when_no_opus() {
    let job = PlannedJob {
        video: PathBuf::from("/dummy/video.mkv"),
        video_name: "video.mkv".to_string(),
        base: "video".to_string(),
        output_file: PathBuf::from("/dummy/out.mkv"),
        subtitle_file: Some(PathBuf::from("/dummy/sub.ass")),
        subtitle_label: "Indonesian".to_string(),
        subtitle_mkv_lang: "ind".to_string(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "video.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let output_path = PathBuf::from("/dummy/out.mkv");

    let args = build_mkvmerge_args(&job, &selection, None, &fonts, &output_path, 0.0);
    let sub_idx = args.iter().position(|a| a == "/dummy/sub.ass").unwrap();
    assert!(!args[..sub_idx].contains(&"--no-chapters".to_string()));
}

#[test]
fn test_subtitle_sync_args() {
    let job = PlannedJob {
        video: PathBuf::from("/dummy/video.mkv"),
        video_name: "video.mkv".to_string(),
        base: "video".to_string(),
        output_file: PathBuf::from("/dummy/out.mkv"),
        subtitle_file: Some(PathBuf::from("/dummy/sub.ass")),
        subtitle_label: "Indonesian".to_string(),
        subtitle_mkv_lang: "ind".to_string(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "video.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let output_path = PathBuf::from("/dummy/out.mkv");

    let args_with_sync = build_mkvmerge_args(&job, &selection, None, &fonts, &output_path, 1.5);
    assert!(args_with_sync.contains(&"--sync".to_string()));
    assert!(args_with_sync.contains(&"0:1500".to_string()));

    let args_no_sync = build_mkvmerge_args(&job, &selection, None, &fonts, &output_path, 0.0);
    assert!(!args_no_sync.contains(&"--sync".to_string()));
}

#[test]
fn test_ffmpeg_args() {
    let video = PathBuf::from("/dummy/video.mkv");
    let temp_opus = PathBuf::from("/dummy/audio.opus");
    let args = build_ffmpeg_args(&video, 1, &temp_opus, "192k", 10, 4);

    assert_eq!(args[0], "-y");
    assert!(args.contains(&"-map".to_string()));
    assert!(args.contains(&"0:a:1".to_string()));
    assert!(args.contains(&"-c:a".to_string()));
    assert!(args.contains(&"libopus".to_string()));
    assert!(args.contains(&"-b:a".to_string()));
    assert!(args.contains(&"192k".to_string()));
    assert!(args.contains(&"-compression_level".to_string()));
    assert!(args.contains(&"10".to_string()));
}

#[test]
fn test_subtitle_extraction_filename_format() {
    use mkv_core::domain::EpisodeInfo;
    use mkv_core::extract::select_subtitle_for_extraction;
    use std::path::Path;

    let meta = MkvmergeMetadata {
        tracks: vec![MkvTrack {
            id: Some(2),
            track_type: "subtitles".to_string(),
            codec: None,
            properties: TrackProperties {
                codec_id: Some("s_text/ass".to_string()),
                language: Some("eng".to_string()),
                ..Default::default()
            },
        }],
        attachments: vec![],
        chapters: vec![],
    };
    let inspected = InspectedVideo::new(meta);
    let parsed = EpisodeInfo::new("1", "E", "01");
    let res =
        select_subtitle_for_extraction(&inspected, "Show - 01", Path::new("/out"), Some(&parsed));
    assert!(res.is_some());
    let (tid, path) = res.unwrap();
    assert_eq!(tid, 2);
    assert_eq!(
        path,
        Path::new("/out").join("Show - 01_EP01_eng_track2.ass")
    );
}

#[cfg(unix)]
static TEST_ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[cfg(unix)]
async fn test_execute_remux_job_collects_stdout_on_mkvmerge_failure() {
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tempfile::tempdir;

    let _lock = TEST_ENV_MUTEX.lock().await;

    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let mock_mkvmerge = bin_dir.path().join("mkvmerge");
    let script = "#!/bin/sh\necho 'Error: Simulated mkvmerge stdout failure'\nexit 2\n";
    std::fs::write(&mock_mkvmerge, script).unwrap();
    std::fs::set_permissions(&mock_mkvmerge, std::fs::Permissions::from_mode(0o755)).unwrap();

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.path().display(), orig_path);

    struct PathReset(String);
    impl Drop for PathReset {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }
    let _path_guard = PathReset(orig_path);
    std::env::set_var("PATH", new_path);

    let video = dir.path().join("test.mkv");
    File::create(&video).unwrap();
    let out = dir.path().join("out.mkv");

    let job = PlannedJob {
        video: video.clone(),
        video_name: "test.mkv".to_string(),
        base: "test".to_string(),
        output_file: out,
        subtitle_file: None,
        subtitle_label: String::new(),
        subtitle_mkv_lang: String::new(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "test.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let options = mkv_core::remux::RemuxOptions {
        opus_bitrate: "192k",
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
    };

    // Test with io_semaphore = None
    let res =
        mkv_core::remux::execute_remux_job(&job, &selection, &fonts, &options, None, None).await;

    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.contains("Simulated mkvmerge stdout failure"),
        "Error message did not contain stdout output: {err}"
    );
    assert!(
        err.contains("exit code 2"),
        "Error message did not contain exit code: {err}"
    );

    // Test with io_semaphore = Some(...)
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let res_with_sem =
        mkv_core::remux::execute_remux_job(&job, &selection, &fonts, &options, None, Some(&sem))
            .await;
    assert!(res_with_sem.is_err());
    let err_with_sem = res_with_sem.unwrap_err();
    assert!(err_with_sem.contains("Simulated mkvmerge stdout failure"));
}

#[tokio::test]
#[cfg(unix)]
async fn test_execute_remux_job_combines_stderr_and_stdout_on_failure() {
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let _lock = TEST_ENV_MUTEX.lock().await;

    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let mock_mkvmerge = bin_dir.path().join("mkvmerge");
    let script = "#!/bin/sh\necho 'Auxiliary stderr notice' >&2\necho 'Error: Detailed stdout failure'\nexit 2\n";
    std::fs::write(&mock_mkvmerge, script).unwrap();
    std::fs::set_permissions(&mock_mkvmerge, std::fs::Permissions::from_mode(0o755)).unwrap();

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.path().display(), orig_path);

    struct PathReset(String);
    impl Drop for PathReset {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }
    let _path_guard = PathReset(orig_path);
    std::env::set_var("PATH", new_path);

    let video = dir.path().join("test_both.mkv");
    File::create(&video).unwrap();
    let out = dir.path().join("out_both.mkv");

    let job = PlannedJob {
        video: video.clone(),
        video_name: "test_both.mkv".to_string(),
        base: "test_both".to_string(),
        output_file: out,
        subtitle_file: None,
        subtitle_label: String::new(),
        subtitle_mkv_lang: String::new(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "test_both.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let options = mkv_core::remux::RemuxOptions {
        opus_bitrate: "192k",
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
    };

    let res =
        mkv_core::remux::execute_remux_job(&job, &selection, &fonts, &options, None, None).await;

    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.contains("Auxiliary stderr notice"),
        "Error message did not contain stderr output: {err}"
    );
    assert!(
        err.contains("Error: Detailed stdout failure"),
        "Error message did not contain stdout output: {err}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_execute_remux_job_logs_warnings_on_exit_code_1() {
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let _lock = TEST_ENV_MUTEX.lock().await;

    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let mock_mkvmerge = bin_dir.path().join("mkvmerge");
    let script =
        "#!/bin/sh\necho 'Warning: QuickTime track parsing'\necho 'dummy data' > \"$2\"\nexit 1\n";
    std::fs::write(&mock_mkvmerge, script).unwrap();
    std::fs::set_permissions(&mock_mkvmerge, std::fs::Permissions::from_mode(0o755)).unwrap();

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.path().display(), orig_path);

    struct PathReset(String);
    impl Drop for PathReset {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }
    let _path_guard = PathReset(orig_path);
    std::env::set_var("PATH", new_path);

    let video = dir.path().join("test_warn.mkv");
    File::create(&video).unwrap();
    let out = dir.path().join("out_warn.mkv");

    let job = PlannedJob {
        video: video.clone(),
        video_name: "test_warn.mkv".to_string(),
        base: "test_warn".to_string(),
        output_file: out.clone(),
        subtitle_file: None,
        subtitle_label: String::new(),
        subtitle_mkv_lang: String::new(),
        chapter_file: None,
        cover_file: None,
        embedded_sub_id: None,
        use_source_fonts: false,
        release_dir: None,
        item_id: "test_warn.mkv".to_string(),
    };

    let selection = TrackSelection {
        video_id: Some(0),
        audio_id: Some(1),
        audio_index: 0,
        audio_codec: "aac".to_string(),
        audio_channels: 2,
        audio_lang: "jpn".to_string(),
        audio_name: "Stereo".to_string(),
        audio_reason: "test".to_string(),
        has_source_chapters: false,
        source_cover_id: None,
        source_font_ids: vec![],
    };

    let fonts = FontInfo::default();
    let options = mkv_core::remux::RemuxOptions {
        opus_bitrate: "192k",
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
    };

    let res =
        mkv_core::remux::execute_remux_job(&job, &selection, &fonts, &options, None, None).await;

    assert!(
        res.is_ok(),
        "Expected success on exit code 1 with output file created: {:?}",
        res.err()
    );
    let summary = res.unwrap();
    assert!(
        summary
            .log
            .iter()
            .any(|l| l.contains("Warning: QuickTime track parsing")),
        "Summary log did not contain warning: {:?}",
        summary.log
    );
}
