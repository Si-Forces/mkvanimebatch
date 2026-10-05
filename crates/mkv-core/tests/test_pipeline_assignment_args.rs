//! Proves that manual `TrackAssignment` language, title, and sync offsets survive
//! the full remux pipeline into the argv handed to `mkvmerge`, and that an
//! assignment's `offset_ms` replaces `default_sub_offset_ms` (ADR-0012).
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

use common::{run_batch, seeded_cache, video_metadata, EnvReset};
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::request::{AssignmentKind, TrackAssignment};

#[tokio::test]
async fn assigned_language_title_and_offsets_reach_the_mkvmerge_argv() {
    let dir = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();
    let names = [
        "Episode - 01.mkv",
        "Episode - 02.mkv",
        "Episode - 03.mkv",
        "Episode - 04.mkv",
    ];
    let video_paths: Vec<_> = names.iter().map(|name| dir.path().join(name)).collect();
    for path in &video_paths {
        fs::write(path, b"dummy mkv content").unwrap();
    }
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

    let _env_guard = EnvReset::capture(&["PATH", "MKV_ARGS_FILE"]);
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin_dir.path().display()));
    std::env::set_var("MKV_ARGS_FILE", &args_file);

    // AAC audio is muxed as-is, so the run never needs a real ffmpeg.
    let videos: Vec<_> = video_paths.iter().map(|p| p.as_path()).collect();
    let cache = seeded_cache(dir.path(), &videos, &video_metadata("A_AAC"));
    // One assignment per row of the ADR-0012 offset table: a nonzero offset of
    // either sign replaces the default, an explicit zero suppresses it, and no
    // offset inherits it.
    let offsets = [Some(1250), Some(-1250), Some(0), None];
    let assignment = |name: &str, offset_ms: Option<i64>| TrackAssignment {
        target_video: name.into(),
        asset_path: "custom.ass".into(),
        track_type: AssignmentKind::Subtitle,
        language: Some("ind".into()),
        title: Some("Bahasa Indonesia".into()),
        offset_ms,
    };
    let config = PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: dir.path().to_path_buf(),
        out_dir: Some(dir.path().join("out")),
        cache: Some(cache),
        track_assignments: Some(
            names
                .iter()
                .zip(offsets)
                .map(|(name, offset_ms)| assignment(name, offset_ms))
                .collect(),
        ),
        default_sub_offset_ms: 3000,
        // The selected batch proves that every assignment resolves to its
        // requested item.
        selected_items: Some(names.iter().map(|name| name.to_string()).collect()),
        workers: 1,
        io_jobs: 1,
        ..Default::default()
    };

    let (summary, _events) = run_batch(config, CancellationToken::new()).await;
    let summary = summary.unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (4, 0),
        "the assigned subtitles must be muxed, not skipped"
    );

    let args: Vec<String> = fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    // Workers run in any order, so pair each mkvmerge call with its source video.
    let calls: Vec<&[String]> = args
        .split(|line| line == "__CALL__")
        .filter(|call| !call.is_empty())
        .collect();
    let syncs_for = |name: &str| -> Vec<&String> {
        let call = calls
            .iter()
            .find(|call| call.iter().any(|arg| arg.ends_with(name)))
            .unwrap_or_else(|| panic!("no mkvmerge call for {name}: {args:?}"));
        call.windows(2)
            .filter(|pair| pair[0] == "--sync")
            .map(|pair| &pair[1])
            .collect()
    };
    assert_eq!(
        syncs_for(names[0]),
        ["0:1250"],
        "positive offset_ms must replace the default"
    );
    assert_eq!(
        syncs_for(names[1]),
        ["0:-1250"],
        "negative offset_ms must replace the default"
    );
    assert!(
        syncs_for(names[2]).is_empty(),
        "an explicit zero offset_ms must suppress the default"
    );
    assert_eq!(
        syncs_for(names[3]),
        ["0:3000"],
        "no offset_ms must inherit default_sub_offset_ms"
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
