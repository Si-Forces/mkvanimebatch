mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use common::{run_batch, seeded_cache, video_metadata, ScriptedRunner, Step};
use mkv_core::fonts::FontHashMemo;
use mkv_core::media_tool::MediaTool;
use mkv_core::pipeline::{
    create_event_channel, create_log_channel, BatchPipeline, CancellationToken, ItemOutcome,
    JobEvent, LogLine, PipelineConfig, PipelineMode,
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
        summary
            .log
            .iter()
            .any(|line| line.text.contains(&expected_nested_str)
                || line.text.contains(&expected_nested_slash)),
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
    assert!(
        first_log.text.contains("No MKV files found in"),
        "{first_log:?}"
    );
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

// Unix-only: a canonical-ID collision needs two distinct paths that normalize to one Item ID,
// which takes a literal backslash in a filename. Windows forbids `\` in a filename.
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

// Leading whitespace is part of a name, so " a.mkv" and "a.mkv" are two items rather than
// one shared ID. Unlike the backslash collision above, this is representable on every platform.
#[tokio::test]
async fn test_files_differing_by_leading_space_are_two_items() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join(" a.mkv"), b"spaced video").unwrap();
    fs::write(tmp.path().join("a.mkv"), b"plain video").unwrap();

    for (selected_items, expected_ids) in [
        (None, vec![" a.mkv", "a.mkv"]),
        (Some(vec![" a.mkv".to_owned()]), vec![" a.mkv"]),
    ] {
        let (summary, events) = run_batch(
            PipelineConfig {
                dry_run: true,
                no_cache: true,
                selected_items,
                ..config(PipelineMode::Extract, tmp.path())
            },
            CancellationToken::new(),
        )
        .await;
        let summary = summary.unwrap();
        assert_eq!(
            (summary.total, summary.completed, summary.errors),
            (expected_ids.len(), expected_ids.len(), 0)
        );

        let mut planned: Vec<&str> = events
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
        planned.sort_unstable();
        assert_eq!(planned, expected_ids);
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

// Linux only: the colliding sources differ only by case, which takes a case-sensitive file
// system; the default Windows and macOS ones are not.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_remux_rejects_colliding_destinations_before_batch_start() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let names = [
        "Show - 01.mkv",
        "Show - 01.MKV",
        "Season 1/Show - 02.mkv",
        "season 1/Show - 02.mkv",
        "Show - 03.mkv",
    ];
    let videos: Vec<_> = names.iter().map(|name| input.path().join(name)).collect();
    for video in &videos {
        fs::create_dir_all(video.parent().unwrap()).unwrap();
        fs::write(video, b"video").unwrap();
    }
    let video_refs: Vec<&Path> = videos.iter().map(|video| video.as_path()).collect();
    let dry_run = |out_dir: Option<&Path>, selected_items: Option<Vec<String>>| {
        run_batch(
            PipelineConfig {
                out_dir: out_dir.map(Path::to_path_buf),
                selected_items,
                dry_run: true,
                cache: Some(seeded_cache(
                    cache_dir.path(),
                    &video_refs,
                    &video_metadata("A_AAC"),
                )),
                ..config(PipelineMode::Remux, input.path())
            },
            CancellationToken::new(),
        )
    };

    // Beside each source, `Show - 01` collides byte for byte and `Show - 02` by case only. Under
    // --out-dir the mirrored `Season 1/` and `season 1/` collide by case.
    for (out_dir, destinations) in [
        (
            None,
            [
                input.path().join("Output/Show - 01.mkv"),
                input.path().join("Season 1/Output/Show - 02.mkv"),
            ],
        ),
        (
            Some(output.path()),
            [
                output.path().join("Show - 01.mkv"),
                output.path().join("Season 1/Show - 02.mkv"),
            ],
        ),
    ] {
        let (result, events) = dry_run(out_dir, None).await;
        let err = result.unwrap_err();
        assert!(
            err.starts_with("Multiple items would write the same remux output: "),
            "{err}"
        );
        for id in &names[..4] {
            assert!(err.contains(&format!("'{id}'")), "{err}");
        }
        assert!(!err.contains("Show - 03"), "{err}");
        for destination in destinations {
            assert!(
                err.to_lowercase()
                    .contains(&destination.display().to_string().to_lowercase()),
                "{err}"
            );
        }
        assert!(
            events.is_empty(),
            "a collision must abort before BatchStart"
        );
        assert!(!input.path().join("Output").exists());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    // Deselecting all but one item of each group clears the collision.
    let selected = ["Show - 01.MKV", "season 1/Show - 02.mkv", "Show - 03.mkv"];
    for out_dir in [None, Some(output.path())] {
        let (result, events) = dry_run(out_dir, Some(selected.map(String::from).to_vec())).await;
        let summary = result.unwrap();
        assert_eq!((summary.total, summary.errors), (3, 0), "{:?}", summary.log);
        assert!(matches!(events.first(), Some(JobEvent::BatchStart { .. })));
    }
}

/// An --out-dir above the input mirrors `in/Show - 01.mkv` to `<input>/Show - 01.mkv`, where it
/// would replace the unselected source there. The batch is refused before BatchStart; a mirrored
/// path that stays outside the input is accepted.
#[tokio::test]
async fn test_remux_refuses_an_ancestor_out_dir_landing_in_the_input() {
    let tmp = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let input = tmp.path().join("in");
    let videos = [
        input.join("Show - 01.mkv"),
        input.join("in/Show - 01.mkv"),
        input.join("Season 1/Show - 02.mkv"),
    ];
    for video in &videos {
        fs::create_dir_all(video.parent().unwrap()).unwrap();
        fs::write(video, b"video").unwrap();
    }
    let video_refs: Vec<&Path> = videos.iter().map(|video| video.as_path()).collect();
    let dry_run = |selected: &[&str]| {
        run_batch(
            PipelineConfig {
                out_dir: Some(tmp.path().to_path_buf()),
                selected_items: Some(selected.iter().map(|id| id.to_string()).collect()),
                dry_run: true,
                cache: Some(seeded_cache(
                    cache_dir.path(),
                    &video_refs,
                    &video_metadata("A_AAC"),
                )),
                ..config(PipelineMode::Remux, &input)
            },
            CancellationToken::new(),
        )
    };

    let (result, events) = dry_run(&["in/Show - 01.mkv", "Season 1/Show - 02.mkv"]).await;
    assert_eq!(
        result.unwrap_err(),
        format!(
            "Remux output would land inside the input directory, where it could replace a source or be rediscovered by the next run: 'in/Show - 01.mkv' -> '{}'",
            videos[0].display()
        )
    );
    assert!(events.is_empty(), "the refusal must come before BatchStart");

    let (result, events) = dry_run(&["Season 1/Show - 02.mkv"]).await;
    let summary = result.unwrap();
    assert_eq!((summary.total, summary.errors), (1, 0), "{:?}", summary.log);
    assert!(matches!(events.first(), Some(JobEvent::BatchStart { .. })));
}

/// `Show - 01.mkv` and `Show - 01.MKV` would name their Sidecars from one file stem, so an
/// extract batch holding both is refused before BatchStart, in a dry run and a real one, and
/// writes nothing; selecting one of them runs it.
// Linux only: the colliding sources differ only by case, which takes a case-sensitive file
// system; the default Windows and macOS ones are not.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_extract_rejects_items_whose_sidecars_share_names_before_batch_start() {
    let input = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let videos = [
        input.path().join("Show - 01.mkv"),
        input.path().join("Show - 01.MKV"),
    ];
    for video in &videos {
        fs::write(video, b"video").unwrap();
    }
    let mut metadata = video_metadata("A_AAC");
    metadata.tracks.push(mkv_core::domain::MkvTrack {
        id: Some(2),
        track_type: "subtitles".to_string(),
        codec: Some("S_TEXT/ASS".to_string()),
        properties: mkv_core::domain::TrackProperties {
            codec_id: Some("S_TEXT/ASS".to_string()),
            language: Some("eng".to_string()),
            ..Default::default()
        },
    });
    let video_refs: Vec<&Path> = videos.iter().map(|video| video.as_path()).collect();
    let run = |dry_run: bool, selected: Option<&[&str]>, runner: Arc<ScriptedRunner>| {
        let (tx, mut rx) = create_event_channel(256);
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                selected_items: selected.map(|ids| ids.iter().map(|id| id.to_string()).collect()),
                dry_run,
                cache: Some(seeded_cache(cache_dir.path(), &video_refs, &metadata)),
                // Keeps the run off the user's default font hash memo.
                font_hash_memo: Some(Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
                    cache_dir.path().join("font_hash_memo.json"),
                )))),
                workers: 1,
                io_jobs: 1,
                ..config(PipelineMode::Extract, input.path())
            },
            Some(tx),
            CancellationToken::new(),
        )
        .with_media_tool_runner(runner);
        async move {
            let result = pipeline.run().await;
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            (result, events)
        }
    };
    let entries = || {
        let mut names: Vec<String> = fs::read_dir(input.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    // The two IDs tie in natural order, so they keep the directory's listing order.
    let destination = input.path().join("Show - 01");
    let expected = |ids: &str| {
        format!(
            "Multiple items would extract files with the same names: {ids} -> '{}'",
            destination.display()
        )
    };
    for dry_run in [true, false] {
        // An empty script: any Media Tool invocation panics.
        let runner = Arc::new(ScriptedRunner::new([]));
        let (result, events) = run(dry_run, None, runner.clone()).await;
        let err = result.unwrap_err();
        assert!(
            err == expected("'Show - 01.mkv', 'Show - 01.MKV'")
                || err == expected("'Show - 01.MKV', 'Show - 01.mkv'"),
            "dry run {dry_run}: {err}"
        );
        assert!(
            events.is_empty(),
            "a collision must abort before BatchStart"
        );
        assert!(runner.calls().is_empty());
        assert_eq!(entries(), ["Show - 01.MKV", "Show - 01.mkv"]);
    }

    let (result, events) = run(
        true,
        Some(&["Show - 01.MKV"]),
        Arc::new(ScriptedRunner::new([])),
    )
    .await;
    assert_eq!(result.unwrap().total, 1);
    assert!(matches!(events.first(), Some(JobEvent::BatchStart { .. })));

    let runner = Arc::new(ScriptedRunner::new([Step::exited(
        MediaTool::Mkvextract,
        0,
        "",
        "",
    )
    .writes_extract_targets(b"[Script Info]\n")]));
    let (result, _events) = run(false, Some(&["Show - 01.MKV"]), runner.clone()).await;
    let summary = result.unwrap();
    assert_eq!(
        (summary.total, summary.completed, summary.errors),
        (1, 1, 0),
        "{summary:#?}"
    );
    assert_eq!(runner.calls().len(), 1);
    let subtitles: Vec<String> = entries()
        .into_iter()
        .filter(|name| name.ends_with(".ass"))
        .collect();
    assert_eq!(subtitles.len(), 1, "{:?}", entries());
}

/// `out/A` is a dangling link to `B`: extracting `B/Show - 01.mkv` creates `out/B`, after which
/// `A/Show - 01.mkv` would extract into the same directory and replace B's Sidecars. The link
/// cannot be resolved before the run, so the batch is refused before BatchStart.
#[cfg(unix)]
#[tokio::test]
async fn test_extract_rejects_an_output_dir_through_a_dangling_symlink_before_batch_start() {
    let input = tempdir().unwrap();
    let out = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let videos = [
        input.path().join("A").join("Show - 01.mkv"),
        input.path().join("B").join("Show - 01.mkv"),
    ];
    for video in &videos {
        fs::create_dir_all(video.parent().unwrap()).unwrap();
        fs::write(video, b"video").unwrap();
    }
    std::os::unix::fs::symlink("B", out.path().join("A")).unwrap();
    let video_refs: Vec<&Path> = videos.iter().map(|video| video.as_path()).collect();
    let metadata = video_metadata("A_AAC");
    let expected = format!(
        "Cannot determine the extract destination of 'A/Show - 01.mkv', '{}': '{}' is a symlink whose target does not exist",
        out.path().join("A").join("Show - 01").display(),
        out.path().canonicalize().unwrap().join("A").display()
    );

    for dry_run in [true, false] {
        // An empty script: any Media Tool invocation panics.
        let runner = Arc::new(ScriptedRunner::new([]));
        let (tx, mut rx) = create_event_channel(256);
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                selected_items: Some(vec!["B/Show - 01.mkv".into(), "A/Show - 01.mkv".into()]),
                out_dir: Some(out.path().to_path_buf()),
                dry_run,
                cache: Some(seeded_cache(cache_dir.path(), &video_refs, &metadata)),
                // Keeps the run off the user's default font hash memo.
                font_hash_memo: Some(Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
                    cache_dir.path().join("font_hash_memo.json"),
                )))),
                workers: 1,
                io_jobs: 1,
                ..config(PipelineMode::Extract, input.path())
            },
            Some(tx),
            CancellationToken::new(),
        )
        .with_media_tool_runner(runner.clone());
        assert_eq!(
            pipeline.run().await.unwrap_err(),
            expected,
            "dry run {dry_run}"
        );
        assert!(
            rx.try_recv().is_err(),
            "the refusal must come before BatchStart"
        );
        assert!(runner.calls().is_empty());
        let entries: Vec<_> = fs::read_dir(out.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["A"], "dry run {dry_run}");
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
    // Seeding hashes nothing, and this dummy episode carries no font to promote, so the
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

const CHAPTER_XML: &[u8] =
    b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>";

/// Runs a remux dry run over `root` and returns the summary log. Every `Show - NN.mkv` has a
/// sibling subtitle and probes as `metadata`.
async fn chapter_dry_run_log(
    root: &Path,
    metadata: &mkv_core::scan::MkvmergeMetadata,
    assets_dir: Option<&Path>,
    selected_items: Option<Vec<String>>,
) -> Vec<LogLine> {
    let videos: Vec<_> = ["Show - 01.mkv", "Show - 02.mkv"]
        .iter()
        .map(|name| root.join(name))
        .collect();
    for video in &videos {
        fs::write(video, b"dummy mkv").unwrap();
        fs::write(video.with_extension("ass"), b"[Script Info]\n").unwrap();
    }
    let video_refs: Vec<&Path> = videos.iter().map(|v| v.as_path()).collect();

    let (summary, _) = run_batch(
        PipelineConfig {
            dry_run: true,
            assets_dir: assets_dir.map(Path::to_path_buf),
            selected_items,
            cache: Some(seeded_cache(root, &video_refs, metadata)),
            ..config(PipelineMode::Remux, root)
        },
        CancellationToken::new(),
    )
    .await;
    summary.unwrap().log
}

fn write_chapters(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).unwrap();
    for name in names {
        fs::write(dir.join(name), CHAPTER_XML).unwrap();
    }
}

fn duplicate_chapter_lines(log: &[LogLine]) -> Vec<&LogLine> {
    log.iter()
        .filter(|l| l.text.contains("More than one chapter file"))
        .collect()
}

/// Same-directory duplicates are reported on the item that uses the kept file, right after its
/// `[DRY-RUN]` line, naming every ignored file in index order.
#[tokio::test]
async fn test_remux_dry_run_reports_same_directory_duplicate_chapters_on_the_item() {
    for (names, ignored) in [
        (
            &["Show - 01.xml", "Show - 01_b.xml"][..],
            "'Show - 01_b.xml'",
        ),
        (
            &["Show - 01.xml", "Show - 01_b.xml", "Show - 01_c.xml"][..],
            "'Show - 01_b.xml', 'Show - 01_c.xml'",
        ),
    ] {
        let tmp = tempdir().unwrap();
        write_chapters(tmp.path(), names);

        let log = chapter_dry_run_log(tmp.path(), &video_metadata("A_AAC"), None, None).await;

        let warning = format!(
            "  Warning: More than one chapter file for this episode in '{}': using 'Show - 01.xml', ignoring {ignored}",
            tmp.path().display()
        );
        let dry_run = log
            .iter()
            .position(|l| l.text.starts_with("[DRY-RUN] Would remux 'Show - 01.mkv'"))
            .unwrap_or_else(|| panic!("{log:#?}"));
        assert_eq!(log[dry_run + 1], LogLine::warning(warning), "{log:#?}");
        // Episode 02 has no chapter file, so only episode 01 carries a line.
        assert_eq!(duplicate_chapter_lines(&log).len(), 1, "{log:#?}");
    }
}

/// With duplicates only in the `--assets-dir` fallback, the line appears when the fallback is
/// consulted (the source has no chapters) and not when embedded chapters make it unnecessary.
#[tokio::test]
async fn test_remux_dry_run_reports_fallback_duplicate_chapters_only_when_consulted() {
    let mut embedded = video_metadata("A_AAC");
    embedded.chapters = vec![mkv_core::scan::MkvChapter {
        num_entries: Some(3),
    }];

    for (metadata, reported) in [(video_metadata("A_AAC"), true), (embedded, false)] {
        let tmp = tempdir().unwrap();
        let assets = tmp.path().join("assets");
        write_chapters(&assets, &["Show - 01.xml", "Show - 01_b.xml"]);

        let log = chapter_dry_run_log(tmp.path(), &metadata, Some(&assets), None).await;

        assert_eq!(
            duplicate_chapter_lines(&log).len(),
            usize::from(reported),
            "{log:#?}"
        );
    }
}

/// A duplicate for an episode outside the run's selection is not reported.
#[tokio::test]
async fn test_remux_dry_run_ignores_duplicate_chapters_of_unselected_episodes() {
    let tmp = tempdir().unwrap();
    write_chapters(tmp.path(), &["Show - 02.xml", "Show - 02_b.xml"]);

    let log = chapter_dry_run_log(
        tmp.path(),
        &video_metadata("A_AAC"),
        None,
        Some(vec!["Show - 01.mkv".to_string()]),
    )
    .await;

    assert!(
        log.iter()
            .any(|l| l.text.contains("Would remux 'Show - 01.mkv'")),
        "{log:#?}"
    );
    assert!(duplicate_chapter_lines(&log).is_empty(), "{log:#?}");
}

/// Remuxes `selected` (every video when `None`) through a scripted mkvmerge and returns the
/// chapter file each item was muxed with, in batch order. `videos` are paths under `root`, each
/// probing from the seeded cache as its metadata, with a sibling subtitle so no item is skipped.
/// Only the selected videos may become items: the batch announces and settles those alone.
async fn remuxed_chapter_files(
    root: &Path,
    videos: &[(&str, &mkv_core::scan::MkvmergeMetadata)],
    selected: Option<&[&str]>,
) -> Vec<Option<String>> {
    let out = tempdir().unwrap();
    let cache = seeded_cache(root, &[], &video_metadata("A_AAC"));
    for (name, metadata) in videos {
        let video = root.join(name);
        fs::create_dir_all(video.parent().unwrap()).unwrap();
        fs::write(&video, b"dummy mkv").unwrap();
        fs::write(video.with_extension("ass"), b"[Script Info]\n").unwrap();
        cache.put_path(&video, (*metadata).clone());
    }
    let items = selected.map_or(videos.len(), <[&str]>::len);
    let runner = Arc::new(ScriptedRunner::new((0..items).map(|_| {
        Step::exited(MediaTool::Mkvmerge, 0, "", "").writes_output(b"remuxed")
    })));

    // Large enough that the undrained channel never parks the batch.
    let (tx, mut rx) = create_event_channel(256);
    let summary = BatchPipeline::new(
        PipelineConfig {
            out_dir: Some(out.path().to_path_buf()),
            selected_items: selected.map(|ids| ids.iter().map(|id| id.to_string()).collect()),
            cache: Some(cache),
            workers: 1,
            io_jobs: 1,
            ..config(PipelineMode::Remux, root)
        },
        Some(tx),
        CancellationToken::new(),
    )
    .with_media_tool_runner(runner.clone())
    .run()
    .await
    .unwrap();
    assert_eq!(
        (summary.total, summary.completed, summary.errors),
        (items, items, 0),
        "{summary:#?}"
    );
    let mut item_events = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            JobEvent::BatchStart { total, .. } => assert_eq!(total, items),
            JobEvent::ItemStart { .. } | JobEvent::ItemComplete { .. } => item_events += 1,
            _ => {}
        }
    }
    assert_eq!(item_events, 2 * items);

    runner
        .calls()
        .into_iter()
        .map(|(_, args)| {
            let at = args.iter().position(|arg| arg == "--chapters")?;
            Some(args[at + 1].clone())
        })
        .collect()
}

/// A leftover chapter file is judged against the whole Release Directory: a release of
/// several chapterless episodes leaves it unclaimed whether one episode or all are selected,
/// the unselected ones being probed and found chapterless.
#[tokio::test]
async fn test_remux_subset_never_claims_a_leftover_the_full_run_leaves_alone() {
    let tmp = tempdir().unwrap();
    write_chapters(tmp.path(), &["chapters.xml"]);
    let plain = video_metadata("A_AAC");
    let videos = [
        ("Show - 01.mkv", &plain),
        ("Show - 02.mkv", &plain),
        ("Show - 03.mkv", &plain),
    ];

    let full = remuxed_chapter_files(tmp.path(), &videos, None).await;
    assert_eq!(full, [None, None, None]);
    let subset = remuxed_chapter_files(tmp.path(), &videos, Some(&["Show - 02.mkv"])).await;
    assert_eq!(subset, [None]);
}

/// The common retry: episodes 02-04 carry embedded chapters and 01 does not. The unselected
/// episodes of a directory with one unnumbered chapter file are probed, so re-running 01 alone
/// claims the file just as the full run does.
#[tokio::test]
async fn test_remux_subset_claims_the_leftover_the_full_run_claims() {
    let tmp = tempdir().unwrap();
    write_chapters(tmp.path(), &["chapters.xml"]);
    let plain = video_metadata("A_AAC");
    let mut embedded = video_metadata("A_AAC");
    embedded.chapters = vec![mkv_core::scan::MkvChapter {
        num_entries: Some(3),
    }];
    let videos = [
        ("Show - 01.mkv", &plain),
        ("Show - 02.mkv", &embedded),
        ("Show - 03.mkv", &embedded),
        ("Show - 04.mkv", &embedded),
    ];
    let chapters = Some(
        tmp.path()
            .join("chapters.xml")
            .to_string_lossy()
            .into_owned(),
    );

    let full = remuxed_chapter_files(tmp.path(), &videos, None).await;
    assert_eq!(full, [chapters.clone(), None, None, None]);
    let subset = remuxed_chapter_files(tmp.path(), &videos, Some(&["Show - 01.mkv"])).await;
    assert_eq!(subset, [chapters]);
}

/// A single-episode release still claims its only leftover chapter file.
#[tokio::test]
async fn test_remux_single_episode_release_claims_its_leftover_chapters() {
    let tmp = tempdir().unwrap();
    write_chapters(tmp.path(), &["chapters.xml"]);
    let plain = video_metadata("A_AAC");

    let claimed = remuxed_chapter_files(tmp.path(), &[("Show - 01.mkv", &plain)], None).await;
    let chapters = tmp
        .path()
        .join("chapters.xml")
        .to_string_lossy()
        .into_owned();
    assert_eq!(claimed, [Some(chapters)]);
}

#[tokio::test]
async fn test_remux_nested_release_claims_only_its_own_leftover_chapters() {
    let tmp = tempdir().unwrap();
    write_chapters(tmp.path(), &["chapters.xml"]);
    write_chapters(&tmp.path().join("Specials"), &["chapters.xml"]);
    let plain = video_metadata("A_AAC");
    let videos = [
        ("Show - 01.mkv", &plain),
        ("Specials/Show - SP01.mkv", &plain),
    ];
    // Joined per component: Windows reports the scanned path with backslashes only.
    let chapters = |dir: &Path| Some(dir.join("chapters.xml").to_string_lossy().into_owned());
    let specials = tmp.path().join("Specials");

    // Each Release Directory claims its own file, the input root included.
    let full = remuxed_chapter_files(tmp.path(), &videos, None).await;
    assert_eq!(full, [chapters(tmp.path()), chapters(&specials)]);
    let subset =
        remuxed_chapter_files(tmp.path(), &videos, Some(&["Specials/Show - SP01.mkv"])).await;
    assert_eq!(subset, [chapters(&specials)]);
}
