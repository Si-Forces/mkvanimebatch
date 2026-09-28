//! Benchmarks for the batch pipeline: preflight validation throughput and warm-cache
//! dry-run batch execution with event streaming, across scaled synthetic workloads.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, PipelineConfig, PipelineMode,
};
use mkv_core::request::{BatchRequest, TrackAssignment};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};
use tempfile::tempdir;

const WORKLOAD_SIZES: [usize; 3] = [50, 250, 1000];

fn mock_video_metadata() -> MkvmergeMetadata {
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

/// Writes `count` placeholder episodes with matching subtitles across season directories
/// and seeds `cache` with their metadata.
fn populate_synthetic_workload(root: &Path, count: usize, cache: &PersistentInspectCache) {
    let seasons = 4.max(count / 25);
    for i in 0..count {
        let season_idx = i % seasons;
        let season_dir = root.join(format!("Season {season_idx:02}"));
        fs::create_dir_all(&season_dir).unwrap();

        let ep_num = (i / seasons) + 1;
        let stem = format!("Show - S{season_idx:02}E{ep_num:03}");
        let video_path = season_dir.join(format!("{stem}.mkv"));
        fs::write(&video_path, b"mock mkv container").unwrap();
        fs::write(
            season_dir.join(format!("{stem}.ass")),
            b"[Script Info]\nTitle: Benchmark Subtitle\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,Hello world!\n",
        )
        .unwrap();

        cache.put_path(&video_path, mock_video_metadata());
    }
    cache.save();
}

fn bench_validation(runs: usize) {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("ep01.mkv"), b"dummy").unwrap();
    let sub_path = tmp.path().join("ep01.ass");
    fs::write(&sub_path, b"dummy").unwrap();

    let req = BatchRequest {
        mode: PipelineMode::Remux,
        input_dir: tmp.path().to_path_buf(),
        preset: Some("balanced".to_string()),
        bitrate: Some("192k".to_string()),
        complexity: Some(5),
        workers: Some(8),
        io_jobs: Some(2),
        ffmpeg_threads: Some(2),
        sub_offset: Some(0.0),
        dry_run: Some(true),
        no_cache: Some(false),
        assignments: Some(vec![TrackAssignment {
            target_video: "ep01.mkv".to_string(),
            asset_path: sub_path,
            track_type: Some("subtitle".to_string()),
            language: Some("ind".to_string()),
            title: None,
            offset_ms: None,
        }]),
        ..Default::default()
    };

    let start = Instant::now();
    for _ in 0..runs {
        req.validate().unwrap();
    }
    let elapsed = start.elapsed();

    let per_op = elapsed.as_secs_f64() / runs as f64;
    let ops_per_sec = runs as f64 / elapsed.as_secs_f64();
    println!(
        "BatchRequest::validate: {runs} runs in {elapsed:.2?} ({:.2} µs/op, {ops_per_sec:.0} ops/sec)",
        per_op * 1_000_000.0,
    );
}

/// Times a warm-cache dry run of `count` synthetic items while a consumer drains events.
async fn bench_dry_run(mode: PipelineMode, count: usize, workers: usize) {
    let tmp = tempdir().unwrap();
    let cache = Arc::new(PersistentInspectCache::new(
        Some(tmp.path().join("inspect_cache.json")),
        true,
    ));
    populate_synthetic_workload(tmp.path(), count, &cache);

    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        mode,
        dry_run: true,
        cache: Some(cache),
        workers,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = create_event_channel(256);
    let event_consumer = tokio::spawn(async move {
        let mut event_count = 0usize;
        while event_rx.recv().await.is_some() {
            event_count += 1;
        }
        event_count
    });

    let pipeline = BatchPipeline::new(config, Some(event_tx), CancellationToken::new());
    let start = Instant::now();
    let summary = pipeline.run().await.unwrap();
    let elapsed = start.elapsed();

    drop(pipeline);
    let events_received = event_consumer.await.unwrap();

    let items_per_sec = count as f64 / elapsed.as_secs_f64();
    println!(
        "BatchPipeline::run [{mode:?} Dry-Run] ({count} items, {workers} workers): {elapsed:.2?} ({items_per_sec:.1} items/sec, {events_received} events, {} completed, {} skipped, {} errors)",
        summary.completed, summary.skipped, summary.errors
    );
}

#[tokio::main]
async fn main() {
    let concurrency = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    println!("System hardware concurrency: {concurrency} cores/threads");
    println!();

    println!("--- 1. Preflight Validation Throughput ---");
    bench_validation(10_000);
    println!();

    println!("--- 2. Batch Remux Pipeline Scaling (Warm Cache) ---");
    for count in WORKLOAD_SIZES {
        bench_dry_run(PipelineMode::Remux, count, concurrency).await;
    }
    println!();

    println!("--- 3. Batch Extract Pipeline Scaling (Warm Cache) ---");
    for count in WORKLOAD_SIZES {
        bench_dry_run(PipelineMode::Extract, count, concurrency).await;
    }
}
