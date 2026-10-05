//! Fixtures shared by the pipeline integration test binaries.

use std::path::Path;
use std::sync::Arc;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, JobEvent, PipelineConfig,
    PipelineSummary,
};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};

/// Runs a batch to its end and returns its result with every event it emitted. The events are
/// drained while the batch runs, so a batch emitting more events than the channel buffers can
/// never park its sender.
pub async fn run_batch(
    config: PipelineConfig,
    cancel_token: CancellationToken,
) -> (Result<PipelineSummary, String>, Vec<JobEvent>) {
    let (tx, mut rx) = create_event_channel(64);
    let runner = tokio::spawn(async move {
        BatchPipeline::new(config, Some(tx), cancel_token)
            .run()
            .await
    });

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    (runner.await.expect("batch task panicked"), events)
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

/// Restores the environment variables it captured when dropped, so a failing assertion cannot
/// leak a mutated `PATH` (or a stub's own variable) into the rest of the run. Only the binaries
/// that put a stub tool on `PATH` use it, one test per process because the environment is
/// process-global; the others never construct it, hence the `dead_code` allowance.
#[allow(dead_code)]
pub struct EnvReset(Vec<(&'static str, Option<std::ffi::OsString>)>);

#[allow(dead_code)]
impl EnvReset {
    /// Records the current value of each of `vars`, set or not, to restore on drop.
    pub fn capture(vars: &[&'static str]) -> Self {
        Self(
            vars.iter()
                .map(|&var| (var, std::env::var_os(var)))
                .collect(),
        )
    }
}

impl Drop for EnvReset {
    fn drop(&mut self) {
        for (var, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(var, value),
                None => std::env::remove_var(var),
            }
        }
    }
}
