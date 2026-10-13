//! Remux planning: the decision made for one item before anything executes (ADR-0004, ADR-0008,
//! ADR-0012).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::assets::AssetResolver;
use crate::domain::{output_stem, resolve_output_dir};
use crate::fonts::FontInfo;
use crate::item_id::ItemId;
use crate::remux::{select_tracks, PlannedJob, TrackSelection};
use crate::request::TrackAssignment;
use crate::scan::{InspectionResult, ParsedFile};

/// What planning prepared for one Release Directory: its Asset Resolver, its fonts, and the
/// leftover chapter files claimed for its videos.
pub(crate) struct PreparedDir {
    pub(crate) resolver: AssetResolver,
    /// Shared with every `RemuxWork` of the directory, so planning an item never copies the
    /// font list.
    pub(crate) font_info: Arc<FontInfo>,
    pub(crate) chapter_claims: HashMap<PathBuf, PathBuf>,
}

/// The per-item inputs to `execute_remux_job` for an item that planning cleared to run.
/// The item's identity travels beside it as the `BatchItemInfo` that `run_items` pairs
/// with it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RemuxWork {
    pub(crate) job: PlannedJob,
    pub(crate) selection: TrackSelection,
    pub(crate) font_info: Arc<FontInfo>,
}

/// The Remux Plan for one item.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemuxPlan {
    /// The item is not remuxed.
    Skip { reason: String },
    /// The item's probe failed; the item fails and the rest of the batch goes on.
    ProbeFailed { message: String },
    /// The item lies outside every prepared Release Directory, which `run_remux` never lets
    /// happen because it prepares the directory of every batch video; the item fails and the
    /// rest of the batch goes on.
    Internal { message: String },
    /// The whole batch fails closed before any media work.
    Abort { message: String },
    /// The item is ready to execute.
    Planned(Box<RemuxWork>),
}

/// Plans each item of one remux batch from its prepared Release Directories and inspections.
/// It decides only; reporting the decisions, dry-run handling, and cancellation stay with the
/// Batch Pipeline.
pub(crate) struct RemuxPlanner<'a> {
    input_dir: &'a Path,
    out_dir: Option<&'a Path>,
    assets_dir: Option<&'a Path>,
    /// Keyed by the target's [`ItemId`]. Every target was already proven to match exactly one
    /// discovered item.
    assignments: HashMap<ItemId, &'a TrackAssignment>,
    prepared_dirs: &'a HashMap<&'a Path, PreparedDir>,
    inspections: &'a HashMap<PathBuf, InspectionResult>,
}

impl<'a> RemuxPlanner<'a> {
    pub(crate) fn new(
        input_dir: &'a Path,
        out_dir: Option<&'a Path>,
        assets_dir: Option<&'a Path>,
        assignments: &'a [TrackAssignment],
        prepared_dirs: &'a HashMap<&'a Path, PreparedDir>,
        inspections: &'a HashMap<PathBuf, InspectionResult>,
    ) -> Self {
        Self {
            input_dir,
            out_dir,
            assets_dir,
            assignments: assignments
                .iter()
                .map(|assignment| (assignment.target_id(), assignment))
                .collect(),
            prepared_dirs,
            inspections,
        }
    }

    /// Plans `file`. The checks run in a fixed order. The assigned-subtitle existence check
    /// comes first on purpose: a stale assignment fails the batch closed (ADR-0008) even for a
    /// video that would otherwise be skipped or fail its probe.
    pub(crate) fn plan(&self, file: &ParsedFile) -> RemuxPlan {
        let mkv_dir = file.path.parent().unwrap_or(Path::new("."));
        let Some(dir_prep) = self.prepared_dirs.get(mkv_dir) else {
            return RemuxPlan::Internal {
                message: format!(
                    "Internal error: no prepared Release Directory for '{}'",
                    mkv_dir.display()
                ),
            };
        };

        let assignment = self
            .assignments
            .get(&ItemId::of(&file.path, self.input_dir))
            .copied();
        let assigned_sub =
            assignment.map(|a| a.resolve_asset_path(self.input_dir, self.assets_dir));
        let assigned_sub = assigned_sub.as_deref();
        if let Some(missing) = assigned_sub.filter(|path| !path.is_file()) {
            return RemuxPlan::Abort {
                message: format!("Assigned subtitle '{}' no longer exists", missing.display()),
            };
        }

        if file.parsed.is_none() && assignment.is_none() {
            return RemuxPlan::Skip {
                reason: "Could not extract episode number from filename".into(),
            };
        }

        let inspection = self.inspections.get(&file.path);
        let Some(inspected) = inspection.and_then(|r| r.video.as_ref()) else {
            return RemuxPlan::ProbeFailed {
                message: inspection
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "Failed to probe video".to_string()),
            };
        };

        let claimed_chapter = dir_prep
            .chapter_claims
            .get(&file.path)
            .map(PathBuf::as_path);
        let res_assets =
            dir_prep
                .resolver
                .resolve_for(file, Some(inspected), claimed_chapter, assigned_sub);

        if !res_assets.matched_subtitle {
            return RemuxPlan::Skip {
                reason: res_assets
                    .skip_reason
                    .unwrap_or_else(|| "No matching subtitle found".into()),
            };
        }

        // Assignment attributes only describe the assigned subtitle; they never restyle a
        // subtitle the resolver chose instead. The resolver always picks the assigned subtitle
        // first (ADR-0004), so the filter is a guard, and the debug assertion pins that.
        let chose_assigned = res_assets.subtitle_file.as_deref() == assigned_sub;
        debug_assert!(assignment.is_none() || chose_assigned);
        let assignment = assignment.filter(|_| chose_assigned);
        RemuxPlan::Planned(Box::new(RemuxWork {
            job: PlannedJob {
                video: file.path.clone(),
                video_name: file.name.clone(),
                output_file: self.output_file(file),
                subtitle_file: res_assets.subtitle_file,
                subtitle_label: res_assets.subtitle_label,
                subtitle_mkv_lang: res_assets.subtitle_mkv_lang,
                chapter_file: res_assets.chapter_file,
                chapter_warning: res_assets.chapter_warning,
                cover_file: res_assets.cover_file,
                embedded_sub_id: res_assets.embedded_sub_id,
                use_source_fonts: dir_prep.font_info.count == 0,
                custom_offset_ms: assignment.and_then(|a| a.offset_ms),
                custom_language: assignment.and_then(|a| a.language.clone()),
                custom_title: assignment.and_then(|a| a.title.clone()),
            },
            selection: select_tracks(inspected),
            font_info: Arc::clone(&dir_prep.font_info),
        }))
    }

    /// `<out dir>/<stem>.mkv`; without an output directory the file goes to `Output/` beside
    /// its source.
    fn output_file(&self, file: &ParsedFile) -> PathBuf {
        let base = file
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "video".to_string());
        let out_stem = match &file.parsed {
            Some(parsed) => output_stem(&base, &parsed.ep_type, &parsed.ep),
            None => base,
        };
        let out_dir = resolve_output_dir(&file.path, self.input_dir, self.out_dir);
        if self.out_dir.is_none() {
            out_dir.join("Output").join(format!("{out_stem}.mkv"))
        } else {
            out_dir.join(format!("{out_stem}.mkv"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::domain::{MkvTrack, TrackProperties};
    use crate::pipeline::{prepare_release_dirs, CancellationToken, PipelineConfig};
    use crate::request::AssignmentKind;
    use crate::scan::{create_parsed_file, MkvmergeMetadata};

    /// One HEVC video track and one Japanese stereo audio track; no subtitle track.
    fn video_metadata() -> MkvmergeMetadata {
        MkvmergeMetadata {
            tracks: vec![
                MkvTrack {
                    id: Some(0),
                    track_type: "video".into(),
                    codec: Some("V_MPEGH/ISO/HEVC".into()),
                    properties: TrackProperties::default(),
                },
                MkvTrack {
                    id: Some(1),
                    track_type: "audio".into(),
                    codec: Some("A_AAC".into()),
                    properties: TrackProperties {
                        language: Some("jpn".into()),
                        audio_channels: Some(2),
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        }
    }

    fn assignment(target: &str, asset: &str) -> TrackAssignment {
        TrackAssignment {
            target_video: target.into(),
            asset_path: asset.into(),
            track_type: AssignmentKind::Subtitle,
            language: Some("ind".into()),
            title: Some("Bahasa Indonesia".into()),
            offset_ms: Some(-1250),
        }
    }

    fn write_subtitle(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"[Script Info]\n").unwrap();
    }

    fn inspections_of(video: &ParsedFile) -> HashMap<PathBuf, InspectionResult> {
        HashMap::from([(
            video.path.clone(),
            InspectionResult::success(video_metadata()),
        )])
    }

    /// Prepares the Release Directories of `videos` the way `run_remux` does, then hands the
    /// planner and the prepared directories to `f`.
    fn with_planner<T>(
        config: &PipelineConfig,
        inspections: &HashMap<PathBuf, InspectionResult>,
        videos: &[ParsedFile],
        f: impl FnOnce(&RemuxPlanner, &HashMap<&Path, PreparedDir>) -> T,
    ) -> T {
        let mut videos_by_dir: BTreeMap<&Path, Vec<ParsedFile>> = BTreeMap::new();
        for video in videos {
            videos_by_dir
                .entry(video.path.parent().unwrap())
                .or_default()
                .push(video.clone());
        }
        let prepared_dirs = prepare_release_dirs(
            &videos_by_dir,
            inspections,
            config,
            &CancellationToken::new(),
        )
        .expect("planning is never cancelled in these tests");
        let planner = RemuxPlanner::new(
            &config.input_dir,
            config.out_dir.as_deref(),
            config.assets_dir.as_deref(),
            config.track_assignments.as_deref().unwrap_or_default(),
            &prepared_dirs,
            inspections,
        );
        f(&planner, &prepared_dirs)
    }

    fn plan_video(
        config: &PipelineConfig,
        inspections: &HashMap<PathBuf, InspectionResult>,
        video: &ParsedFile,
    ) -> RemuxPlan {
        with_planner(
            config,
            inspections,
            std::slice::from_ref(video),
            |planner, _| planner.plan(video),
        )
    }

    fn planned(plan: RemuxPlan) -> RemuxWork {
        match plan {
            RemuxPlan::Planned(work) => *work,
            other => panic!("expected a planned job, got {other:?}"),
        }
    }

    #[test]
    fn assignment_language_title_and_offset_reach_the_planned_job() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Show - 01.mkv"));
        write_subtitle(&dir.path().join("custom.ass"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            track_assignments: Some(vec![assignment("Show - 01.mkv", "custom.ass")]),
            ..Default::default()
        };

        let job = planned(plan_video(&config, &inspections_of(&video), &video)).job;

        assert_eq!(job.subtitle_file, Some(dir.path().join("custom.ass")));
        assert_eq!(job.custom_language.as_deref(), Some("ind"));
        assert_eq!(job.custom_title.as_deref(), Some("Bahasa Indonesia"));
        assert_eq!(job.custom_offset_ms, Some(-1250));
    }

    #[test]
    fn an_assignment_finds_its_video_in_a_nested_release_directory() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Season 1").join("Show - 01.mkv"));
        write_subtitle(&dir.path().join("Season 1").join("custom.ass"));
        // A Windows-style target still keys to the video's `ItemId`.
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            track_assignments: Some(vec![assignment(
                "Season 1\\Show - 01.mkv",
                "Season 1/custom.ass",
            )]),
            ..Default::default()
        };

        let job = planned(plan_video(&config, &inspections_of(&video), &video)).job;

        assert_eq!(
            job.subtitle_file,
            Some(dir.path().join("Season 1").join("custom.ass"))
        );
        assert_eq!(job.custom_language.as_deref(), Some("ind"));
        assert_eq!(job.custom_offset_ms, Some(-1250));
    }

    #[test]
    fn each_video_of_a_batch_gets_its_own_assignment_and_its_directory_fonts() {
        let dir = tempdir().unwrap();
        let videos = [
            create_parsed_file(dir.path().join("Show - 01.mkv")),
            create_parsed_file(dir.path().join("Show - 02.mkv")),
        ];
        write_subtitle(&dir.path().join("one.ass"));
        write_subtitle(&dir.path().join("two.ass"));
        let second = TrackAssignment {
            language: Some("eng".into()),
            title: Some("English".into()),
            offset_ms: Some(500),
            ..assignment("Show - 02.mkv", "two.ass")
        };
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            track_assignments: Some(vec![assignment("Show - 01.mkv", "one.ass"), second]),
            ..Default::default()
        };
        let inspections: HashMap<_, _> = videos
            .iter()
            .map(|video| {
                (
                    video.path.clone(),
                    InspectionResult::success(video_metadata()),
                )
            })
            .collect();

        let works = with_planner(&config, &inspections, &videos, |planner, prepared_dirs| {
            let fonts = &prepared_dirs[dir.path()].font_info;
            videos
                .iter()
                .map(|video| {
                    let work = planned(planner.plan(video));
                    assert!(
                        Arc::ptr_eq(&work.font_info, fonts),
                        "shares its directory's fonts"
                    );
                    work
                })
                .collect::<Vec<_>>()
        });

        let expected = [
            ("one.ass", "ind", "Bahasa Indonesia", -1250),
            ("two.ass", "eng", "English", 500),
        ];
        assert_eq!(works.len(), expected.len());
        for (work, (sub, language, title, offset)) in works.iter().zip(expected) {
            let job = &work.job;
            assert_eq!(job.subtitle_file, Some(dir.path().join(sub)));
            assert_eq!(job.custom_language.as_deref(), Some(language));
            assert_eq!(job.custom_title.as_deref(), Some(title));
            assert_eq!(job.custom_offset_ms, Some(offset));
            assert_eq!(job.chapter_file, None);
            assert_eq!(job.cover_file, None);
            // The directory has no fonts, so the source's own attachments are kept.
            assert_eq!(work.font_info.count, 0);
            assert!(job.use_source_fonts);
            assert_eq!(work.selection.video_id, Some(0));
            assert_eq!(work.selection.audio_id, Some(1));
        }
    }

    #[test]
    fn output_goes_to_output_beside_the_source_without_an_out_dir() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Season 1").join("Show - 01.mkv"));
        write_subtitle(&dir.path().join("Season 1").join("Show - 01.ass"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let job = planned(plan_video(&config, &inspections_of(&video), &video)).job;

        assert_eq!(
            job.output_file,
            dir.path().join("Season 1/Output/Show - 01.mkv")
        );
    }

    #[test]
    fn output_mirrors_the_release_directory_under_an_out_dir() {
        let dir = tempdir().unwrap();
        let out_dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Season 1").join("Show - 01.mkv"));
        write_subtitle(&dir.path().join("Season 1").join("Show - 01.ass"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            out_dir: Some(out_dir.path().to_path_buf()),
            ..Default::default()
        };

        let job = planned(plan_video(&config, &inspections_of(&video), &video)).job;

        assert_eq!(
            job.output_file,
            out_dir.path().join("Season 1/Show - 01.mkv")
        );
    }

    #[test]
    fn an_unparsed_video_with_an_assignment_is_planned_under_its_own_name() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("unknown.mkv"));
        assert!(video.parsed.is_none());
        write_subtitle(&dir.path().join("custom.ass"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            track_assignments: Some(vec![assignment("unknown.mkv", "custom.ass")]),
            ..Default::default()
        };

        let job = planned(plan_video(&config, &inspections_of(&video), &video)).job;

        assert_eq!(job.output_file, dir.path().join("Output/unknown.mkv"));
        assert_eq!(job.subtitle_file, Some(dir.path().join("custom.ass")));
        assert_eq!(job.custom_title.as_deref(), Some("Bahasa Indonesia"));
    }

    #[test]
    fn an_unparsed_video_without_an_assignment_is_skipped() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("unknown.mkv"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        assert_eq!(
            plan_video(&config, &inspections_of(&video), &video),
            RemuxPlan::Skip {
                reason: "Could not extract episode number from filename".into()
            }
        );
    }

    #[test]
    fn a_video_without_a_matching_subtitle_is_skipped_with_the_resolver_reason() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Show - 01.mkv"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let inspections = inspections_of(&video);
        let plan_and_resolver_reason = || {
            with_planner(
                &config,
                &inspections,
                std::slice::from_ref(&video),
                |planner, dirs| {
                    // Resolve with the inspection `plan` passes, so the reasons are comparable.
                    let inspected = inspections[&video.path].video.as_ref();
                    let resolved = dirs[dir.path()]
                        .resolver
                        .resolve_for(&video, inspected, None, None);
                    assert!(!resolved.matched_subtitle);
                    (
                        planner.plan(&video),
                        resolved.skip_reason.expect("a reason"),
                    )
                },
            )
        };

        let (plan, no_files) = plan_and_resolver_reason();
        assert_eq!(
            plan,
            RemuxPlan::Skip {
                reason: no_files.clone()
            }
        );

        write_subtitle(&dir.path().join("Show - 99.ass"));
        let (plan, none_matched) = plan_and_resolver_reason();
        assert_eq!(
            plan,
            RemuxPlan::Skip {
                reason: none_matched.clone()
            }
        );
        assert_ne!(
            no_files, none_matched,
            "each case keeps the resolver's own reason"
        );
    }

    #[test]
    fn a_failed_probe_is_an_item_error_with_the_probe_message() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Show - 01.mkv"));
        write_subtitle(&dir.path().join("Show - 01.ass"));
        let config = PipelineConfig {
            input_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let failed = HashMap::from([(video.path.clone(), InspectionResult::failure("bad header"))]);
        assert_eq!(
            plan_video(&config, &failed, &video),
            RemuxPlan::ProbeFailed {
                message: "bad header".into()
            }
        );
        assert_eq!(
            plan_video(&config, &HashMap::new(), &video),
            RemuxPlan::ProbeFailed {
                message: "Failed to probe video".into()
            }
        );
    }

    #[test]
    fn a_missing_assigned_subtitle_aborts_the_batch_for_parsed_and_unparsed_videos() {
        let dir = tempdir().unwrap();
        write_subtitle(&dir.path().join("Show - 01.ass"));
        let stale = dir.path().join("deleted.ass");

        for name in ["Show - 01.mkv", "unknown.mkv"] {
            let video = create_parsed_file(dir.path().join(name));
            let config = PipelineConfig {
                input_dir: dir.path().to_path_buf(),
                track_assignments: Some(vec![assignment(name, "deleted.ass")]),
                ..Default::default()
            };

            assert_eq!(
                plan_video(&config, &inspections_of(&video), &video),
                RemuxPlan::Abort {
                    message: format!("Assigned subtitle '{}' no longer exists", stale.display())
                },
                "{name}"
            );
        }
    }

    #[test]
    fn a_video_outside_every_prepared_directory_is_an_internal_error() {
        let dir = tempdir().unwrap();
        let video = create_parsed_file(dir.path().join("Show - 01.mkv"));
        let prepared_dirs = HashMap::new();
        let inspections = inspections_of(&video);
        let planner = RemuxPlanner::new(dir.path(), None, None, &[], &prepared_dirs, &inspections);

        let RemuxPlan::Internal { message } = planner.plan(&video) else {
            panic!("expected an internal item error");
        };
        assert!(message.starts_with("Internal error: "), "{message}");
        assert!(
            message.contains(&dir.path().display().to_string()),
            "{message}"
        );
    }
}
