//! Fixtures shared by the pipeline integration test binaries.

use std::path::Path;
use std::sync::Arc;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, JobEvent, PipelineConfig,
    PipelineSummary,
};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};

/// Runs a batch to its end and returns its result with every event it emitted. The event
/// buffer holds every event a small test batch emits, so no concurrent consumer is needed.
pub async fn run_batch(
    config: PipelineConfig,
    cancel_token: CancellationToken,
) -> (Result<PipelineSummary, String>, Vec<JobEvent>) {
    let (tx, mut rx) = create_event_channel(64);
    let result = BatchPipeline::new(config, Some(tx), cancel_token)
        .run()
        .await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (result, events)
}

/// One HEVC video track plus one Japanese stereo audio track. An `A_FLAC` audio codec makes a
/// real (non-dry) remux transcode to Opus through ffmpeg first.
pub fn video_metadata(audio_codec: &str) -> MkvmergeMetadata {
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
                codec: Some(audio_codec.to_string()),
                properties: TrackProperties {
                    language: Some("jpn".to_string()),
                    track_name: Some("Stereo".to_string()),
                    audio_channels: Some(2),
                    codec_id: Some(audio_codec.to_string()),
                    ..Default::default()
                },
            },
        ],
        attachments: vec![],
        chapters: vec![],
    }
}

/// An inspection cache persisted inside `dir` and seeded with `metadata` for every video, so a
/// pipeline run never probes the placeholder files or writes the user's default cache.
pub fn seeded_cache(
    dir: &Path,
    videos: &[&Path],
    metadata: &MkvmergeMetadata,
) -> Arc<PersistentInspectCache> {
    let cache = Arc::new(PersistentInspectCache::new(
        Some(dir.join("inspect_cache.json")),
        true,
    ));
    for video in videos {
        cache.put_path(video, metadata.clone());
    }
    cache
}
