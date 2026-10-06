mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use common::{run_batch, seeded_cache, video_metadata};
use mkv_core::fonts::FontHashMemo;
use mkv_core::pipeline::{
    create_event_channel, create_log_channel, BatchPipeline, CancellationToken, ItemOutcome,
    JobEvent, PipelineConfig, PipelineMode,
};

fn config(mode: PipelineMode, input_dir: &Path) -> PipelineConfig {
    PipelineConfig {
        mode,
        input_dir: input_dir.to_path_buf(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_extract_dry_run_emits_planned_events() {
    let tmp = tempdir().unwrap();
    let season_dir = tmp.path().join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();
    fs::write(tmp.path().join("Show - 01.mkv"), b"dummy content 1").unwrap();
    fs::write(season_dir.join("Show - 02.mkv"), b"dummy content 2").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            dry_run: true,
            no_cache: true,
            workers: 2,
            io_jobs: 1,
            ..config(PipelineMode::Extract, tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 2);
    assert_eq!(summary.completed, 2);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.errors, 0);
    assert!(!summary.cancelled);

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

    let planned_items: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::ItemComplete {
                id,
                status: ItemOutcome::Planned,
                ..
            } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(planned_items.len(), 2);
    assert!(planned_items.contains(&"Show - 01.mkv"));
    assert!(planned_items.contains(&"Season 01/Show - 02.mkv"));
}

#[tokio::test]
async fn test_remux_dry_run_preserves_nested_output_directory() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();

    let season_dir = in_tmp.path().join("Season 02");
    fs::create_dir_all(&season_dir).unwrap();
    let ep1 = season_dir.join("Show - 01.mkv");
    fs::write(&ep1, b"dummy mkv").unwrap();
    fs::write(
        season_dir.join("Show - 01.ass"),
        b"[Script Info]\nTitle: Test Sub",
    )
    .unwrap();

    // A non-existent out_dir is valid input, and a dry run must not create it.
    let out_dir = out_tmp.path().join("not-yet-created");
    let (summary, events) = run_batch(
        PipelineConfig {
            out_dir: Some(out_dir.clone()),
            dry_run: true,
            cache: Some(seeded_cache(
                in_tmp.path(),
                &[&ep1],
                &video_metadata("A_FLAC"),
            )),
            ..config(PipelineMode::Remux, in_tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);
    assert!(
        !out_dir.exists(),
        "dry run created the output directory {}",
        out_dir.display()
    );

    let expected_nested_path = out_dir.join("Season 02").join("Show - 01.mkv");
    let expected_nested_str = expected_nested_path.to_string_lossy().into_owned();
    let expected_nested_slash = expected_nested_str.replace('\\', "/");
    assert!(
        summary.log.iter().any(
            |line| line.contains(&expected_nested_str) || line.contains(&expected_nested_slash)
        ),
        "Log should contain preserved nested output path '{}'. Actual log:\n{:#?}",
        expected_nested_path.display(),
        summary.log
    );

    assert!(events.iter().any(|e| matches!(
        e,
        JobEvent::ItemComplete {
            id,
            status: ItemOutcome::Planned,
            duration_sec: None,
            ..
        } if id.as_str() == "Season 02/Show - 01.mkv"
    )));
}

/// An input directory reached through a different spelling than its canonical path (a symlink
/// here; an 8.3 short name or verbatim prefix on Windows) must still pair each video with the
/// subtitle beside it.
#[cfg(unix)]
#[tokio::test]
async fn test_remux_dry_run_matches_subtitles_through_symlinked_input_dir() {
    let tmp = tempdir().unwrap();
    let release = tmp.path().join("release");
    let season_dir = release.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();
    let ep1 = season_dir.join("Show - 01.mkv");
    fs::write(&ep1, b"dummy mkv").unwrap();
    fs::write(
        season_dir.join("Show - 01.ass"),
        b"[Script Info]\nTitle: Test Sub",
    )
    .unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&release, &link).unwrap();

    let (summary, _) = run_batch(
        PipelineConfig {
            dry_run: true,
            cache: Some(seeded_cache(tmp.path(), &[&ep1], &video_metadata("A_FLAC"))),
            ..config(PipelineMode::Remux, &link)
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(
        (summary.completed, summary.skipped),
        (1, 0),
        "{:#?}",
        summary.log
    );
}

#[tokio::test]
async fn test_remux_skips_when_no_matching_subtitle() {
    let in_tmp = tempdir().unwrap();
    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            dry_run: true,
            cache: Some(seeded_cache(
                in_tmp.path(),
                &[&ep],
                &video_metadata("A_FLAC"),
            )),
            ..config(PipelineMode::Remux, in_tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.errors, 0);
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
async fn test_remux_planning_outcomes_pair_nested_id_with_file_name() {
    let in_tmp = tempdir().unwrap();
    let season_dir = in_tmp.path().join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();
    // Probes from the seeded cache but has no subtitle: skipped by the Asset Resolver.
    let no_sub = season_dir.join("Show - 01.mkv");
    // Not seeded and not a real MKV: its probe fails.
    let corrupt = season_dir.join("Show - 02.mkv");
    // No episode number: skipped before probing.
    let unparsed = season_dir.join("Extras.mkv");
    for video in [&no_sub, &corrupt, &unparsed] {
        fs::write(video, b"dummy mkv").unwrap();
    }

    let (summary, events) = run_batch(
        PipelineConfig {
            dry_run: true,
            cache: Some(seeded_cache(
                in_tmp.path(),
                &[&no_sub],
                &video_metadata("A_FLAC"),
            )),
            ..config(PipelineMode::Remux, in_tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();
    assert_eq!((summary.skipped, summary.errors), (2, 1));

    // A nested ID never equals its file name, so a swapped pair fails here.
    let mut outcomes: Vec<(&str, &str, ItemOutcome)> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::ItemComplete {
                id, file, status, ..
            } => Some((id.as_str(), file.as_str(), *status)),
            _ => None,
        })
        .collect();
    outcomes.sort_by_key(|(id, ..)| *id);
    assert_eq!(
        outcomes,
        [
            ("Season 01/Extras.mkv", "Extras.mkv", ItemOutcome::Skipped),
            (
                "Season 01/Show - 01.mkv",
                "Show - 01.mkv",
                ItemOutcome::Skipped
            ),
            (
                "Season 01/Show - 02.mkv",
                "Show - 02.mkv",
                ItemOutcome::Error
            ),
        ]
    );
}

#[tokio::test]
async fn test_pipeline_cancellation_drains_items() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("Show - 01.mkv"), b"dummy mkv 1").unwrap();
    fs::write(tmp.path().join("Show - 02.mkv"), b"dummy mkv 2").unwrap();

    let cancel_token = CancellationToken::new();
    cancel_token.cancel();
    let (summary, events) = run_batch(
        PipelineConfig {
            dry_run: true,
            no_cache: true,
            workers: 1,
            ..config(PipelineMode::Extract, tmp.path())
        },
        cancel_token,
    )
    .await;

    assert!(summary.unwrap().cancelled);
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
    let (log_sender, mut log_receiver) = create_log_channel(64);
    let pipeline = BatchPipeline::new(
        // Explicit, so the test stays off the user's cache even if the empty-batch return moves.
        PipelineConfig {
            no_cache: true,
            ..config(PipelineMode::Extract, dir.path())
        },
        None,
        CancellationToken::new(),
    )
    .with_log_sender(log_sender);

    let summary = pipeline.run().await.unwrap();
    assert_eq!(summary.total, 0);

    let first_log = log_receiver.try_recv().expect("expected a log line");
    assert!(first_log.contains("No MKV files found in"), "{first_log}");
}

#[tokio::test]
async fn test_extract_emits_item_complete_duration_sec() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();
    let ep1 = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep1, b"dummy mkv").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            out_dir: Some(out_tmp.path().to_path_buf()),
            cache: Some(seeded_cache(
                in_tmp.path(),
                &[&ep1],
                &video_metadata("A_FLAC"),
            )),
            // Keeps the run off the user's default font hash memo.
            font_hash_memo: Some(Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
                in_tmp.path().join("font_hash_memo.json"),
            )))),
            workers: 1,
            io_jobs: 1,
            ..config(PipelineMode::Extract, in_tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);

    let complete_events: Vec<&JobEvent> = events
        .iter()
        .filter(|e| matches!(e, JobEvent::ItemComplete { .. }))
        .collect();
    assert_eq!(complete_events.len(), 1);
    assert!(
        matches!(
            complete_events[0],
            JobEvent::ItemComplete {
                id,
                status: ItemOutcome::Completed,
                duration_sec: Some(d),
                ..
            } if id.as_str() == "Show - 01.mkv" && *d >= 0.0
        ),
        "{:?}",
        complete_events[0]
    );
}

#[tokio::test]
async fn test_extract_error_in_worker_emits_duration_sec() {
    let tmp = tempdir().unwrap();
    // Not a real MKV, so probing fails inside the worker.
    fs::write(tmp.path().join("Corrupt - 01.mkv"), b"not a real mkv file").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            no_cache: true,
            workers: 1,
            io_jobs: 1,
            ..config(PipelineMode::Extract, tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.errors, 1);
    assert!(
        events.iter().any(|e| matches!(
            e,
            JobEvent::ItemComplete {
                id,
                status: ItemOutcome::Error,
                duration_sec: Some(d),
                ..
            } if id.as_str() == "Corrupt - 01.mkv" && *d >= 0.0
        )),
        "expected an Error completion carrying duration_sec: {events:?}"
    );
}

#[tokio::test]
async fn test_pipeline_in_flight_cancellation_emits_duration_sec() {
    let tmp = tempdir().unwrap();
    for i in 1..=20 {
        fs::write(tmp.path().join(format!("Show - {i:02}.mkv")), b"dummy mkv").unwrap();
    }
    let batch_config = PipelineConfig {
        no_cache: true,
        workers: 1,
        io_jobs: 1,
        ..config(PipelineMode::Extract, tmp.path())
    };

    // A one-slot channel parks the pipeline on its first `ItemStart` while that item is
    // already in flight, so the stop below lands mid-item.
    let cancel_token = CancellationToken::new();
    let (event_sender, mut event_receiver) = create_event_channel(1);
    let cancel_token_clone = cancel_token.clone();
    let runner = tokio::spawn(async move {
        BatchPipeline::new(batch_config, Some(event_sender), cancel_token)
            .run()
            .await
            .unwrap()
    });

    let mut events = Vec::new();
    while let Some(event) = event_receiver.recv().await {
        if matches!(event, JobEvent::BatchStart { .. }) {
            cancel_token_clone.cancel();
        }
        let is_terminal = matches!(event, JobEvent::BatchComplete { .. });
        events.push(event);
        if is_terminal {
            break;
        }
    }

    assert!(runner.await.unwrap().cancelled);
    assert!(
        events.iter().any(|e| matches!(
            e,
            JobEvent::ItemComplete {
                status: ItemOutcome::Cancelled,
                duration_sec: Some(d),
                ..
            } if *d >= 0.0
        )),
        "expected an in-flight cancelled item with duration_sec: {events:?}"
    );
}

#[tokio::test]
async fn test_pipeline_selected_items_filters_and_orders_batch() {
    let dir = tempdir().unwrap();
    for ep in 1..=3 {
        fs::write(
            dir.path()
                .join(format!("[SubsPlease] Show - 0{ep} (1080p).mkv")),
            b"dummy",
        )
        .unwrap();
    }

    let (summary, events) = run_batch(
        PipelineConfig {
            dry_run: true,
            no_cache: true,
            selected_items: Some(vec![
                "[SubsPlease] Show - 03 (1080p).mkv".into(),
                "[SubsPlease] Show - 01 (1080p).mkv".into(),
            ]),
            ..config(PipelineMode::Extract, dir.path())
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();
    assert_eq!(summary.total, 2);
    assert_eq!(summary.completed, 2);

    let (total, items) = events
        .iter()
        .find_map(|e| match e {
            JobEvent::BatchStart { total, items, .. } => Some((*total, items.clone())),
            _ => None,
        })
        .expect("Expected BatchStart event");
    assert_eq!(total, 2);
    let files: Vec<String> = items.unwrap().into_iter().map(|item| item.file).collect();
    assert_eq!(
        files,
        [
            "[SubsPlease] Show - 03 (1080p).mkv",
            "[SubsPlease] Show - 01 (1080p).mkv"
        ]
    );
}

// Unix-only by construction, not by convenience: a canonical-ID collision needs two
// distinct filesystem paths that normalize to one item ID, which requires a literal
// backslash inside a filename. Windows forbids `\` in a filename, so the scenario is
// unrepresentable there and the Windows leg has nothing to cover.
#[cfg(unix)]
#[tokio::test]
async fn test_batch_rejects_colliding_canonical_ids_before_batch_start() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::create_dir_all(input.path().join("Show")).unwrap();
    fs::write(input.path().join(r"Show\Season 1.mkv"), b"root video").unwrap();
    fs::write(input.path().join("Show/Season 1.mkv"), b"nested video").unwrap();

    for selected_items in [Some(vec!["Show/Season 1.mkv".to_owned()]), None] {
        let (result, events) = run_batch(
            PipelineConfig {
                out_dir: Some(output.path().to_path_buf()),
                selected_items,
                ..config(PipelineMode::Extract, input.path())
            },
            CancellationToken::new(),
        )
        .await;

        assert_eq!(
            result.unwrap_err(),
            "Discovered MKV files share canonical ID 'Show/Season 1.mkv'"
        );
        assert!(events.is_empty(), "collision must abort before BatchStart");
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn test_pipeline_rejects_every_unmatched_selection_before_batch_start() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"video").unwrap();
    fs::write(input.path().join("notes.txt"), b"text").unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden").unwrap();

    for (selected, expected_err_parts) in [
        (
            vec![
                "Show - 01.mkv".into(),
                "notes.txt".into(),
                "Output/hidden.mkv".into(),
                "ghost.mkv".into(),
            ],
            vec!["notes.txt", "Output/hidden.mkv", "ghost.mkv"],
        ),
        (vec!["Output/hidden.mkv".into()], vec!["Output/hidden.mkv"]),
    ] {
        for mode in [PipelineMode::Extract, PipelineMode::Remux] {
            let (result, events) = run_batch(
                PipelineConfig {
                    out_dir: Some(output.path().to_path_buf()),
                    selected_items: Some(selected.clone()),
                    ..config(mode, input.path())
                },
                CancellationToken::new(),
            )
            .await;
            let err = result.unwrap_err();
            for unmatched in &expected_err_parts {
                assert!(err.contains(unmatched), "{err}");
            }
            assert!(
                events.is_empty(),
                "invalid selection must not start a batch"
            );
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
        }
    }
}

#[tokio::test]
async fn test_pipeline_extract_persists_font_hash_memo() {
    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();
    let memo_file = out_tmp.path().join("test_font_memo.json");

    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv content").unwrap();

    let fonts_dir = in_tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();
    let font_file = fonts_dir.join("CustomFont.ttf");
    fs::write(&font_file, b"font binary data").unwrap();

    let memo = Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        memo_file.clone(),
    )));
    // Seeding hashes nothing (#33), and this dummy episode carries no font to promote, so the
    // run itself adds no entry. Leave an unsaved one instead: the run must persist it.
    let sig = mkv_core::scan::FileSignature::of(&font_file).unwrap();
    let hash = mkv_core::fonts::hash_file(&font_file).unwrap();
    memo.lock().put(&font_file, sig, hash);
    let (summary, _events) = run_batch(
        PipelineConfig {
            out_dir: Some(out_tmp.path().to_path_buf()),
            fonts_dir: Some(fonts_dir),
            cache: Some(seeded_cache(
                in_tmp.path(),
                &[&ep],
                &video_metadata("A_FLAC"),
            )),
            font_hash_memo: Some(memo),
            ..config(PipelineMode::Extract, in_tmp.path())
        },
        CancellationToken::new(),
    )
    .await;
    assert_eq!(summary.unwrap().total, 1);

    let reloaded = FontHashMemo::new(memo_file);
    assert!(
        reloaded.get(&font_file, sig).is_some(),
        "persisted memo must contain font hash"
    );
}
