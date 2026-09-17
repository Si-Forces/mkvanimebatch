use std::fs;
use std::sync::Arc;
use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: None,
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
    assert!(pipeline.config.keep_fonts); // baseline from balanced
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
        keep_fonts: false,
        sub_assignments: None,
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
