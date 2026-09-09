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
    assert_eq!(path.to_string_lossy(), "/out/Show - 01_EP01_eng_track2.ass");
}
