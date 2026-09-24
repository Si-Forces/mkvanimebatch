//! Benchmark suite for Media Engine Batch Pipeline.
//! Measures preflight validation throughput, dry-run batch execution,
//! font deduplication, and event streaming performance across scaled workloads.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, PipelineConfig, PipelineMode,
};
use mkv_core::request::{BatchRequest, TrackAssignment};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};
use tempfile::tempdir;

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

fn populate_synthetic_workload(
    root: &Path,
    count: usize,
    cache: &PersistentInspectCache,
) -> Vec<PathBuf> {
    let mut videos = Vec::with_capacity(count);
    let seasons = 4.max(count / 25);

    for i in 0..count {
        let season_idx = i % seasons;
        let season_dir = root.join(format!("Season {season_idx:02}"));
        if !season_dir.exists() {
            fs::create_dir_all(&season_dir).unwrap();
        }

        let ep_num = (i / seasons) + 1;
        let video_name = format!("Show - S{season_idx:02}E{ep_num:03}.mkv");
        let sub_name = format!("Show - S{season_idx:02}E{ep_num:03}.ass");

        let video_path = season_dir.join(&video_name);
        let sub_path = season_dir.join(&sub_name);

        fs::write(&video_path, b"mock mkv container").unwrap();
        fs::write(
            &sub_path,
            b"[Script Info]\nTitle: Benchmark Subtitle\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,Hello world!\n",
        )
        .unwrap();

        cache.put_path(&video_path, mock_video_metadata());
        videos.push(video_path);
    }

    cache.save();
    videos
}

fn bench_validation(runs: usize) {
    let tmp = tempdir().unwrap();
    let video_path = tmp.path().join("ep01.mkv");
    fs::write(&video_path, b"dummy").unwrap();

    let sub_path = tmp.path().join("ep01.ass");
    fs::write(&sub_path, b"dummy").unwrap();

    let req = BatchRequest {
        mode: PipelineMode::Remux,
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
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
        selected_items: None,
    };

    let start = Instant::now();
    for _ in 0..runs {
        req.validate().unwrap();
    }
    let elapsed = start.elapsed();

    let per_op = elapsed.as_secs_f64() / runs as f64;
    let ops_per_sec = runs as f64 / elapsed.as_secs_f64();

    println!(
        "BatchRequest::validate: {} runs in {:.2?} ({:.2} µs/op, {:.0} ops/sec)",
        runs,
        elapsed,
        per_op * 1_000_000.0,
        ops_per_sec
    );
}

async fn bench_batch_pipeline(count: usize, workers: usize) {
    let tmp = tempdir().unwrap();
    let cache_path = tmp.path().join("inspect_cache.json");
    let cache = Arc::new(PersistentInspectCache::new(Some(cache_path), true));

    let _videos = populate_synthetic_workload(tmp.path(), count, &cache);

    // Warm manifest/cache remux dry-run
    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Remux,
        dry_run: true,
        no_cache: false,
        cache: Some(cache.clone()),
        cache_file: None,
        workers,
        io_jobs: 2,
        bitrate: "192k".to_string(),
        complexity: 5,
        ffmpeg_threads: 2,
        sub_offset: 0.0,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_tx, mut event_rx) = create_event_channel(256);

    let event_consumer = tokio::spawn(async move {
        let mut event_count = 0usize;
        while let Some(_event) = event_rx.recv().await {
            event_count += 1;
        }
        event_count
    });

    let pipeline = BatchPipeline::new(config, Some(event_tx), cancel_token);

    let start = Instant::now();
    let summary = pipeline.run().await.unwrap();
    let elapsed = start.elapsed();

    drop(pipeline);
    let events_received = event_consumer.await.unwrap();

    let items_per_sec = count as f64 / elapsed.as_secs_f64();
    println!(
        "BatchPipeline::run [Remux Dry-Run] ({} items, {} workers): {:.2?} ({:.1} items/sec, {} events processed, {} completed, {} skipped, {} errors)",
        count, workers, elapsed, items_per_sec, events_received, summary.completed, summary.skipped, summary.errors
    );
}

async fn bench_extract_pipeline(count: usize, workers: usize) {
    let tmp = tempdir().unwrap();
    let cache_path = tmp.path().join("inspect_cache.json");
    let cache = Arc::new(PersistentInspectCache::new(Some(cache_path), true));

    let _videos = populate_synthetic_workload(tmp.path(), count, &cache);

    let config = PipelineConfig {
        input_dir: tmp.path().to_path_buf(),
        out_dir: None,
        fonts_dir: None,
        assets_dir: None,
        mode: PipelineMode::Extract,
        dry_run: true,
        no_cache: false,
        cache: Some(cache),
        cache_file: None,
        workers,
        io_jobs: 2,
        ..Default::default()
    };

    let cancel_token = CancellationToken::new();
    let (event_tx, mut event_rx) = create_event_channel(256);

    let event_consumer = tokio::spawn(async move {
        let mut count = 0usize;
        while let Some(_event) = event_rx.recv().await {
            count += 1;
        }
        count
    });

    let pipeline = BatchPipeline::new(config, Some(event_tx), cancel_token);

    let start = Instant::now();
    let summary = pipeline.run().await.unwrap();
    let elapsed = start.elapsed();

    drop(pipeline);
    let _events = event_consumer.await.unwrap();
    let items_per_sec = count as f64 / elapsed.as_secs_f64();

    println!(
        "BatchPipeline::run [Extract Dry-Run] ({} items, {} workers): {:.2?} ({:.1} items/sec, {} completed, {} errors)",
        count, workers, elapsed, items_per_sec, summary.completed, summary.errors
    );
}

#[tokio::main]
async fn main() {
    println!("============================================================");
    println!(" MKV Media Engine Benchmark Pipeline (Rust BatchPipeline)   ");
    println!("============================================================");

    let concurrency = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    println!("System hardware concurrency: {} cores/threads", concurrency);
    println!();

    println!("--- 1. Preflight Validation Throughput ---");
    bench_validation(10_000);
    println!();

    println!("--- 2. Batch Remux Pipeline Scaling (Warm Cache) ---");
    for &count in &[50, 250, 1000] {
        bench_batch_pipeline(count, concurrency).await;
    }
    println!();

    println!("--- 3. Batch Extract Pipeline Scaling ---");
    for &count in &[50, 250, 1000] {
        bench_extract_pipeline(count, concurrency).await;
    }
    println!();
    println!("============================================================");
}
