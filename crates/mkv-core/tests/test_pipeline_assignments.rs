use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, ItemOutcome, JobEvent, PipelineConfig,
    PipelineMode,
};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};

fn sample_video_metadata() -> MkvmergeMetadata {
    MkvmergeMetadata {
        tracks: vec![
            MkvTrack {
                id: Some(0),
                track_type: "video".to_string(),
                codec: Some("V_MPEGH/ISO/HEVC".to_string()),
                properties: TrackProperties {
                    codec_id: Some("V_MPEGH/ISO/HEVC".to_string()),
                    pixel_dimensions: Some("1920x1080".to_string()),
                    ..Default::default()
                },
            },
            MkvTrack {
                id: Some(1),
                track_type: "audio".to_string(),
                codec: Some("A_FLAC".to_string()),
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
    }
}

#[tokio::test]
async fn test_missing_assignment_aborts_entire_remux_plan() {
    use mkv_core::request::{BatchRequest, TrackAssignment};

    for (assigned_name, dry_run) in [("Show - 02.mkv", false), ("unparseable.mkv", true)] {
        let input = tempdir().unwrap();
        let output = tempdir().unwrap();
        let first = input.path().join("Show - 01.mkv");
        let assigned = input.path().join(assigned_name);
        let manual = input.path().join("manual.ass");
        fs::write(&first, b"first video").unwrap();
        fs::write(&assigned, b"second video").unwrap();
        fs::write(input.path().join("Show - 01.ass"), b"first subtitle").unwrap();
        fs::write(input.path().join("Show - 02.ass"), b"automatic subtitle").unwrap();
        fs::write(&manual, b"manual subtitle").unwrap();

        let assignment = TrackAssignment {
            target_video: assigned_name.into(),
            asset_path: "manual.ass".into(),
            track_type: Some("subtitle".into()),
            language: Some("ind".into()),
            title: Some("Manual".into()),
            offset_ms: Some(-1250),
        };
        let request = BatchRequest {
            input_dir: input.path().to_path_buf(),
            assignments: Some(vec![assignment.clone()]),
            selected_items: Some(vec!["Show - 01.mkv".into(), assigned_name.into()]),
            ..Default::default()
        };
        request.validate().unwrap();
        fs::remove_file(&manual).unwrap();

        let cache = Arc::new(PersistentInspectCache::new(
            Some(input.path().join("test_cache.json")),
            true,
        ));
        cache.put_path(&first, sample_video_metadata());
        cache.put_path(&assigned, sample_video_metadata());
        cache.save();
        let config = PipelineConfig {
            input_dir: input.path().to_path_buf(),
            out_dir: Some(output.path().to_path_buf()),
            mode: PipelineMode::Remux,
            dry_run,
            cache: Some(cache),
            track_assignments: Some(vec![assignment]),
            selected_items: request.selected_items,
            ..Default::default()
        };
        let (tx, mut rx) = create_event_channel(32);
        let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());
        let summary = pipeline.run().await.unwrap();
        assert_eq!(
            (
                summary.total,
                summary.completed,
                summary.skipped,
                summary.errors
            ),
            (2, 0, 0, 2)
        );
        assert!(!summary.cancelled);

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(matches!(
            events.first(),
            Some(JobEvent::BatchStart { total: 2, .. })
        ));
        let outcomes: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                JobEvent::ItemComplete {
                    id, status, error, ..
                } => Some((id.as_str(), status, error.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|(_, status, _)| **status == ItemOutcome::Error));
        assert!(outcomes.iter().any(|(id, _, err)| *id == assigned_name
            && err.unwrap().contains(&manual.display().to_string())));
        assert!(outcomes.iter().any(|(id, _, err)| *id == "Show - 01.mkv"
            && err.unwrap().contains("Batch planning aborted")));
        let terminal_count = events
            .iter()
            .filter(|event| matches!(event, JobEvent::BatchComplete { .. }))
            .count();
        assert_eq!(terminal_count, 1, "batch must settle exactly once");
        let terminal_index = events
            .iter()
            .position(|event| matches!(event, JobEvent::BatchComplete { .. }))
            .expect("expected a terminal event");
        assert!(matches!(
            events[terminal_index],
            JobEvent::BatchComplete {
                success: false,
                cancelled: false,
                ..
            }
        ));
        let last_item_event = events
            .iter()
            .rposition(|event| matches!(event, JobEvent::ItemComplete { .. }))
            .expect("planning failure must settle every selected item");
        assert!(
            last_item_event < terminal_index,
            "the single terminal must follow every item error"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn test_valid_assignment_plans_unparsed_video() {
    use mkv_core::request::{BatchRequest, TrackAssignment};

    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let unparsed = input.path().join("unparseable_video.mkv");
    let manual = input.path().join("manual.ass");
    fs::write(&unparsed, b"unparsed video").unwrap();
    fs::write(&manual, b"manual subtitle").unwrap();

    let assignment = TrackAssignment {
        target_video: "unparseable_video.mkv".into(),
        asset_path: "manual.ass".into(),
        track_type: Some("subtitle".into()),
        language: Some("eng".into()),
        title: Some("Manual".into()),
        offset_ms: None,
    };
    let request = BatchRequest {
        input_dir: input.path().to_path_buf(),
        assignments: Some(vec![assignment.clone()]),
        selected_items: Some(vec!["unparseable_video.mkv".into()]),
        ..Default::default()
    };
    request.validate().unwrap();

    let cache = Arc::new(PersistentInspectCache::new(None, true));
    cache.put_path(&unparsed, sample_video_metadata());

    let config = PipelineConfig {
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        mode: PipelineMode::Remux,
        dry_run: true,
        cache: Some(cache),
        track_assignments: Some(vec![assignment]),
        selected_items: request.selected_items,
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(32);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.errors, 0);

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::ItemComplete {
            status: ItemOutcome::Planned,
            ..
        }
    )));
}

#[tokio::test]
async fn test_root_assignment_does_not_match_nested_unparsed_video_with_same_name() {
    use mkv_core::request::TrackAssignment;

    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let season = input.path().join("Season 01");
    fs::create_dir_all(&season).unwrap();
    let root_video = input.path().join("unparseable_video.mkv");
    let nested_video = season.join("unparseable_video.mkv");
    fs::write(&root_video, b"root video").unwrap();
    fs::write(&nested_video, b"nested video").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(None, true));
    cache.put_path(&root_video, sample_video_metadata());
    cache.put_path(&nested_video, sample_video_metadata());

    let config = PipelineConfig {
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        mode: PipelineMode::Remux,
        dry_run: true,
        cache: Some(cache),
        track_assignments: Some(vec![TrackAssignment {
            target_video: "unparseable_video.mkv".into(),
            asset_path: "manual.ass".into(),
            track_type: Some("subtitle".into()),
            language: Some("eng".into()),
            title: Some("Manual".into()),
            offset_ms: None,
        }]),
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(32);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.completed, 1);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.errors, 0);

    let mut outcomes = HashMap::new();
    while let Ok(event) = rx.try_recv() {
        if let JobEvent::ItemComplete { id, status, .. } = event {
            outcomes.insert(id, status);
        }
    }
    assert!(matches!(
        outcomes.get("unparseable_video.mkv"),
        Some(ItemOutcome::Planned)
    ));
    assert!(matches!(
        outcomes.get("Season 01/unparseable_video.mkv"),
        Some(ItemOutcome::Skipped)
    ));
}

#[tokio::test]
async fn test_pipeline_with_track_assignments_plans_override() {
    let dir = tempfile::tempdir().unwrap();
    let video_path = dir.path().join("Episode - 01.mkv");
    let sub_path = dir.path().join("custom_track.ass");

    std::fs::write(&video_path, b"dummy mkv content").unwrap();
    std::fs::write(&sub_path, b"[Script Info]\nTitle: Custom\n").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(
        Some(dir.path().join("test_cache.json")),
        true,
    ));
    cache.put_path(&video_path, sample_video_metadata());
    cache.save();

    let assignment = mkv_core::request::TrackAssignment {
        target_video: "Episode - 01.mkv".into(),
        asset_path: std::path::PathBuf::from("custom_track.ass"),
        track_type: Some("subtitle".into()),
        language: Some("ind".into()),
        title: Some("Bahasa Indonesia".into()),
        offset_ms: Some(1250),
    };

    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: dir.path().to_path_buf(),
        dry_run: true,
        cache: Some(cache),
        track_assignments: Some(vec![assignment]),
        ..Default::default()
    };

    let (tx, mut rx) = create_event_channel(32);
    let cancel_token = CancellationToken::new();
    let pipeline = BatchPipeline::new(config, Some(tx), cancel_token);

    let summary = pipeline.run().await.unwrap();
    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let complete_event = events
        .iter()
        .find(|e| {
            matches!(
                e,
                JobEvent::ItemComplete {
                    status: ItemOutcome::Planned,
                    ..
                }
            )
        })
        .expect("Expected ItemComplete with Planned status");

    if let JobEvent::ItemComplete { file, .. } = complete_event {
        assert_eq!(file, "Episode - 01.mkv");
    }
}

fn manual_subtitle_assignment(target: &str, asset: &str) -> mkv_core::request::TrackAssignment {
    mkv_core::request::TrackAssignment {
        target_video: target.into(),
        asset_path: asset.into(),
        track_type: Some("subtitle".into()),
        language: Some("ind".into()),
        title: Some("Bahasa Indonesia".into()),
        offset_ms: Some(1250),
    }
}

#[tokio::test]
async fn test_assignment_target_excluded_from_discovery_fails_closed() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    let discovered = input.path().join("Show - 01.mkv");
    fs::write(&discovered, b"video").unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        dry_run: true,
        track_assignments: Some(vec![manual_subtitle_assignment(
            "Output/hidden.mkv",
            "manual.ass",
        )]),
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(16);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());

    let err = pipeline.run().await.unwrap_err();
    assert!(err.contains("Output/hidden.mkv"), "{err}");
    assert!(
        rx.try_recv().is_err(),
        "an unusable assignment must abort before BatchStart"
    );
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_assignment_target_outside_active_selection_fails_closed() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let selected = input.path().join("Show - 01.mkv");
    let excluded = input.path().join("Show - 02.mkv");
    fs::write(&selected, b"first video").unwrap();
    fs::write(&excluded, b"second video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        dry_run: true,
        track_assignments: Some(vec![manual_subtitle_assignment(
            "Show - 02.mkv",
            "manual.ass",
        )]),
        selected_items: Some(vec!["Show - 01.mkv".into()]),
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(16);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());

    let err = pipeline.run().await.unwrap_err();
    assert!(err.contains("Show - 02.mkv"), "{err}");
    assert!(rx.try_recv().is_err(), "{err}");
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_every_unmatched_assignment_target_fails_in_one_error() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"video").unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        dry_run: true,
        track_assignments: Some(vec![
            manual_subtitle_assignment("Output/hidden.mkv", "manual.ass"),
            manual_subtitle_assignment("ghost.mkv", "manual.ass"),
            manual_subtitle_assignment("Season 02/Show - 02.mkv", "manual.ass"),
            manual_subtitle_assignment("Output/hidden.mkv", "manual.ass"),
        ]),
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(16);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());

    let err = pipeline.run().await.unwrap_err();
    assert_eq!(
        err,
        concat!(
            "Track assignment targets do not match exactly one video in ",
            "the discovered MKV files: Output/hidden.mkv, ghost.mkv, ",
            "Season 02/Show - 02.mkv"
        )
    );
    assert!(rx.try_recv().is_err(), "{err}");
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_valid_assignments_still_resolve_within_the_active_selection() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let unparsed = input.path().join("raw_capture.mkv");
    let automatic = input.path().join("Show - 01.mkv");
    fs::write(&unparsed, b"unparsed video").unwrap();
    fs::write(&automatic, b"video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(None, true));
    cache.put_path(&unparsed, sample_video_metadata());
    cache.put_path(&automatic, sample_video_metadata());

    let selected = vec!["raw_capture.mkv".into(), "Show - 01.mkv".into()];
    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        dry_run: true,
        cache: Some(cache),
        track_assignments: Some(vec![manual_subtitle_assignment(
            "raw_capture.mkv",
            "manual.ass",
        )]),
        selected_items: Some(selected.clone()),
        ..Default::default()
    };
    let (tx, mut rx) = create_event_channel(32);
    let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());

    let summary = pipeline.run().await.unwrap();
    assert_eq!(
        (
            summary.total,
            summary.completed,
            summary.skipped,
            summary.errors
        ),
        (2, 2, 0, 0)
    );

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    let items = events
        .iter()
        .find_map(|event| match event {
            JobEvent::BatchStart { items, .. } => items.clone(),
            _ => None,
        })
        .expect("expected BatchStart event");
    assert_eq!(
        items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        selected
    );
    let outcomes: HashMap<_, _> = events
        .iter()
        .filter_map(|event| match event {
            JobEvent::ItemComplete { id, status, .. } => Some((id.as_str(), *status)),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes.get("raw_capture.mkv"), Some(&ItemOutcome::Planned));
    assert_eq!(outcomes.get("Show - 01.mkv"), Some(&ItemOutcome::Planned));
}
