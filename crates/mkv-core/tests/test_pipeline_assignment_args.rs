//! Proves that manual `TrackAssignment` language, title, and sync offsets survive
//! the full remux pipeline into the argv handed to `mkvmerge`.
//!
//! This lives in its own integration binary on purpose. Capturing mkvmerge's
//! arguments means putting a stub script ahead of the real one on `PATH`, and
//! `PATH` is process-global: in a shared binary every other test that can reach
//! a probe or a remux would race it for the same variable. One test per process
//! makes the mutation harmless, so no lock is needed and no unrelated test has
//! to be serialized behind this one. It is Unix-only because the harness uses a
//! POSIX shell script and Unix executable permissions; consequently this argv
//! contract has no Windows coverage.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use common::{run_batch, seeded_cache, video_metadata};
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::TrackAssignment;

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

#[tokio::test]
async fn assigned_language_title_and_both_offset_signs_reach_the_mkvmerge_argv() {
    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let video_path = dir.path().join("Episode - 01.mkv");
    let second_video_path = dir.path().join("Episode - 02.mkv");
    fs::write(&video_path, b"dummy mkv content").unwrap();
    fs::write(&second_video_path, b"dummy mkv content").unwrap();
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
        "#!/bin/sh\nprintf '__CALL__\\n' >> \"$MKV_ARGS_FILE\"\nprintf '%s\\n' \"$@\" >> \"$MKV_ARGS_FILE\"\nout=\"\"\nprev=\"\"\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then out=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nmkdir -p \"$(dirname \"$out\")\"\nprintf 'fake mkv' > \"$out\"\n",
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

    // AAC audio is muxed as-is, so the run never needs a real ffmpeg.
    let cache = seeded_cache(
        dir.path(),
        &[&video_path, &second_video_path],
        &video_metadata("A_AAC"),
    );
    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: dir.path().to_path_buf(),
        out_dir: Some(dir.path().join("out")),
        cache: Some(cache),
        track_assignments: Some(vec![
            TrackAssignment {
                target_video: "Episode - 01.mkv".into(),
                asset_path: "custom.ass".into(),
                track_type: Some("subtitle".into()),
                language: Some("ind".into()),
                title: Some("Bahasa Indonesia".into()),
                offset_ms: Some(1250),
            },
            TrackAssignment {
                target_video: "Episode - 02.mkv".into(),
                asset_path: "custom.ass".into(),
                track_type: Some("subtitle".into()),
                language: Some("ind".into()),
                title: Some("Bahasa Indonesia".into()),
                offset_ms: Some(-1250),
            },
        ]),
        // The selected batch proves that both assignments resolve to the
        // requested items and that both offset signs reach the argv.
        selected_items: Some(vec!["Episode - 01.mkv".into(), "Episode - 02.mkv".into()]),
        workers: 1,
        io_jobs: 1,
        ..Default::default()
    };

    let (summary, _events) = run_batch(config, CancellationToken::new()).await;
    let summary = summary.unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (2, 0),
        "the assigned subtitles must be muxed, not skipped"
    );

    let args: Vec<String> = fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        args.windows(2).any(|pair| pair == ["--sync", "0:1250"]),
        "positive offset_ms must reach mkvmerge: {args:?}"
    );
    assert!(
        args.windows(2).any(|pair| pair == ["--sync", "0:-1250"]),
        "negative offset_ms must reach mkvmerge: {args:?}"
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
