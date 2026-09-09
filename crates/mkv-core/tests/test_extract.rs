use std::fs::File;
use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::extract::extract_mkv_file;
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

        let summary = extract_mkv_file(&mkv_path, &inspected, &font_mgr, None)
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
