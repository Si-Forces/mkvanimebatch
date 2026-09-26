//! Proves that a manual `TrackAssignment`'s language, title, and sync offset
//! survive the full remux pipeline into the argv handed to `mkvmerge`.
//!
//! This lives in its own integration binary on purpose. Capturing mkvmerge's
//! arguments means putting a stub script ahead of the real one on `PATH`, and
//! `PATH` is process-global: in a shared binary every other test that can reach
//! a probe or a remux would race it for the same variable. One test per process
//! makes the mutation harmless, so no lock is needed and no unrelated test has
//! to be serialized behind this one.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tempfile::tempdir;

use mkv_core::domain::{MkvTrack, TrackProperties};
use mkv_core::pipeline::{BatchPipeline, CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::TrackAssignment;
use mkv_core::scan::{MkvmergeMetadata, PersistentInspectCache};

/// Restores the process environment it displaced, so a failing assertion cannot
/// leak a mutated `PATH` into the rest of the run.
struct EnvReset {
    path: String,
    args_file: Option<String>,
}

impl Drop for EnvReset {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.path);
        match &self.args_file {
            Some(value) => std::env::set_var("MKV_ARGS_FILE", value),
            None => std::env::remove_var("MKV_ARGS_FILE"),
        }
    }
}

fn video_metadata() -> MkvmergeMetadata {
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
                codec: Some("A_AAC".to_string()),
                properties: TrackProperties {
                    language: Some("jpn".to_string()),
                    track_name: Some("Stereo".to_string()),
                    audio_channels: Some(2),
                    codec_id: Some("A_AAC".to_string()),
                    ..Default::default()
                },
            },
        ],
        attachments: vec![],
        chapters: vec![],
    }
}

#[tokio::test]
async fn assigned_language_title_and_offset_reach_the_mkvmerge_argv() {
    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let video_path = dir.path().join("Episode - 01.mkv");
    fs::write(&video_path, b"dummy mkv content").unwrap();
    fs::write(
        dir.path().join("custom.ass"),
        b"[Script Info]\nTitle: Custom\n",
    )
    .unwrap();

    // Records its own argv, then produces a placeholder output file so the
    // pipeline sees a successful mux.
    let args_file = dir.path().join("mkvmerge.args");
    let script = bin_dir.path().join("mkvmerge");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$MKV_ARGS_FILE\"\nout=\"\"\nprev=\"\"\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then out=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nmkdir -p \"$(dirname \"$out\")\"\nprintf 'fake mkv' > \"$out\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let _env_guard = EnvReset {
        path: old_path.clone(),
        args_file: std::env::var("MKV_ARGS_FILE").ok(),
    };
    std::env::set_var("PATH", format!("{}:{old_path}", bin_dir.path().display()));
    std::env::set_var("MKV_ARGS_FILE", &args_file);

    let cache = Arc::new(PersistentInspectCache::new(None, true));
    cache.put_path(&video_path, video_metadata());
    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: dir.path().to_path_buf(),
        out_dir: Some(dir.path().join("out")),
        cache: Some(cache),
        track_assignments: Some(vec![TrackAssignment {
            target_video: "Episode - 01.mkv".into(),
            asset_path: "custom.ass".into(),
            track_type: Some("subtitle".into()),
            language: Some("ind".into()),
            title: Some("Bahasa Indonesia".into()),
            offset_ms: Some(1250),
        }]),
        // Scoped to a selection so this one run proves both halves of the
        // contract: the assignment resolves to exactly the selected item, and
        // its language/title/offset reach the argv.
        selected_items: Some(vec!["Episode - 01.mkv".into()]),
        workers: 1,
        io_jobs: 1,
        ..Default::default()
    };

    let summary = BatchPipeline::new(config, None, CancellationToken::new())
        .run()
        .await
        .unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (1, 0),
        "the assigned subtitle must be muxed, not skipped"
    );

    let args: Vec<String> = fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        args.windows(2).any(|pair| pair == ["--sync", "0:1250"]),
        "offset_ms must reach mkvmerge: {args:?}"
    );
    assert!(
        args.windows(2).any(|pair| pair == ["--language", "0:ind"]),
        "language must reach mkvmerge: {args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--track-name", "0:Bahasa Indonesia"]),
        "title must reach mkvmerge: {args:?}"
    );
}
