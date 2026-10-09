mod common;

use std::fs;
use tempfile::tempdir;

use common::{ScriptedRunner, Step};
use mkv_core::domain::{EpisodeInfo, MkvTrack, TrackProperties, SUBPROCESS_TIMEOUT};
use mkv_core::extract::{extract_mkv_file, select_subtitle_for_extraction};
use mkv_core::fonts::FontRegistry;
use mkv_core::media_tool::MediaTool;
use mkv_core::scan::{InspectedVideo, MkvAttachment, MkvmergeMetadata};

fn video_track() -> MkvTrack {
    MkvTrack {
        id: Some(0),
        track_type: "video".to_string(),
        codec: None,
        properties: TrackProperties::default(),
    }
}

fn inspected(tracks: Vec<MkvTrack>) -> InspectedVideo {
    InspectedVideo::new(MkvmergeMetadata {
        tracks,
        attachments: vec![],
        chapters: vec![],
    })
}

/// With nothing to extract, mkvextract is never spawned and nothing is written beside the video.
#[tokio::test]
async fn test_extract_mkv_file_no_work() {
    let dir = tempdir().unwrap();
    let mkv_path = dir.path().join("Show - 01.mkv");
    fs::File::create(&mkv_path).unwrap();
    let font_mgr = FontRegistry::new(dir.path().join("fonts"), None);

    let summary = extract_mkv_file(
        &mkv_path,
        &inspected(vec![video_track()]),
        &font_mgr,
        None,
        None,
        &ScriptedRunner::new([]),
    )
    .await
    .unwrap();
    assert_eq!(summary.extracted_sub, None);
    assert_eq!(summary.extracted_chapters, None);
    assert_eq!(summary.extracted_cover, None);
    assert_eq!(summary.extracted_fonts, 0);

    let entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["Show - 01.mkv"]);
}

fn ass_subtitle_track() -> MkvTrack {
    MkvTrack {
        id: Some(2),
        track_type: "subtitles".to_string(),
        codec: None,
        properties: TrackProperties {
            codec_id: Some("s_text/ass".to_string()),
            language: Some("eng".to_string()),
            ..Default::default()
        },
    }
}

/// mkvextract exits 1 when it finished with warnings: the extraction still succeeds, and the
/// warning reaches the summary log.
#[tokio::test]
async fn test_extract_mkv_file_logs_mkvextract_warnings_on_exit_code_1() {
    let dir = tempdir().unwrap();
    let mkv_path = dir.path().join("Show - 01.mkv");
    fs::File::create(&mkv_path).unwrap();
    let font_mgr = FontRegistry::new(dir.path().join("fonts"), None);
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvextract,
        1,
        "Warning: Cluster out of order\n",
        "",
    )
    .writes_extract_targets(b"[Script Info]\n")]);

    let summary = extract_mkv_file(
        &mkv_path,
        &inspected(vec![video_track(), ass_subtitle_track()]),
        &font_mgr,
        None,
        None,
        &runner,
    )
    .await
    .unwrap();

    assert!(
        summary
            .log
            .iter()
            .any(|l| l == "  Warning: Cluster out of order"),
        "log did not contain the warning: {:?}",
        summary.log
    );
    let extracted = summary.extracted_sub.expect("subtitle was not promoted");
    assert_eq!(fs::read(extracted).unwrap(), b"[Script Info]\n");
    assert!(matches!(
        runner.calls().as_slice(),
        [(MediaTool::Mkvextract, _)]
    ));
    assert_eq!(runner.timeouts(), [SUBPROCESS_TIMEOUT]);
}

/// A failed mkvextract run fails the item and leaves nothing beside the video.
#[tokio::test]
async fn test_extract_mkv_file_fails_on_mkvextract_exit_code_2() {
    let dir = tempdir().unwrap();
    let mkv_path = dir.path().join("Show - 01.mkv");
    fs::File::create(&mkv_path).unwrap();
    let font_mgr = FontRegistry::new(dir.path().join("fonts"), None);
    let runner = ScriptedRunner::new([Step::exited(
        MediaTool::Mkvextract,
        2,
        "",
        "Error: cannot open file\n",
    )]);

    let err = extract_mkv_file(
        &mkv_path,
        &inspected(vec![video_track(), ass_subtitle_track()]),
        &font_mgr,
        None,
        None,
        &runner,
    )
    .await
    .unwrap_err();

    assert_eq!(
        err,
        "mkvextract failed (exit code 2): Error: cannot open file"
    );
    let entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["Show - 01.mkv"]);
}

#[test]
fn test_subtitle_extraction_filename_format() {
    let inspected = inspected(vec![video_track(), ass_subtitle_track()]);
    let out = std::path::Path::new("/out");

    let parsed = EpisodeInfo::new("1", "E", "01");
    let (tid, path) =
        select_subtitle_for_extraction(&inspected, "Show - 01", out, Some(&parsed)).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(path, out.join("Show - 01_EP01_eng_track2.ass"));

    let (tid, path) = select_subtitle_for_extraction(&inspected, "Extras", out, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(path, out.join("Extras_eng_track2.ass"));
}

#[test]
fn test_select_subtitle_for_extraction_sanitizes_path_traversal() {
    let dir = tempdir().unwrap();
    let test_payloads = [
        "../../evil/escape",
        "/abs/x/traversal",
        "..\\windows\\escape",
        "CON",
        "",
        "   ",
        "..",
    ];

    for payload in test_payloads {
        let subtitle = MkvTrack {
            id: Some(1),
            track_type: "subtitles".to_string(),
            codec: Some("S_TEXT/ASS".to_string()),
            properties: TrackProperties {
                language: Some(payload.to_string()),
                codec_id: Some("S_TEXT/ASS".to_string()),
                ..Default::default()
            },
        };
        let selected = select_subtitle_for_extraction(
            &inspected(vec![video_track(), subtitle]),
            "Episode_01",
            dir.path(),
            None,
        );
        let (track_id, sub_path) =
            selected.unwrap_or_else(|| panic!("Failed for payload: {payload:?}"));
        assert_eq!(track_id, 1);
        assert!(
            !sub_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "Sub path contains ParentDir traversal component for {payload:?}: {sub_path:?}"
        );
        assert_eq!(
            sub_path.parent(),
            Some(dir.path()),
            "Sub path is not an immediate child of target dir for {payload:?}: {sub_path:?}"
        );
    }
}

fn font_attachment(id: u32, name: &str) -> MkvAttachment {
    MkvAttachment {
        id: Some(id),
        file_name: Some(name.to_string()),
        content_type: Some("application/x-truetype-font".to_string()),
        size: Some(10),
    }
}

/// Extracts `fonts` from a video whose mkvextract run writes `content` to every attachment
/// target, and returns the summary.
async fn extract_fonts(
    fonts: &[MkvAttachment],
    content: &'static [u8],
) -> mkv_core::extract::ExtractSummary {
    let dir = tempdir().unwrap();
    let mkv_path = dir.path().join("Show - 01.mkv");
    fs::File::create(&mkv_path).unwrap();
    let font_mgr = FontRegistry::new(dir.path().join("fonts"), None);
    let runner = ScriptedRunner::new([
        Step::exited(MediaTool::Mkvextract, 0, "", "").writes_extract_targets(content)
    ]);
    let inspected = InspectedVideo::new(MkvmergeMetadata {
        tracks: vec![video_track()],
        attachments: fonts.to_vec(),
        chapters: vec![],
    });
    extract_mkv_file(&mkv_path, &inspected, &font_mgr, None, None, &runner)
        .await
        .unwrap()
}

/// A font mkvextract left empty is reported on the item's log; the item still completes and the
/// font count stays at zero.
#[tokio::test]
async fn test_extract_mkv_file_logs_a_font_that_was_not_saved() {
    let summary = extract_fonts(&[font_attachment(1, "Broken.ttf")], b"").await;

    assert_eq!(summary.extracted_fonts, 0);
    assert!(
        summary
            .log
            .iter()
            .any(|l| l
                == "   -> WARNING: Font 'Broken.ttf' was not saved: the extracted file is empty"),
        "{:?}",
        summary.log
    );
    assert!(!summary.log.iter().any(|l| l.contains("new font(s)")));
}

/// A content duplicate is dedup working as designed: counted once and not warned about.
#[tokio::test]
async fn test_extract_mkv_file_does_not_warn_about_a_duplicate_font() {
    let fonts = [font_attachment(1, "A.ttf"), font_attachment(2, "B.ttf")];
    let summary = extract_fonts(&fonts, b"same font bytes").await;

    assert_eq!(summary.extracted_fonts, 1);
    assert!(
        !summary.log.iter().any(|l| l.contains("WARNING")),
        "{:?}",
        summary.log
    );
}
