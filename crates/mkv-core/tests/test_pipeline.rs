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
                    ..
                }
            )
        })
        .count();
    assert_eq!(cancelled_count, 2);
}
