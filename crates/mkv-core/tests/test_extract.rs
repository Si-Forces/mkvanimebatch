use std::fs::File;
use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::extract::{extract_mkv_file, select_subtitle_for_extraction};
use mkv_core::fonts::FontRegistry;
use mkv_core::scan::{InspectedVideo, MkvmergeMetadata};

#[test]
fn test_extract_mkv_file_no_work() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dir = tempdir().unwrap();
        let mkv_path = dir.path().join("Show - 01.mkv");
        File::create(&mkv_path).unwrap();

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
        let inspected = InspectedVideo::new(meta);
        let font_mgr = FontRegistry::new(dir.path().join("fonts"), None);

        let summary = extract_mkv_file(&mkv_path, &inspected, &font_mgr, None, None)
            .await
            .unwrap();
        assert_eq!(summary.extracted_sub, None);
        assert_eq!(summary.extracted_chapters, None);
        assert_eq!(summary.extracted_cover, None);
        assert_eq!(summary.extracted_fonts, 0);
        assert!(summary
            .log
            .iter()
            .any(|l| l.contains("No tracks, chapters, covers, or new fonts to extract")));
    });
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
                    track_type: "subtitles".to_string(),
                    codec: Some("S_TEXT/ASS".to_string()),
                    properties: TrackProperties {
                        language: Some(payload.to_string()),
                        codec_id: Some("S_TEXT/ASS".to_string()),
                        ..Default::default()
                    },
                },
            ],
            attachments: vec![],
            chapters: vec![],
        };
        let inspected = InspectedVideo::new(meta);
        let selected = select_subtitle_for_extraction(&inspected, "Episode_01", dir.path(), None);
        assert!(selected.is_some(), "Failed for payload: {payload:?}");
        let (track_id, sub_path) = selected.unwrap();
        assert_eq!(track_id, 1);
        // Path must stay inside dir.path() and not escape
        assert!(
            sub_path.starts_with(dir.path()),
            "Sub path escaped target directory for {payload:?}: {:?}",
            sub_path
        );
        assert!(
            !sub_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "Sub path contains ParentDir traversal component for {payload:?}: {:?}",
            sub_path
        );
        assert_eq!(
            sub_path.parent(),
            Some(dir.path()),
            "Sub path is not an immediate child of target dir for {payload:?}: {:?}",
            sub_path
        );
    }
}
