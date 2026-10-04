use std::collections::HashSet;
use std::fs;

use mkv_core::domain::{AudioTrack, MkvTrack, TrackProperties};
use mkv_core::scan::{
    is_cover_attachment, is_cover_filename, is_font_attachment, scan_assets, scan_mkv_files,
    FileSignature, InspectedVideo, MkvAttachment, MkvChapter, MkvmergeMetadata,
    PersistentInspectCache,
};
use tempfile::tempdir;

/// Minimal EBML header followed by a Segment ID; the Segment size VINT comes next.
const EBML_HEADER_AND_SEGMENT_ID: [u8; 9] = [0x1A, 0x45, 0xDF, 0xA3, 0x80, 0x18, 0x53, 0x80, 0x67];

fn track(id: u32, track_type: &str) -> MkvTrack {
    MkvTrack {
        id: Some(id),
        track_type: track_type.to_string(),
        codec: None,
        properties: TrackProperties::default(),
    }
}

#[test]
fn test_is_cover_filename() {
    assert!(is_cover_filename("cover.jpg"));
    assert!(is_cover_filename("COVER.PNG"));
    assert!(is_cover_filename("Cover.webp"));
    assert!(is_cover_filename("folder.jpg"));
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
            track(0, "video"),
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

    assert_eq!(
        inspected
            .cover_attachment()
            .and_then(|c| c.file_name.as_deref()),
        Some("cover.jpg")
    );

    let fonts = inspected.font_attachments();
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].file_name.as_deref(), Some("font.ttf"));
}

#[test]
fn test_persistent_inspect_cache_save_shared_debouncing_and_direct_save() {
    let tmp = tempdir().unwrap();
    let cache_file = tmp.path().join("inspect_cache.json");
    let video_path1 = tmp.path().join("test_video1.mkv");
    let video_path2 = tmp.path().join("test_video2.mkv");
    fs::write(&video_path1, b"dummy content 1").unwrap();
    fs::write(&video_path2, b"dummy content 2").unwrap();

    let meta1 = MkvmergeMetadata {
        tracks: vec![track(0, "video")],
        ..Default::default()
    };
    let meta2 = MkvmergeMetadata {
        tracks: vec![track(1, "audio")],
        ..Default::default()
    };

    let cache = PersistentInspectCache::new(Some(cache_file.clone()), true);

    // 1. First save_shared() writes to disk (no prior last_save_ts).
    cache.put_path(&video_path1, meta1.clone());
    cache.save_shared();

    let disk_content = fs::read_to_string(&cache_file).unwrap();
    assert!(disk_content.contains("test_video1.mkv"));
    assert!(!disk_content.contains("test_video2.mkv"));

    // 2. Intermediate save_shared() within 500ms debounces and does NOT flush to disk.
    cache.put_path(&video_path2, meta2.clone());
    cache.save_shared();

    let disk_content_after_debounce = fs::read_to_string(&cache_file).unwrap();
    assert!(!disk_content_after_debounce.contains("test_video2.mkv"));

    // 3. Direct save() immediately flushes regardless of the debounce interval.
    cache.save();

    let disk_content_after_save = fs::read_to_string(&cache_file).unwrap();
    assert!(disk_content_after_save.contains("test_video1.mkv"));
    assert!(disk_content_after_save.contains("test_video2.mkv"));

    // 4. Data persists correctly to disk and reloads on startup.
    let reloaded_cache = PersistentInspectCache::new(Some(cache_file), true);
    assert_eq!(reloaded_cache.get_path(&video_path1), Some(meta1));
    assert_eq!(reloaded_cache.get_path(&video_path2), Some(meta2));
}

/// Cache files written by earlier versions carry fields the current entry no longer has.
#[test]
fn test_persistent_inspect_cache_loads_entries_with_legacy_fields() {
    let tmp = tempdir().unwrap();
    let cache_file = tmp.path().join("inspect_cache.json");
    let video_path = tmp.path().join("video.mkv");
    fs::write(&video_path, b"v").unwrap();
    let meta = MkvmergeMetadata {
        tracks: vec![track(0, "video")],
        ..Default::default()
    };

    let cache = PersistentInspectCache::new(Some(cache_file.clone()), true);
    cache.put_path(&video_path, meta.clone());
    cache.save();

    let mut on_disk: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cache_file).unwrap()).unwrap();
    for entry in on_disk.as_object_mut().unwrap().values_mut() {
        entry["mtime"] = serde_json::json!(1.5);
    }
    fs::write(&cache_file, on_disk.to_string()).unwrap();

    let reloaded = PersistentInspectCache::new(Some(cache_file), true);
    assert_eq!(reloaded.get_path(&video_path), Some(meta));
}

#[test]
fn test_persistent_inspect_cache_save_shared_flushes_after_interval() {
    let tmp = tempdir().unwrap();
    let cache_file = tmp.path().join("inspect_cache.json");
    let video_path1 = tmp.path().join("video1.mkv");
    let video_path2 = tmp.path().join("video2.mkv");
    fs::write(&video_path1, b"v1").unwrap();
    fs::write(&video_path2, b"v2").unwrap();

    let meta = MkvmergeMetadata::default();
    let cache = PersistentInspectCache::new(Some(cache_file.clone()), true);

    cache.put_path(&video_path1, meta.clone());
    cache.save_shared();
    assert!(fs::read_to_string(&cache_file)
        .unwrap()
        .contains("video1.mkv"));

    cache.put_path(&video_path2, meta);
    // Sleep to exceed the 500ms debounce interval
    std::thread::sleep(std::time::Duration::from_millis(520));
    cache.save_shared();

    let disk_content = fs::read_to_string(&cache_file).unwrap();
    assert!(disk_content.contains("video1.mkv"));
    assert!(disk_content.contains("video2.mkv"));
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
    fs::write(dir.join("Show - 01.png"), b"episode cover").unwrap();
    fs::write(dir.join("ignored.txt"), b"ignored").unwrap();

    let scan = scan_assets(&dir, None);

    assert_eq!(scan.subs.len(), 1);
    assert_eq!(scan.subs[0].name, "Show - 01.ass");

    assert_eq!(scan.chapters.len(), 1);
    assert_eq!(scan.chapters[0].name, "Show - 01_chapters.xml");

    // Videos are not reported, but the video's stem makes its bare-stem image an Episode Cover.
    assert_eq!(scan.covers.len(), 2);
    assert_eq!(scan.covers[0].name, "cover.jpg");
    assert_eq!(scan.covers[1].name, "Show - 01.png");
}

/// A directory reached through a second spelling gets the first spelling's results back, so
/// asset paths, and the subtitle IDs derived from them, keep that spelling (#32).
#[cfg(unix)]
#[test]
fn test_scan_assets_cache_keeps_first_spelling() {
    use mkv_core::scan::ScanCache;
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let real = tmp.path().join("assets");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("Show - 01.ass"), b"sub").unwrap();
    let link = tmp.path().join("assets_link");
    symlink(&real, &link).unwrap();

    let mut cache = ScanCache::default();
    let first = scan_assets(&real, Some(&mut cache));
    let second = scan_assets(&link, Some(&mut cache));

    assert_eq!(first.subs[0].path, real.join("Show - 01.ass"));
    assert_eq!(second.subs[0].path, real.join("Show - 01.ass"));
}

/// Entries are kept when they are, or link to, a regular file; a symlink to a directory and a
/// broken symlink are skipped.
#[cfg(unix)]
#[test]
fn test_scan_assets_symlink_entries() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("release");
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(elsewhere.join("Show - 02.ass")).unwrap();
    fs::write(elsewhere.join("Show - 01.ass"), b"sub").unwrap();
    symlink(elsewhere.join("Show - 01.ass"), dir.join("Show - 01.ass")).unwrap();
    symlink(elsewhere.join("Show - 02.ass"), dir.join("Show - 02.ass")).unwrap();
    symlink(elsewhere.join("missing.ass"), dir.join("Show - 03.ass")).unwrap();

    let scan = scan_assets(&dir, None);
    let names: Vec<_> = scan.subs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Show - 01.ass"]);
}

#[test]
fn test_scan_assets_cover_names_require_applicable_video() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    for name in [
        "Show - 01.mkv",
        "unknown.MKV",
        "cover.jpg",
        "folder.PNG",
        "Show - 01_cover.jpg",
        "Show - 01.cover.webp",
        "Show - 01.jpeg",
        "unknown.png",
        "other.jpeg",
        "logo_cover.jpg",
        "poster.png",
        "Show - 02.cover.jpg",
        "Show - 01_cover.txt",
    ] {
        fs::write(dir.join(name), b"sample").unwrap();
    }
    let scan = scan_assets(dir, None);
    let names: HashSet<&str> = scan
        .covers
        .iter()
        .map(|cover| cover.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "cover.jpg",
            "folder.PNG",
            "Show - 01_cover.jpg",
            "Show - 01.cover.webp",
            "Show - 01.jpeg",
            "unknown.png",
            "Show - 02.cover.jpg"
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn test_scan_mkv_files_recursive_skips_output_and_font_dirs() {
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
    let names: HashSet<&str> = rec.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, HashSet::from(["root_video.mkv", "s01_video.mkv"]));
}

#[tokio::test]
async fn test_probe_video_rejects_non_matroska_file() {
    let tmp = tempdir().unwrap();
    let corrupt_mkv = tmp.path().join("corrupt.mkv");
    fs::write(&corrupt_mkv, b"not a real ebml file").unwrap();
    assert!(mkv_core::scan::probe_video(&corrupt_mkv).await.is_err());
}

/// A stalled native read must not block the runtime (#20). `open()` on a FIFO blocks until a
/// writer appears, standing in for a cold or hung device.
#[cfg(unix)]
#[test]
fn test_probe_video_stalled_native_read_does_not_block_runtime() {
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp = tempdir().unwrap();
    let fifo = tmp.path().join("stalled.mkv");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success());

    // The runtime runs on its own thread so this thread can watchdog it: an inline blocking
    // probe would stall the runtime and a #[tokio::test] would hang rather than fail.
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let probe_path = fifo.clone();
    let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let _probe =
                tokio::spawn(async move { mkv_core::scan::probe_video(&probe_path).await });
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(()).unwrap();
        });
        // Keep the runtime alive until the writer has opened: shutting it down would cancel a
        // blocking probe that has not started yet.
        let _ = release_rx.recv();
    });

    let ticked = rx.recv_timeout(Duration::from_secs(5)).is_ok();
    // Unblock the reader so the probe thread exits either way. The writer's open() returns only
    // once the probe holds the read end, so it also proves the native read was reached.
    let (opened_tx, opened_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let writer = fs::OpenOptions::new().write(true).open(&fifo);
        opened_tx.send(writer.map(drop)).unwrap();
    });
    opened_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("probe_video never opened the FIFO, so the stall was not exercised")
        .unwrap();
    drop(release_tx);
    worker.join().unwrap();
    assert!(
        ticked,
        "runtime timer stalled behind a blocked native probe"
    );
}

#[test]
fn test_probe_ebml_8byte_vint_segment_size() {
    let tmp = tempdir().unwrap();
    let test_mkv = tmp.path().join("eight_byte_vint.mkv");
    let mut data = EBML_HEADER_AND_SEGMENT_ID.to_vec();
    data.extend([0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10]); // 8-byte VINT: 16 bytes
    data.extend([0u8; 16]);
    fs::write(&test_mkv, &data).unwrap();
    // The segment holds no tracks; decoding the 8-byte length must not overflow a shift.
    assert!(mkv_core::ebml::probe_ebml_native(&test_mkv).is_err());
}

#[test]
fn test_unknown_size_vint_is_rejected_by_native_probe() {
    let tmp = tempdir().unwrap();

    // RFC 8794 §5.1.1: a VINT whose data bits are all 1 encodes an unknown size.
    let unknown_size_vints: [&[u8]; 2] =
        [&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], &[0xFF]];
    for (i, vint) in unknown_size_vints.into_iter().enumerate() {
        let test_mkv = tmp.path().join(format!("unknown_vint_{i}.mkv"));
        let mut data = EBML_HEADER_AND_SEGMENT_ID.to_vec();
        data.extend_from_slice(vint);
        data.extend([0u8; 32]);
        fs::write(&test_mkv, &data).unwrap();

        let ebml_res = mkv_core::ebml::probe_ebml_native(&test_mkv);
        assert!(
            ebml_res
                .as_ref()
                .is_err_and(|e| e.contains("Unknown EBML element length")),
            "Expected unknown length error for {vint:02X?}, got: {ebml_res:?}"
        );
    }
}

/// Wraps `tracks_payload` in a Tracks element inside a Segment.
fn mkv_with_tracks_payload(tracks_payload: &[u8]) -> Vec<u8> {
    let mut tracks_elem = vec![0x16, 0x54, 0xAE, 0x6B]; // Tracks ID
    tracks_elem.push(0x80 | (tracks_payload.len() as u8));
    tracks_elem.extend_from_slice(tracks_payload);

    let mut data = EBML_HEADER_AND_SEGMENT_ID.to_vec();
    data.push(0x80 | (tracks_elem.len() as u8));
    data.extend_from_slice(&tracks_elem);
    data
}

/// TrackEntry (length 6) with TrackNumber = 1 and TrackType = 1 (video).
const VALID_VIDEO_TRACK_ENTRY: [u8; 8] = [0xAE, 0x86, 0xD7, 0x81, 0x01, 0x83, 0x81, 0x01];

#[test]
fn test_corrupted_track_entry_fails_native_probe() {
    let tmp = tempdir().unwrap();
    let corrupt_tracks_mkv = tmp.path().join("corrupt_tracks.mkv");

    let mut tracks_payload = VALID_VIDEO_TRACK_ENTRY.to_vec();
    // TrackEntry (length 4) whose only child has an invalid zero EBML ID.
    tracks_payload.extend([0xAE, 0x84, 0x00, 0x00, 0x00, 0x00]);
    fs::write(
        &corrupt_tracks_mkv,
        mkv_with_tracks_payload(&tracks_payload),
    )
    .unwrap();

    // Partial tracks must not be returned; failing hands the file to mkvmerge instead.
    let ebml_res = mkv_core::ebml::probe_ebml_native(&corrupt_tracks_mkv);
    assert!(
        ebml_res.is_err(),
        "Native probe should have failed on corrupted track entry, but returned: {ebml_res:?}"
    );
}

#[test]
fn test_truncated_tracks_block_fails_native_probe() {
    let tmp = tempdir().unwrap();
    let truncated_mkv = tmp.path().join("truncated_tracks.mkv");

    let mut tracks_payload = VALID_VIDEO_TRACK_ENTRY.to_vec();
    // A trailing element ID with no size: the Tracks element ends mid-element.
    tracks_payload.push(0xAE);
    fs::write(&truncated_mkv, mkv_with_tracks_payload(&tracks_payload)).unwrap();

    let ebml_res = mkv_core::ebml::probe_ebml_native(&truncated_mkv);
    assert!(
        ebml_res.is_err(),
        "Native probe should have failed on truncated track entry, but returned: {ebml_res:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_scan_mkv_files_symlink_cycle() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let season = tmp.path().join("Season 1");
    fs::create_dir(&season).unwrap();

    let mkv = season.join("ep01.mkv");
    fs::write(&mkv, b"dummy").unwrap();

    let extras = season.join("extras");
    fs::create_dir(&extras).unwrap();
    let extra_mkv = extras.join("extra.mkv");
    fs::write(&extra_mkv, b"dummy").unwrap();

    // Season 1/cycle -> Season 1
    symlink(&season, season.join("cycle")).unwrap();
    // Season 1/link_extras -> Season 1/extras
    symlink(&extras, season.join("link_extras")).unwrap();

    // The walk must terminate and must not double-count extra.mkv.
    let results = scan_mkv_files(tmp.path(), true, None);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].name, "ep01.mkv");
    assert_eq!(results[1].name, "extra.mkv");
}

#[cfg(unix)]
#[test]
fn test_scan_mkv_files_symlink_file_duplicate() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let mkv = tmp.path().join("ep01.mkv");
    fs::write(&mkv, b"dummy").unwrap();
    symlink(&mkv, tmp.path().join("ep01_link.mkv")).unwrap();

    let results = scan_mkv_files(tmp.path(), false, None);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].name == "ep01.mkv" || results[0].name == "ep01_link.mkv",
        "Unexpected name: {}",
        results[0].name
    );
}

/// A symlinked walk root (as macOS gives every `/tmp` tempdir, via `/tmp -> /private/tmp`)
/// must not defeat symlink-alias dedup, or the same episode reaches the pipeline twice.
#[cfg(unix)]
#[test]
fn test_scan_mkv_files_symlink_dedup_under_non_canonical_root() {
    use std::os::unix::fs::symlink;

    let base = tempdir().unwrap();
    let real = base.path().join("real");
    let season = real.join("Season 1");
    fs::create_dir_all(&season).unwrap();
    fs::write(season.join("ep01.mkv"), b"dummy").unwrap();
    fs::create_dir(season.join("extras")).unwrap();
    fs::write(season.join("extras/extra.mkv"), b"dummy").unwrap();
    symlink(season.join("extras"), season.join("link_extras")).unwrap();

    let aliased_root = base.path().join("aliased_root");
    symlink(&real, &aliased_root).unwrap();
    assert_ne!(
        aliased_root.canonicalize().unwrap(),
        aliased_root,
        "precondition: the walk root must not already be canonical"
    );

    let results = scan_mkv_files(&aliased_root, true, None);
    assert_eq!(results.len(), 2, "duplicate episodes leaked: {results:?}");
}

/// `ignore_paths` must still apply when the walk root is not canonical -- a relative CLI root
/// (`-i ./Anime`) or one reached through a symlinked mount.
#[cfg(unix)]
#[test]
fn test_scan_mkv_files_honors_ignore_paths_under_non_canonical_root() {
    let base = tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir_all(real.join("keep")).unwrap();
    fs::create_dir_all(real.join("skip")).unwrap();
    fs::write(real.join("keep/keep.mkv"), b"dummy").unwrap();
    fs::write(real.join("skip/skip.mkv"), b"dummy").unwrap();

    let ignore = HashSet::from([real.join("skip")]);

    let aliased = base.path().join("aliased");
    std::os::unix::fs::symlink(&real, &aliased).unwrap();
    let with_dotdot = base.path().join("real/..").join("real");

    for (label, root) in [("symlinked root", aliased), ("dot-dot root", with_dotdot)] {
        assert_ne!(
            root.canonicalize().unwrap(),
            root,
            "precondition: {label} must not already be canonical"
        );
        let results = scan_mkv_files(&root, true, Some(&ignore));
        assert_eq!(
            results.len(),
            1,
            "ignore_paths silently ignored under {label}: {results:?}"
        );
        assert_eq!(results[0].name, "keep.mkv");
    }
}

/// Directories beneath a symlinked directory are reached under a non-canonical spelling and
/// must still match the canonicalized `ignore_paths`.
#[cfg(unix)]
#[test]
fn test_scan_mkv_files_honors_ignore_paths_under_symlinked_subdirectory() {
    let base = tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(outside.join("keep")).unwrap();
    fs::create_dir_all(outside.join("skip")).unwrap();
    fs::write(outside.join("keep/keep.mkv"), b"dummy").unwrap();
    fs::write(outside.join("skip/skip.mkv"), b"dummy").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

    let ignore = HashSet::from([outside.join("skip")]);
    let results = scan_mkv_files(&root, true, Some(&ignore));
    assert_eq!(
        results.len(),
        1,
        "ignore_paths silently ignored beneath a symlinked directory: {results:?}"
    );
    assert_eq!(results[0].name, "keep.mkv");
}

#[test]
fn test_scan_mkv_files_honors_ignore_paths_for_individual_files() {
    let base = tempdir().unwrap();
    let file1 = base.path().join("ep01.mkv");
    let file2 = base.path().join("ep02.mkv");
    fs::write(&file1, b"dummy1").unwrap();
    fs::write(&file2, b"dummy2").unwrap();

    let ignore = HashSet::from([file2]);
    let results = scan_mkv_files(base.path(), false, Some(&ignore));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "ep01.mkv");
}

/// Reported paths keep the caller's spelling even when the root is a symlink; only dedup keys
/// use the resolved path. Asset directories are derived from the same spelling.
#[cfg(unix)]
#[test]
fn test_scan_mkv_files_keeps_caller_spelling_under_non_canonical_root() {
    let base = tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("ep01.mkv"), b"dummy").unwrap();
    let aliased = base.path().join("aliased_root");
    std::os::unix::fs::symlink(&real, &aliased).unwrap();

    let results = scan_mkv_files(&aliased, true, None);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].path,
        aliased.join("ep01.mkv"),
        "scan must report paths in the caller's spelling so they match asset directories"
    );
}

/// `of_dir_entry` agrees with `FileSignature::of` for a regular entry and for a symlink to a
/// regular file, and reuses the caller's metadata instead of re-reading it.
#[cfg(unix)]
#[test]
fn test_file_signature_of_dir_entry_matches_resolved_of() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let target = tmp.path().join("a-fairly-long-target-name.ttf");
    fs::write(&target, vec![b'x'; 4096]).unwrap();
    let link = tmp.path().join("l.ttf");
    symlink(&target, &link).unwrap();

    let entries: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    let target_entry = entries.iter().find(|e| e.path() == target).unwrap();
    let link_entry = entries.iter().find(|e| e.path() == link).unwrap();

    let target_meta = target_entry.metadata().unwrap();
    let expected_target = FileSignature::of(&target).unwrap();
    assert_eq!(
        FileSignature::of_dir_entry(target_entry, &target_meta).unwrap(),
        expected_target,
        "a regular entry must sign the file the caller read"
    );
    let link_meta = link_entry.metadata().unwrap();
    assert!(
        link_meta.is_symlink(),
        "precondition: lstat must not follow the link"
    );
    assert_ne!(
        FileSignature::from_metadata(&link_meta),
        expected_target,
        "precondition: the link's own metadata must differ from its target's"
    );
    assert_eq!(
        FileSignature::of_dir_entry(link_entry, &link_meta).unwrap(),
        FileSignature::of(&link).unwrap(),
        "a symlinked entry must be reported under its resolved signature"
    );

    // Deleting the file proves the regular-entry path signs the metadata it was handed
    // rather than issuing a second `stat`.
    fs::remove_file(&target).unwrap();
    assert_eq!(
        FileSignature::of_dir_entry(target_entry, &target_meta),
        Some(expected_target),
        "of_dir_entry must reuse the caller's metadata, not re-stat the entry"
    );
}

/// A directory named with a font extension, or a symlink to one, must never be signed,
/// because its `Metadata::len()` is the directory's own byte length rather than any file size.
#[cfg(unix)]
#[test]
fn test_file_signature_of_dir_entry_rejects_directories() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("dir.otf");
    fs::create_dir(&dir).unwrap();
    let link = tmp.path().join("link.otf");
    symlink(&dir, &link).unwrap();

    let entries = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap())
        .collect::<Vec<_>>();
    let link_entry = entries.iter().find(|e| e.path() == link).unwrap();
    let link_meta = link_entry.metadata().unwrap();
    assert!(
        link_meta.is_symlink(),
        "precondition: DirEntry::metadata must not follow the link"
    );
    assert!(
        FileSignature::of_dir_entry(link_entry, &link_meta).is_none(),
        "a symlink to a directory must not produce a font signature"
    );

    let dir_entry = entries.iter().find(|e| e.path() == dir).unwrap();
    let dir_meta = dir_entry.metadata().unwrap();
    assert!(
        dir_meta.is_dir(),
        "precondition: a real directory entry must report a directory"
    );
    assert!(
        FileSignature::of_dir_entry(dir_entry, &dir_meta).is_none(),
        "a real directory must not produce a font signature"
    );
}

/// Only symlink aliases are deduplicated: a hardlink pair is two directory entries whose
/// realpaths differ, so both are discovered.
#[cfg(unix)]
#[test]
fn test_scan_mkv_files_hardlink_aliases_are_both_discovered() {
    let tmp = tempdir().unwrap();
    let original = tmp.path().join("ep01.mkv");
    fs::write(&original, b"dummy").unwrap();
    let hardlink = tmp.path().join("ep01_hard.mkv");
    fs::hard_link(&original, &hardlink).unwrap();

    let results = scan_mkv_files(tmp.path(), false, None);
    assert_eq!(results.len(), 2);
}

/// `DirEntry::metadata` does not resolve symlinks while `FileSignature::of` does; for a regular
/// file the two must agree on every platform, which is what lets `of_dir_entry` skip a `stat`.
#[test]
fn test_file_signature_from_dirent_metadata_matches_resolved_of() {
    let tmp = tempdir().unwrap();
    let file = tmp.path().join("font.ttf");
    fs::write(&file, b"not-a-real-font").unwrap();

    let dirent_meta = fs::read_dir(tmp.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .metadata()
        .unwrap();
    assert!(!dirent_meta.is_symlink());
    assert_eq!(
        FileSignature::from_metadata(&dirent_meta),
        FileSignature::of(&file).unwrap()
    );
}
