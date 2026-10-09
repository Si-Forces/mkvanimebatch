mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use common::{run_batch, seeded_cache, video_metadata};
use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{
    create_event_channel, create_log_channel, BatchPipeline, CancellationToken, ItemOutcome,
    JobEvent, PipelineConfig, PipelineMode,
};
use mkv_core::request::{AssignmentKind, BatchRequest, TrackAssignment};

fn manual_subtitle_assignment(target: &str, asset: &str) -> TrackAssignment {
    TrackAssignment {
        target_video: target.into(),
        asset_path: asset.into(),
        track_type: AssignmentKind::Subtitle,
        language: Some("ind".into()),
        title: Some("Bahasa Indonesia".into()),
        offset_ms: Some(1250),
    }
}

/// A remux dry run of `input_dir` into `out_dir` that applies `assignments`.
fn remux_dry_run(
    input_dir: &Path,
    out_dir: &Path,
    assignments: Vec<TrackAssignment>,
) -> PipelineConfig {
    PipelineConfig {
        mode: PipelineMode::Remux,
        input_dir: input_dir.to_path_buf(),
        out_dir: Some(out_dir.to_path_buf()),
        dry_run: true,
        track_assignments: Some(assignments),
        ..Default::default()
    }
}

fn item_outcomes(events: &[JobEvent]) -> HashMap<&str, ItemOutcome> {
    events
        .iter()
        .filter_map(|event| match event {
            JobEvent::ItemComplete { id, status, .. } => Some((id.as_str(), *status)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn test_missing_assignment_aborts_entire_remux_plan() {
    for (assigned_name, dry_run) in [("Show - 02.mkv", false), ("unparseable.mkv", true)] {
        let input = tempdir().unwrap();
        let output = tempdir().unwrap();
        let first = input.path().join("Show - 01.mkv");
        let assigned = input.path().join(assigned_name);
        let manual = input.path().join("manual.ass");
        fs::write(&first, b"first video").unwrap();
        fs::write(&assigned, b"second video").unwrap();
        fs::write(input.path().join("Show - 01.ass"), b"first subtitle").unwrap();
        fs::write(input.path().join("Show - 02.ass"), b"automatic subtitle").unwrap();
        fs::write(&manual, b"manual subtitle").unwrap();

        // The assignment passes preflight, then its asset disappears before planning.
        let assignment = manual_subtitle_assignment(assigned_name, "manual.ass");
        let request = BatchRequest {
            input_dir: input.path().to_path_buf(),
            assignments: Some(vec![assignment.clone()]),
            selected_items: Some(vec!["Show - 01.mkv".into(), assigned_name.into()]),
            ..Default::default()
        };
        request.clone().resolve().unwrap();
        fs::remove_file(&manual).unwrap();

        let (summary, events) = run_batch(
            PipelineConfig {
                dry_run,
                cache: Some(seeded_cache(
                    input.path(),
                    &[&first, &assigned],
                    &video_metadata("A_FLAC"),
                )),
                selected_items: request.selected_items,
                ..remux_dry_run(input.path(), output.path(), vec![assignment])
            },
            CancellationToken::new(),
        )
        .await;
        let summary = summary.unwrap();
        assert_eq!(
            (
                summary.total,
                summary.completed,
                summary.skipped,
                summary.errors
            ),
            (2, 0, 0, 2)
        );
        assert!(!summary.cancelled);

        assert!(matches!(
            events.first(),
            Some(JobEvent::BatchStart { total: 2, .. })
        ));
        let outcomes: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                JobEvent::ItemComplete {
                    id, status, error, ..
                } => Some((id.as_str(), status, error.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|(_, status, _)| **status == ItemOutcome::Error));
        assert!(outcomes.iter().any(|(id, _, err)| *id == assigned_name
            && err.unwrap().contains(&manual.display().to_string())));
        assert!(outcomes.iter().any(|(id, _, err)| *id == "Show - 01.mkv"
            && err.unwrap().contains("Batch planning aborted")));
        let terminal_count = events
            .iter()
            .filter(|event| matches!(event, JobEvent::BatchComplete { .. }))
            .count();
        assert_eq!(terminal_count, 1, "batch must settle exactly once");
        assert!(
            matches!(
                events.last(),
                Some(JobEvent::BatchComplete {
                    success: false,
                    cancelled: false,
                    ..
                })
            ),
            "the single terminal must follow every item error"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
}

/// A planning abort keeps the lines planning collected before it: the skip reaches the Log
/// Channel and the summary log ahead of the abort line. The dry-run prediction for the planned
/// item in between is dropped, since the aborted batch never does that work.
#[tokio::test]
async fn test_planning_abort_keeps_earlier_skip_lines_and_drops_dry_run_lines() {
    for dry_run in [false, true] {
        let input = tempdir().unwrap();
        let output = tempdir().unwrap();
        let skipped = input.path().join("Extras.mkv");
        let planned = input.path().join("Show - 01.mkv");
        let assigned = input.path().join("Show - 02.mkv");
        let manual = input.path().join("manual.ass");
        for video in [&skipped, &planned, &assigned] {
            fs::write(video, b"video").unwrap();
        }
        fs::write(input.path().join("Show - 01.ass"), b"subtitle").unwrap();

        // The assignment's asset never exists, so planning "Show - 02.mkv" aborts the batch.
        let (event_sender, mut event_receiver) = create_event_channel(64);
        let (log_sender, mut log_receiver) = create_log_channel(64);
        let pipeline = BatchPipeline::new(
            PipelineConfig {
                dry_run,
                cache: Some(seeded_cache(
                    input.path(),
                    &[&skipped, &planned, &assigned],
                    &video_metadata("A_FLAC"),
                )),
                selected_items: Some(vec![
                    "Extras.mkv".into(),
                    "Show - 01.mkv".into(),
                    "Show - 02.mkv".into(),
                ]),
                ..remux_dry_run(
                    input.path(),
                    output.path(),
                    vec![manual_subtitle_assignment("Show - 02.mkv", "manual.ass")],
                )
            },
            Some(event_sender),
            CancellationToken::new(),
        )
        .with_log_sender(log_sender);
        let summary = pipeline.run().await.unwrap();

        let expected = vec![
            "Skipping 'Extras.mkv': Could not extract episode number from filename".to_string(),
            format!(
                "  ERROR: Remux planning failed for 'Show - 02.mkv': Assigned subtitle '{}' no longer exists",
                manual.display()
            ),
        ];
        let mut channel_log = Vec::new();
        while let Ok(line) = log_receiver.try_recv() {
            channel_log.push(line);
        }
        assert_eq!(channel_log, expected, "dry_run={dry_run}");
        assert_eq!(summary.log, expected, "dry_run={dry_run}");
        assert_eq!((summary.total, summary.errors), (3, 3));
        assert_eq!(
            fs::read_dir(output.path()).unwrap().count(),
            0,
            "dry_run={dry_run}"
        );

        let mut errors = 0;
        while let Ok(event) = event_receiver.try_recv() {
            if let JobEvent::ItemComplete { status, .. } = event {
                assert_eq!(status, ItemOutcome::Error);
                errors += 1;
            }
        }
        assert_eq!(errors, 3);
    }
}

#[tokio::test]
async fn test_valid_assignment_plans_unparsed_video() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let unparsed = input.path().join("unparseable_video.mkv");
    fs::write(&unparsed, b"unparsed video").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            cache: Some(seeded_cache(
                input.path(),
                &[&unparsed],
                &video_metadata("A_FLAC"),
            )),
            selected_items: Some(vec!["unparseable_video.mkv".into()]),
            ..remux_dry_run(
                input.path(),
                output.path(),
                vec![manual_subtitle_assignment(
                    "unparseable_video.mkv",
                    "manual.ass",
                )],
            )
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.errors, 0);
    assert_eq!(
        item_outcomes(&events).get("unparseable_video.mkv"),
        Some(&ItemOutcome::Planned)
    );
}

#[tokio::test]
async fn test_root_assignment_does_not_match_nested_unparsed_video_with_same_name() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let season = input.path().join("Season 01");
    fs::create_dir_all(&season).unwrap();
    let root_video = input.path().join("unparseable_video.mkv");
    let nested_video = season.join("unparseable_video.mkv");
    fs::write(&root_video, b"root video").unwrap();
    fs::write(&nested_video, b"nested video").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            cache: Some(seeded_cache(
                input.path(),
                &[&root_video, &nested_video],
                &video_metadata("A_FLAC"),
            )),
            ..remux_dry_run(
                input.path(),
                output.path(),
                vec![manual_subtitle_assignment(
                    "unparseable_video.mkv",
                    "manual.ass",
                )],
            )
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.completed, 1);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.errors, 0);

    let outcomes = item_outcomes(&events);
    assert_eq!(
        outcomes.get("unparseable_video.mkv"),
        Some(&ItemOutcome::Planned)
    );
    assert_eq!(
        outcomes.get("Season 01/unparseable_video.mkv"),
        Some(&ItemOutcome::Skipped)
    );
}

#[tokio::test]
async fn test_pipeline_with_track_assignments_plans_override() {
    let dir = tempdir().unwrap();
    let output = tempdir().unwrap();
    let video_path = dir.path().join("Episode - 01.mkv");
    fs::write(&video_path, b"dummy mkv content").unwrap();
    fs::write(
        dir.path().join("custom_track.ass"),
        b"[Script Info]\nTitle: Custom\n",
    )
    .unwrap();

    let (summary, events) = run_batch(
        PipelineConfig {
            cache: Some(seeded_cache(
                dir.path(),
                &[&video_path],
                &video_metadata("A_FLAC"),
            )),
            ..remux_dry_run(
                dir.path(),
                output.path(),
                vec![manual_subtitle_assignment(
                    "Episode - 01.mkv",
                    "custom_track.ass",
                )],
            )
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(summary.total, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.errors, 0);
    assert_eq!(
        item_outcomes(&events).get("Episode - 01.mkv"),
        Some(&ItemOutcome::Planned)
    );
}

#[tokio::test]
async fn test_assignment_target_excluded_from_discovery_fails_closed() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"video").unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let (result, events) = run_batch(
        remux_dry_run(
            input.path(),
            output.path(),
            vec![manual_subtitle_assignment(
                "Output/hidden.mkv",
                "manual.ass",
            )],
        ),
        CancellationToken::new(),
    )
    .await;

    let err = result.unwrap_err();
    assert!(err.contains("Output/hidden.mkv"), "{err}");
    assert!(
        events.is_empty(),
        "an unusable assignment must abort before BatchStart"
    );
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_assignment_target_outside_active_selection_fails_closed() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"first video").unwrap();
    fs::write(input.path().join("Show - 02.mkv"), b"second video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let (result, events) = run_batch(
        PipelineConfig {
            selected_items: Some(vec!["Show - 01.mkv".into()]),
            ..remux_dry_run(
                input.path(),
                output.path(),
                vec![manual_subtitle_assignment("Show - 02.mkv", "manual.ass")],
            )
        },
        CancellationToken::new(),
    )
    .await;

    let err = result.unwrap_err();
    assert!(err.contains("Show - 02.mkv"), "{err}");
    assert!(events.is_empty(), "{err}");
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_every_unmatched_assignment_target_fails_in_one_error() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    fs::create_dir(input.path().join("Output")).unwrap();
    fs::write(input.path().join("Show - 01.mkv"), b"video").unwrap();
    fs::write(input.path().join("Output/hidden.mkv"), b"hidden video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let (result, events) = run_batch(
        remux_dry_run(
            input.path(),
            output.path(),
            vec![
                manual_subtitle_assignment("Output/hidden.mkv", "manual.ass"),
                manual_subtitle_assignment("ghost.mkv", "manual.ass"),
                manual_subtitle_assignment("Season 02/Show - 02.mkv", "manual.ass"),
                manual_subtitle_assignment("Output/hidden.mkv", "manual.ass"),
            ],
        ),
        CancellationToken::new(),
    )
    .await;

    let err = result.unwrap_err();
    assert_eq!(
        err,
        concat!(
            "Track assignment targets do not match exactly one video in ",
            "the discovered MKV files: Output/hidden.mkv, ghost.mkv, ",
            "Season 02/Show - 02.mkv"
        )
    );
    assert!(events.is_empty(), "{err}");
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_valid_assignments_still_resolve_within_the_active_selection() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let unparsed = input.path().join("raw_capture.mkv");
    let automatic = input.path().join("Show - 01.mkv");
    fs::write(&unparsed, b"unparsed video").unwrap();
    fs::write(&automatic, b"video").unwrap();
    fs::write(input.path().join("Show - 01.ass"), b"automatic subtitle").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();

    let selected = vec!["raw_capture.mkv".to_string(), "Show - 01.mkv".to_string()];
    let (summary, events) = run_batch(
        PipelineConfig {
            cache: Some(seeded_cache(
                input.path(),
                &[&unparsed, &automatic],
                &video_metadata("A_FLAC"),
            )),
            selected_items: Some(selected.clone()),
            ..remux_dry_run(
                input.path(),
                output.path(),
                vec![manual_subtitle_assignment("raw_capture.mkv", "manual.ass")],
            )
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();
    assert_eq!(
        (
            summary.total,
            summary.completed,
            summary.skipped,
            summary.errors
        ),
        (2, 2, 0, 0)
    );

    let items = events
        .iter()
        .find_map(|event| match event {
            JobEvent::BatchStart { items, .. } => items.clone(),
            _ => None,
        })
        .expect("expected BatchStart event");
    assert_eq!(
        items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        selected
    );
    let outcomes = item_outcomes(&events);
    assert_eq!(outcomes.get("raw_capture.mkv"), Some(&ItemOutcome::Planned));
    assert_eq!(outcomes.get("Show - 01.mkv"), Some(&ItemOutcome::Planned));
}

// Surrounding whitespace is part of a name (#61): a top-level folder that starts with a space
// must list, select and assign under the ID it has on disk.
#[tokio::test]
async fn test_item_under_a_leading_space_folder_is_listed_selected_and_assigned() {
    let input = tempdir().unwrap();
    let output = tempdir().unwrap();
    let id = " Season 1/01.mkv";
    let video = input.path().join(id);
    fs::create_dir_all(video.parent().unwrap()).unwrap();
    fs::write(&video, b"video").unwrap();
    fs::write(input.path().join("manual.ass"), b"manual subtitle").unwrap();
    let cache = seeded_cache(input.path(), &[&video], &video_metadata("A_FLAC"));

    let manifest = generate_inspection_manifest(input.path(), Some(Arc::clone(&cache)), None)
        .await
        .unwrap();
    let ids: Vec<_> = manifest.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, [id]);

    let request = BatchRequest {
        input_dir: input.path().to_path_buf(),
        out_dir: Some(output.path().to_path_buf()),
        dry_run: Some(true),
        assignments: Some(vec![manual_subtitle_assignment(id, "manual.ass")]),
        selected_items: Some(vec![id.into()]),
        ..Default::default()
    };
    let (summary, events) = run_batch(
        PipelineConfig {
            cache: Some(cache),
            ..request.resolve().unwrap()
        },
        CancellationToken::new(),
    )
    .await;
    let summary = summary.unwrap();

    assert_eq!(
        (
            summary.total,
            summary.completed,
            summary.skipped,
            summary.errors
        ),
        (1, 1, 0, 0)
    );
    assert_eq!(item_outcomes(&events).get(id), Some(&ItemOutcome::Planned));
}
