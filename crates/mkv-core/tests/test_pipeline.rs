use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::fonts::FontHashMemo;
use mkv_core::pipeline::{
    create_event_channel, create_log_channel, BatchPipeline, CancellationToken, ItemOutcome,
    JobEvent, PipelineConfig, PipelineMode,
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
async fn test_extract_dry_run_emits_planned_events() {
    let tmp = tempdir().unwrap();
    let season_dir = tmp.path().join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    let ep1 = tmp.path().join("Show - 01.mkv");
    let ep2 = season_dir.join("Show - 02.mkv");
    fs::write(&ep1, b"dummy content 1").unwrap();
    fs::write(&ep2, b"dummy content 2").unwrap();

    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: true,
        no_cache: false,
        cache: None,
        cache_file: None,
        workers: 2,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 2,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 2);
    assert_eq!(summary.completed, 2);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.errors, 0);
    assert!(!summary.cancelled);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }

    assert!(events
        .iter()
        .any(|e| matches!(e, JobEvent::BatchStart { total: 2, .. })));
    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::BatchComplete {
            success: true,
            cancelled: false,
            ..
        }
    )));

    let planned_items: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::ItemComplete { id, status, .. } if *status == ItemOutcome::Planned => {
                Some(id.clone())
            }
            _ => None,
        })
        .collect();

    assert_eq!(planned_items.len(), 2);
    assert!(planned_items.contains(&"Show - 01.mkv".to_string()));
    assert!(planned_items.contains(&"Season 01/Show - 02.mkv".to_string()));
}

#[tokio::test]
async fn test_extract_dry_run_preserves_nested_output_directory() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();

    let season_dir = in_tmp.path().join("Season 03");
    fs::create_dir_all(&season_dir).unwrap();

    let ep1 = season_dir.join("Show - 01.mkv");
    fs::write(&ep1, b"dummy mkv").unwrap();

    let config = PipelineConfig {
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: Some(out_tmp.path().to_path_buf()),
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: true,
        no_cache: false,
        cache: None,
        cache_file: None,
        workers: 1,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }

    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::ItemComplete {
            id,
            status: ItemOutcome::Planned,
            duration_sec: None,
            ..
        } if id == "Season 03/Show - 01.mkv"
    )));
}

#[tokio::test]
async fn test_remux_dry_run_preserves_nested_output_directory() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();

    let season_dir = in_tmp.path().join("Season 02");
    fs::create_dir_all(&season_dir).unwrap();

    let ep1 = season_dir.join("Show - 01.mkv");
    let sub1 = season_dir.join("Show - 01.ass");
    fs::write(&ep1, b"dummy mkv").unwrap();
    fs::write(&sub1, b"[Script Info]\nTitle: Test Sub").unwrap();

    // Seed inspection cache so probe succeeds without external mkvmerge
    let cache = Arc::new(PersistentInspectCache::new(
        Some(in_tmp.path().join("test_cache.json")),
        true,
    ));
    cache.put_path(&ep1, sample_video_metadata());
    cache.save();

    let config = PipelineConfig {
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: Some(out_tmp.path().to_path_buf()),
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Remux,
        dry_run: true,
        no_cache: false,
        cache: Some(cache.clone()),
        cache_file: None,
        workers: 2,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 2,
        sub_offset: 0.0,
        ..Default::default()
    };
    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);

    // Verify output path preserved Season 02 subdirectory
    let expected_nested_path = out_tmp.path().join("Season 02").join("Show - 01.mkv");
    let expected_nested_str = expected_nested_path.to_string_lossy().to_string();
    let expected_nested_slash = expected_nested_str.replace('\\', "/");
    let log_contains_expected_output = summary
        .log
        .iter()
        .any(|line| line.contains(&expected_nested_str) || line.contains(&expected_nested_slash));
    assert!(
        log_contains_expected_output,
        "Log should contain preserved nested output path '{}'. Actual log:\n{:#?}",
        expected_nested_path.display(),
        summary.log
    );

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::ItemComplete {
            id,
            status: ItemOutcome::Planned,
            duration_sec: None,
            ..
        } if id == "Season 02/Show - 01.mkv"
    )));
}

#[tokio::test]
async fn test_remux_skips_when_no_matching_subtitle() {
    let in_tmp = tempdir().unwrap();
    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(
        Some(in_tmp.path().join("test_cache.json")),
        true,
    ));
    cache.put_path(&ep, sample_video_metadata());
    cache.save();

    let config = PipelineConfig {
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Remux,
        dry_run: true,
        no_cache: false,
        cache: Some(cache.clone()),
        cache_file: None,
        workers: 2,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 2,
        sub_offset: 0.0,
        ..Default::default()
    };
    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.errors, 0);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::ItemComplete {
            status: ItemOutcome::Skipped,
            duration_sec: None,
            ..
        }
    )));
}

#[tokio::test]
async fn test_pipeline_cancellation_drains_items() {
    let tmp = tempdir().unwrap();
    let ep1 = tmp.path().join("Show - 01.mkv");
    let ep2 = tmp.path().join("Show - 02.mkv");
    fs::write(&ep1, b"dummy mkv 1").unwrap();
    fs::write(&ep2, b"dummy mkv 2").unwrap();

    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: true,
        no_cache: false,
        cache: None,
        cache_file: None,
        workers: 1,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    // Cancel immediately before running
    cancel_token.cancel();

    let (event_sender, mut event_receiver) = create_event_channel(64);
    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert!(summary.cancelled);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }

    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::BatchComplete {
            cancelled: true,
            success: false,
            ..
        }
    )));

    let cancelled_count = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                JobEvent::ItemComplete {
                    status: ItemOutcome::Cancelled,
                    duration_sec: None,
                    ..
                }
            )
        })
        .count();
    assert_eq!(cancelled_count, 2);
}

#[tokio::test]
async fn test_pipeline_with_log_sender() {
    let dir = tempdir().unwrap();
    let config = PipelineConfig {
        input_dir: dir.path().to_path_buf(),
        mode: PipelineMode::Extract,
        ..Default::default()
    };

    let (event_sender, _event_receiver) = create_event_channel(64);
    let (log_sender, mut log_receiver) = create_log_channel(64);
    let cancel_token = CancellationToken::new();

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token)
        .with_log_sender(Some(log_sender));

    let summary = pipeline.run().await.unwrap();
    assert_eq!(summary.total, 0);

    let mut logs = Vec::new();
    while let Ok(msg) = log_receiver.try_recv() {
        logs.push(msg);
    }

    assert!(!logs.is_empty());
    assert!(logs[0].contains("No MKV files found in"));
}

#[tokio::test]
async fn test_pipeline_from_request_with_preset_and_overrides() {
    let dir = tempdir().unwrap();
    let req = mkv_core::request::BatchRequest {
        mode: PipelineMode::Remux,
        input_dir: dir.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        preset: Some("balanced".into()),
        bitrate: Some("256k".into()),
        complexity: None,
        workers: Some(6),
        io_jobs: None,
        ffmpeg_threads: None,
        sub_offset: None,
        dry_run: Some(true),
        no_cache: None,
        assignments: None,
        selected_items: None,
    };

    assert!(req.validate().is_ok());

    let cancel_token = CancellationToken::new();
    let pipeline = BatchPipeline::from_request(req, None, cancel_token).unwrap();
    assert_eq!(pipeline.config.bitrate, "256k"); // overridden from 192k
    assert_eq!(pipeline.config.complexity, 5); // baseline from balanced
    assert_eq!(pipeline.config.workers, 6);
    assert!(pipeline.config.dry_run);
}

#[tokio::test]
async fn test_extract_emits_item_complete_duration_sec() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();

    let ep1 = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep1, b"dummy mkv").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(
        Some(in_tmp.path().join("test_cache.json")),
        true,
    ));
    cache.put_path(&ep1, sample_video_metadata());
    cache.save();

    let config = PipelineConfig {
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: Some(out_tmp.path().to_path_buf()),
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: false,
        no_cache: false,
        cache: Some(cache.clone()),
        cache_file: None,
        workers: 1,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }

    let complete_events: Vec<&JobEvent> = events
        .iter()
        .filter(|e| matches!(e, JobEvent::ItemComplete { .. }))
        .collect();
    assert_eq!(complete_events.len(), 1);

    match complete_events[0] {
        JobEvent::ItemComplete {
            id,
            status,
            duration_sec,
            ..
        } => {
            assert_eq!(id, "Show - 01.mkv");
            assert_eq!(*status, ItemOutcome::Completed);
            assert!(
                duration_sec.is_some(),
                "duration_sec should be Some(...) on completed item"
            );
            let d = duration_sec.unwrap();
            assert!(d >= 0.0, "duration_sec should be non-negative, got {d}");
        }
        _ => panic!("Expected ItemComplete event"),
    }
}

#[tokio::test]
async fn test_extract_error_in_worker_emits_duration_sec() {
    let tmp = tempdir().unwrap();
    // Create a non-MKV corrupt file that fails probing in the worker
    let ep = tmp.path().join("Corrupt - 01.mkv");
    fs::write(&ep, b"not a real mkv file").unwrap();

    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: false,
        no_cache: true,
        cache: None,
        cache_file: None,
        workers: 1,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(64);

    let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
    let summary = pipeline.run().await.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.errors, 1);

    let mut events = Vec::new();
    while let Ok(event) = event_receiver.try_recv() {
        events.push(event);
    }

    let error_event = events
        .iter()
        .find(|e| {
            matches!(
                e,
                JobEvent::ItemComplete {
                    status: ItemOutcome::Error,
                    ..
                }
            )
        })
        .expect("Expected ItemComplete with Error status");

    match error_event {
        JobEvent::ItemComplete {
            id, duration_sec, ..
        } => {
            assert_eq!(id, "Corrupt - 01.mkv");
            assert!(
                duration_sec.is_some(),
                "duration_sec should be Some(...) even on worker error"
            );
            assert!(duration_sec.unwrap() >= 0.0);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_pipeline_in_flight_cancellation_emits_duration_sec() {
    let tmp = tempdir().unwrap();
    let cache = Arc::new(PersistentInspectCache::new(None, false));
    for i in 1..=20 {
        let ep = tmp.path().join(format!("Show - {i:02}.mkv"));
        fs::write(&ep, b"dummy mkv").unwrap();
        cache.put_path(&ep, sample_video_metadata());
    }
    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: false,
        no_cache: false,
        cache: Some(cache),
        cache_file: None,
        workers: 1,
        io_jobs: 1,
        bitrate: "192k".to_string(),
        complexity: 10,
        ffmpeg_threads: 1,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(1);
    let cancel_token_clone = cancel_token.clone();
    let runner = tokio::spawn(async move {
        let pipeline = BatchPipeline::new(config, Some(event_sender), cancel_token);
        pipeline.run().await.unwrap()
    });

    let mut events = Vec::new();
    while let Some(event) = event_receiver.recv().await {
        if matches!(event, JobEvent::BatchStart { .. }) {
            cancel_token_clone.cancel();
        }
        events.push(event);
        if matches!(events.last(), Some(JobEvent::BatchComplete { .. })) {
            break;
        }
    }

    let summary = runner.await.unwrap();
    assert!(summary.cancelled);

    let cancelled_events: Vec<&JobEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                JobEvent::ItemComplete {
                    status: ItemOutcome::Cancelled,
                    ..
                }
            )
        })
        .collect();

    assert!(!cancelled_events.is_empty());

    let in_flight_cancelled = cancelled_events
        .iter()
        .find(|e| {
            matches!(
                e,
                JobEvent::ItemComplete {
                    duration_sec: Some(_),
                    ..
                }
            )
        })
        .expect("Expected at least one in-flight cancelled item with duration_sec");

    if let JobEvent::ItemComplete {
        duration_sec: Some(d),
        ..
    } = in_flight_cancelled
    {
        assert!(*d >= 0.0, "duration_sec must be non-negative");
    }
}

#[tokio::test]
async fn test_pipeline_selected_items_filters_and_orders_batch() {
    let dir = tempdir().unwrap();
    let ep1 = dir.path().join("[SubsPlease] Show - 01 (1080p).mkv");
    let ep2 = dir.path().join("[SubsPlease] Show - 02 (1080p).mkv");
    let ep3 = dir.path().join("[SubsPlease] Show - 03 (1080p).mkv");
    std::fs::write(&ep1, b"dummy1").unwrap();
    std::fs::write(&ep2, b"dummy2").unwrap();
    std::fs::write(&ep3, b"dummy3").unwrap();

    let config = PipelineConfig {
        mode: PipelineMode::Extract,
        input_dir: dir.path().to_path_buf(),
        dry_run: true,
        selected_items: Some(vec![
            "[SubsPlease] Show - 03 (1080p).mkv".into(),
            "[SubsPlease] Show - 01 (1080p).mkv".into(),
        ]),
        ..Default::default()
    };

    let (tx, mut rx) = create_event_channel(32);
    let cancel_token = CancellationToken::new();
    let pipeline = BatchPipeline::new(config, Some(tx), cancel_token);

    let summary = pipeline.run().await.unwrap();
    assert_eq!(summary.total, 2);
    assert_eq!(summary.completed, 2);

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let start_event = events
        .iter()
        .find_map(|e| match e {
            JobEvent::BatchStart { total, items, .. } => Some((*total, items.clone())),
            _ => None,
        })
        .expect("Expected BatchStart event");

    assert_eq!(start_event.0, 2);
    let items = start_event.1.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].file, "[SubsPlease] Show - 03 (1080p).mkv");
    assert_eq!(items[1].file, "[SubsPlease] Show - 01 (1080p).mkv");
}

#[tokio::test]
async fn test_pipeline_rejects_every_unmatched_selection_before_batch_start() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"video").unwrap();
    fs::write(input.path().join("notes.txt"), b"text").unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden").unwrap();

    for mode in [PipelineMode::Extract, PipelineMode::Remux] {
        let config = PipelineConfig {
            mode,
            input_dir: input.path().to_path_buf(),
            out_dir: Some(output.path().to_path_buf()),
            selected_items: Some(vec![
                "Show - 01.mkv".into(),
                "notes.txt".into(),
                "Output/hidden.mkv".into(),
                "ghost.mkv".into(),
            ]),
            ..Default::default()
        };
        let (tx, mut rx) = create_event_channel(16);
        let pipeline = BatchPipeline::new(config, Some(tx), CancellationToken::new());
        let err = pipeline.run().await.unwrap_err();
        for unmatched in ["notes.txt", "Output/hidden.mkv", "ghost.mkv"] {
            assert!(err.contains(unmatched), "{err}");
        }
        assert!(
            rx.try_recv().is_err(),
            "invalid selection must not start a batch"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
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
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    JobEvent::BatchComplete {
                        success: false,
                        cancelled: false,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
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

#[tokio::test]
async fn test_pipeline_extract_persists_font_hash_memo() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();
    let memo_file = out_tmp.path().join("test_font_memo.json");

    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv content").unwrap();

    // Create an existing font in the input's fonts directory
    let fonts_dir = in_tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();
    let font_file = fonts_dir.join("CustomFont.ttf");
    fs::write(&font_file, b"font binary data").unwrap();

    let cache = Arc::new(PersistentInspectCache::new(None, false));
    cache.put_path(&ep, sample_video_metadata());

    let memo = Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        memo_file.clone(),
    )));

    let config = PipelineConfig {
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: Some(out_tmp.path().to_path_buf()),
        fonts_dir: Some(fonts_dir.clone()),
        mode: PipelineMode::Extract,
        dry_run: false,
        no_cache: false,
        cache: Some(cache),
        font_hash_memo: Some(memo.clone()),
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (tx, _rx) = create_event_channel(32);
    let pipeline = BatchPipeline::new(config, Some(tx), cancel_token);

    let summary = pipeline.run().await.unwrap();
    assert_eq!(summary.total, 1);

    // Verify that the memo file was saved to disk
    assert!(
        memo_file.exists(),
        "font hash memo file should be persisted to disk"
    );

    // Re-load the memo file from disk to confirm it contains the indexed font
    let reloaded = FontHashMemo::new(memo_file);
    let sig = mkv_core::scan::FileSignature::of(&font_file).unwrap();
    let cached_hash = reloaded.get(&font_file, sig);
    assert!(
        cached_hash.is_some(),
        "persisted memo must contain font hash"
    );
}

fn media_command(command: &mut Command) {
    let output = command.output().expect("media tool must be installed");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn remux_media_case(input: &Path, assets: &Path, output: &Path) -> (String, String, Vec<u8>) {
    let pipeline = BatchPipeline::new(
        PipelineConfig {
            mode: PipelineMode::Remux,
            input_dir: input.to_path_buf(),
            out_dir: Some(output.to_path_buf()),
            assets_dir: Some(assets.to_path_buf()),
            no_cache: true,
            ..Default::default()
        },
        None,
        CancellationToken::new(),
    );
    let summary = pipeline.run().await.unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (1, 0),
        "{:?}",
        summary.log
    );
    let mkv = output.join("Season 01/Show - 01.mkv");
    let metadata = mkv_core::scan::probe_video_mkvmerge(&mkv).await.unwrap();
    let subtitle = metadata
        .tracks
        .iter()
        .find(|track| track.track_type == "subtitles")
        .unwrap();
    let subtitle_id = subtitle.id.unwrap();
    let subtitle_path = output.join("selected.srt");
    media_command(
        Command::new("mkvextract")
            .arg("tracks")
            .arg(&mkv)
            .arg(format!("{subtitle_id}:{}", subtitle_path.display())),
    );
    let cover = metadata
        .attachments
        .iter()
        .find(|attachment| {
            attachment
                .file_name
                .as_deref()
                .is_some_and(|name| name.starts_with("cover."))
        })
        .unwrap();
    let extracted = output.join("selected-cover");
    media_command(
        Command::new("mkvextract")
            .arg("attachments")
            .arg(&mkv)
            .arg(format!("{}:{}", cover.id.unwrap(), extracted.display())),
    );
    (
        subtitle.properties.language.clone().unwrap(),
        fs::read_to_string(subtitle_path).unwrap(),
        fs::read(extracted).unwrap(),
    )
}

#[tokio::test]
async fn test_real_remux_subtitle_ladder_and_advertised_cover_names() {
    if ["ffmpeg", "mkvmerge", "mkvextract"]
        .iter()
        .any(|binary| Command::new(binary).arg("--version").output().is_err())
    {
        eprintln!("Skipping real-media regression: FFmpeg/MKVToolNix not installed");
        return;
    }
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input");
    let season = input.join("Season 01");
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let video = season.join("Show - 01.mkv");
    let embedded = tmp.path().join("embedded.srt");
    fs::write(
        &embedded,
        b"1\n00:00:00,000 --> 00:00:00,900\nEMBEDDED INDONESIAN\n",
    )
    .unwrap();
    let raw = tmp.path().join("raw.mkv");
    media_command(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1:d=1",
                "-i",
            ])
            .arg(&embedded)
            .args([
                "-map",
                "0:v",
                "-map",
                "1:s",
                "-c:v",
                "mpeg4",
                "-c:s",
                "srt",
                "-metadata:s:s:0",
                "language=ind",
                "-y",
            ])
            .arg(&raw),
    );
    let source_art = tmp.path().join("source.png");
    fs::write(&source_art, b"SOURCE COVER").unwrap();
    let source_font = tmp.path().join("source.ttf");
    fs::write(&source_font, b"SOURCE FONT").unwrap();
    media_command(
        Command::new("mkvmerge")
            .arg("-o")
            .arg(&video)
            .args([
                "--attachment-name",
                "cover.png",
                "--attachment-mime-type",
                "image/png",
                "--attach-file",
            ])
            .arg(&source_art)
            .args([
                "--attachment-name",
                "Test.ttf",
                "--attachment-mime-type",
                "application/x-truetype-font",
                "--attach-file",
            ])
            .arg(&source_font)
            .arg(&raw),
    );

    let local = season.join("Show - 01.eng.srt");
    let parent = input.join("Show - 01.eng.srt");
    fs::write(&local, b"1\n00:00:00,000 --> 00:00:00,900\nLOCAL ENGLISH\n").unwrap();
    fs::write(
        &parent,
        b"1\n00:00:00,000 --> 00:00:00,900\nPARENT ENGLISH\n",
    )
    .unwrap();
    fs::write(assets.join("cover.jpg"), b"FALLBACK COVER").unwrap();
    fs::write(
        assets.join("Show - 01.eng.srt"),
        b"1\n00:00:00,000 --> 00:00:00,900\nFALLBACK ENGLISH\n",
    )
    .unwrap();

    for (idx, name) in [
        "cover.jpg",
        "folder.jpg",
        "Show - 01_cover.jpg",
        "Show - 01.cover.jpg",
        "Show - 01.jpg",
    ]
    .iter()
    .enumerate()
    {
        let cover = season.join(name);
        let expected = format!("LOCAL COVER {idx}");
        fs::write(&cover, expected.as_bytes()).unwrap();
        let output = tmp.path().join(format!("out-{idx}"));
        let (language, subtitle, attachment) = remux_media_case(&input, &assets, &output).await;
        assert_eq!(language, "eng");
        assert!(subtitle.contains("LOCAL ENGLISH"));
        assert_eq!(attachment, expected.as_bytes());
        if idx == 0 {
            let metadata =
                mkv_core::scan::probe_video_mkvmerge(&output.join("Season 01/Show - 01.mkv"))
                    .await
                    .unwrap();
            assert!(metadata
                .attachments
                .iter()
                .any(|att| att.file_name.as_deref() == Some("Test.ttf")));
        }
        fs::remove_file(cover).unwrap();
    }

    let extract_out = tmp.path().join("extracted");
    let font_out = extract_out.join("Season 01/fonts");
    fs::create_dir_all(&font_out).unwrap();
    let user_font = font_out.join("User.ttf");
    fs::write(&user_font, b"USER FONT").unwrap();
    let extract = BatchPipeline::new(
        PipelineConfig {
            mode: PipelineMode::Extract,
            input_dir: input.clone(),
            out_dir: Some(extract_out),
            no_cache: true,
            ..Default::default()
        },
        None,
        CancellationToken::new(),
    );
    let extracted = extract.run().await.unwrap();
    assert_eq!(
        (extracted.completed, extracted.errors),
        (1, 0),
        "{:?}",
        extracted.log
    );
    assert_eq!(fs::read(font_out.join("Test.ttf")).unwrap(), b"SOURCE FONT");
    assert_eq!(fs::read(user_font).unwrap(), b"USER FONT");

    fs::remove_file(&local).unwrap();
    let ancestor_cover = input.join("folder.jpg");
    fs::write(&ancestor_cover, b"ANCESTOR COVER").unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("ancestor")).await;
    assert_eq!(language, "ind");
    assert!(subtitle.contains("EMBEDDED INDONESIAN"));
    assert_eq!(attachment, b"ANCESTOR COVER");

    fs::remove_file(ancestor_cover).unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("embedded")).await;
    assert_eq!(language, "ind");
    assert!(subtitle.contains("EMBEDDED INDONESIAN"));
    assert_eq!(attachment, b"SOURCE COVER");

    let no_embedded = tmp.path().join("no-embedded.mkv");
    media_command(
        Command::new("mkvmerge")
            .arg("-o")
            .arg(&no_embedded)
            .args(["--no-subtitles", "--no-attachments"])
            .arg(&video),
    );
    fs::copy(no_embedded, &video).unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("fallback")).await;
    assert_eq!(language, "eng");
    assert!(subtitle.contains("PARENT ENGLISH"));
    assert_eq!(attachment, b"FALLBACK COVER");
}
