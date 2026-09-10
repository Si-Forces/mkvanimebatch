use std::fs;

use mkv_core::domain::{AudioTrack, MkvTrack, TrackProperties};
use mkv_core::scan::{
    is_chapter_xml, is_cover_attachment, is_cover_filename, is_font_attachment, scan_assets,
    scan_mkv_files, InspectedVideo, MkvAttachment, MkvChapter, MkvmergeMetadata,
    PersistentInspectCache,
};
use tempfile::tempdir;

#[test]
fn test_is_chapter_xml() {
    let tmp = tempdir().unwrap();
    let chap = tmp.path().join("chap.xml");
    fs::write(
        &chap,
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();
    assert!(is_chapter_xml(&chap));

    let not_chap = tmp.path().join("not_chap.txt");
    fs::write(&not_chap, b"Just regular text").unwrap();
    assert!(!is_chapter_xml(&not_chap));
}

#[test]
fn test_is_cover_filename() {
    assert!(is_cover_filename("cover.jpg"));
    assert!(is_cover_filename("COVER.PNG"));
    assert!(is_cover_filename("Cover.webp"));
    assert!(!is_cover_filename("other.jpg"));
    assert!(!is_cover_filename("cover.txt"));
}

#[test]
fn test_is_cover_attachment() {
    assert!(is_cover_attachment("cover.jpg", "image/jpeg"));
    assert!(is_cover_attachment("poster.png", "image/png"));
    assert!(is_cover_attachment("thumbnail.webp", "image/webp"));
    assert!(!is_cover_attachment(
        "font.ttf",
        "application/x-truetype-font"
    ));
    assert!(!is_cover_attachment("poster.png", "text/plain"));
}

#[test]
fn test_is_font_attachment() {
    assert!(is_font_attachment(
        "arial.ttf",
        "application/x-truetype-font"
    ));
    assert!(is_font_attachment(
        "font.otf",
        "application/vnd.ms-opentype"
    ));
    assert!(is_font_attachment(
        "unknown_ext",
        "application/x-font-truetype"
    ));
    assert!(!is_font_attachment("image.png", "image/png"));
}

#[test]
fn test_inspected_video_properties() {
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
                    track_name: Some("Main Audio".to_string()),
                    audio_channels: Some(2),
                    codec_id: Some("A_AAC".to_string()),
                    ..Default::default()
                },
            },
            MkvTrack {
                id: Some(2),
                track_type: "subtitles".to_string(),
                codec: None,
                properties: TrackProperties {
                    language: Some("ind".to_string()),
                    track_name: Some("Full Dialogue".to_string()),
                    codec_id: Some("S_TEXT/ASS".to_string()),
                    ..Default::default()
                },
            },
        ],
        attachments: vec![
            MkvAttachment {
                id: Some(1),
                file_name: Some("cover.jpg".to_string()),
                content_type: Some("image/jpeg".to_string()),
                size: Some(1234),
            },
            MkvAttachment {
                id: Some(2),
                file_name: Some("font.ttf".to_string()),
                content_type: Some("application/x-truetype-font".to_string()),
                size: Some(5678),
            },
        ],
        chapters: vec![MkvChapter {
            num_entries: Some(5),
        }],
    };

    let inspected = InspectedVideo::new(meta);
    assert_eq!(inspected.tracks().len(), 3);
    assert_eq!(inspected.video_track_pos(), 0);
    assert!(inspected.video_track().is_some());

    let audios = inspected.audio_tracks();
    assert_eq!(audios.len(), 1);
    assert_eq!(
        audios[0],
        AudioTrack {
            id: 1,
            pos: 1,
            audio_idx: 0,
            lang: "jpn".to_string(),
            channels: 2,
            name: "Main Audio".to_string(),
            codec: "A_AAC".to_string(),
        }
    );

    let subs = inspected.subtitle_tracks();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].id, Some(2));

    assert!(inspected.has_chapters());

    let cover = inspected.cover_attachment();
    assert!(cover.is_some());
    assert_eq!(cover.unwrap().file_name, Some("cover.jpg".to_string()));

    let fonts = inspected.font_attachments();
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].file_name, Some("font.ttf".to_string()));
    assert_eq!(inspected.fonts_count(), Some(1));
}

#[test]
fn test_persistent_inspect_cache() {
    let tmp = tempdir().unwrap();
    let cache_file = tmp.path().join("inspect_cache.json");
    let video_path = tmp.path().join("test_video.mkv");
    fs::write(&video_path, b"dummy video content").unwrap();

    let meta = MkvmergeMetadata {
        tracks: vec![MkvTrack {
            id: Some(0),
            track_type: "video".to_string(),
            codec: None,
            properties: TrackProperties::default(),
        }],
        attachments: vec![],
        chapters: vec![],
    };

    {
        let mut cache = PersistentInspectCache::new(Some(cache_file.clone()), true);
        cache.put_path(&video_path, meta.clone());
        cache.save();
    }

    {
        let cache2 = PersistentInspectCache::new(Some(cache_file), true);
        let retrieved = cache2.get_path(&video_path);
        assert_eq!(retrieved, Some(meta));
    }
}

#[test]
fn test_scan_assets_grouping() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("release");
    fs::create_dir_all(&dir).unwrap();

    fs::write(dir.join("Show - 01.mkv"), b"video").unwrap();
    fs::write(dir.join("Show - 01.ass"), b"sub").unwrap();
    fs::write(
        dir.join("Show - 01_chapters.xml"),
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();
    fs::write(dir.join("cover.jpg"), b"cover").unwrap();
    fs::write(dir.join("ignored.txt"), b"ignored").unwrap();

    let scan = scan_assets(&dir, None);
    assert_eq!(scan.videos.len(), 1);
    assert_eq!(scan.videos[0].name, "Show - 01.mkv");

    assert_eq!(scan.subs.len(), 1);
    assert_eq!(scan.subs[0].name, "Show - 01.ass");

    assert_eq!(scan.chapters.len(), 1);
    assert_eq!(scan.chapters[0].name, "Show - 01_chapters.xml");

    assert_eq!(scan.covers.len(), 1);
    assert_eq!(scan.covers[0].name, "cover.jpg");
}

#[test]
fn test_scan_mkv_files_recursive_and_ignore() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season1 = root.join("Season 01");
    let output_dir = root.join("output");
    let fonts_dir = root.join("fonts");

    fs::create_dir_all(&season1).unwrap();
    fs::create_dir_all(&output_dir).unwrap();
    fs::create_dir_all(&fonts_dir).unwrap();

    fs::write(root.join("root_video.mkv"), b"vid").unwrap();
    fs::write(season1.join("s01_video.mkv"), b"vid").unwrap();
    fs::write(output_dir.join("ignored_output.mkv"), b"vid").unwrap();
    fs::write(fonts_dir.join("ignored_font.mkv"), b"vid").unwrap();

    let non_rec = scan_mkv_files(&root, false, None);
    assert_eq!(non_rec.len(), 1);
    assert_eq!(non_rec[0].name, "root_video.mkv");

    let rec = scan_mkv_files(&root, true, None);
    assert_eq!(rec.len(), 2);
    let names: Vec<&str> = rec.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"root_video.mkv"));
    assert!(names.contains(&"s01_video.mkv"));
    assert!(!names.contains(&"ignored_output.mkv"));
    assert!(!names.contains(&"ignored_font.mkv"));
}

#[test]
fn test_probe_video_fallback() {
    let tmp = tempdir().unwrap();
    let corrupt_mkv = tmp.path().join("corrupt.mkv");
    fs::write(&corrupt_mkv, b"not a real ebml file").unwrap();
    assert!(mkv_core::scan::probe_video(&corrupt_mkv).is_err());
}

#[test]
fn test_probe_ebml_8byte_vint() {
    let tmp = tempdir().unwrap();
    let test_mkv = tmp.path().join("eight_byte_vint.mkv");
    // EBML header (0x1A45DFA3 + length 0) + Segment (0x18538067 + 8-byte VINT length)
    let mut data = vec![
        0x1A, 0x45, 0xDF, 0xA3, 0x80, // EBML Header, len 0
        0x18, 0x53, 0x80, 0x67, // Segment ID
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, // 8-byte VINT length (16 bytes)
    ];
    data.extend(vec![0u8; 16]); // Payload
    fs::write(&test_mkv, &data).unwrap();
    // Must not panic with bit-shift overflow
    let res = mkv_core::ebml::probe_ebml_native(&test_mkv);
    assert!(res.is_err()); // Will fail finding tracks, but must NOT panic!
}
