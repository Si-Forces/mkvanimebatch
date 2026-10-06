//! Fixtures shared by the integration test binaries. Each binary uses a subset, hence the
//! `dead_code` allowance.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::media_tool::{Invocation, MediaTool, MediaToolRunner, RawOutcome};
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, JobEvent, PipelineConfig,
    PipelineSummary,
};
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};
use parking_lot::Mutex;

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

/// A file system effect of a scripted invocation, given the invocation's arguments.
type SideEffect = Box<dyn Fn(&[String]) + Send + Sync>;

/// One scripted Media Tool invocation: the stdout lines the runner feeds the sink, a file
/// system side effect, then the outcome it reports.
pub struct Step {
    outcome: RawOutcome,
    stdout_lines: Vec<String>,
    side_effect: Option<SideEffect>,
}

impl Step {
    pub fn new(outcome: RawOutcome) -> Self {
        Self {
            outcome,
            stdout_lines: Vec::new(),
            side_effect: None,
        }
    }

    /// The tool exits with `code`, having printed `stdout` and `stderr`.
    pub fn exited(code: i32, stdout: &str, stderr: &str) -> Self {
        Self::new(RawOutcome::Exited {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    /// Feeds `lines` to the stdout sink, as the tool prints them while running.
    pub fn stdout_lines(mut self, lines: &[&str]) -> Self {
        self.stdout_lines = lines.iter().map(|line| line.to_string()).collect();
        self
    }

    /// Runs `effect` on the invocation's arguments before the outcome is reported.
    pub fn side_effect(mut self, effect: impl Fn(&[String]) + Send + Sync + 'static) -> Self {
        self.side_effect = Some(Box::new(effect));
        self
    }

    /// Writes `content` to the file named after `-o`, as mkvmerge writes its output.
    pub fn writes_output(self, content: &'static [u8]) -> Self {
        self.side_effect(move |args| {
            let at = args
                .iter()
                .position(|arg| arg == "-o")
                .expect("no -o argument");
            std::fs::write(&args[at + 1], content).unwrap();
        })
    }

    /// Writes `content` to every `<id>:<path>` argument, as mkvextract writes each target.
    pub fn writes_extract_targets(self, content: &'static [u8]) -> Self {
        self.side_effect(move |args| {
            for arg in args {
                if let Some((id, path)) = arg.split_once(':') {
                    if id.parse::<u32>().is_ok() {
                        std::fs::write(path, content).unwrap();
                    }
                }
            }
        })
    }
}

/// A [`MediaToolRunner`] that spawns nothing: it records each invocation and answers with the
/// next scripted [`Step`], panicking when the script runs out.
pub struct ScriptedRunner {
    steps: Mutex<VecDeque<Step>>,
    calls: Mutex<Vec<(MediaTool, Vec<String>)>>,
}

impl ScriptedRunner {
    pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The tool and arguments of every invocation so far, in order.
    pub fn calls(&self) -> Vec<(MediaTool, Vec<String>)> {
        self.calls.lock().clone()
    }
}

impl MediaToolRunner for ScriptedRunner {
    fn run<'a>(
        &'a self,
        invocation: Invocation<'a>,
    ) -> Pin<Box<dyn Future<Output = RawOutcome> + Send + 'a>> {
        Box::pin(async move {
            let args: Vec<String> = invocation
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls.lock().push((invocation.tool, args.clone()));
            let step = self
                .steps
                .lock()
                .pop_front()
                .expect("unscripted Media Tool invocation");
            if let Some(on_line) = invocation.on_stdout_line {
                for line in &step.stdout_lines {
                    on_line(line);
                }
            }
            if let Some(effect) = &step.side_effect {
                effect(&args);
            }
            step.outcome
        })
    }
}
