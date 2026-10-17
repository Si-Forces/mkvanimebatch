//! The Episode Inspector of a mode (ADR-0016, S4c): the row a view picked with an `Inspect`
//! Intent, else the first row, presented as the inspector shows it. Pure; the actor calls it on
//! every publication with the mode's published view. The wording is the webview's before S4c,
//! byte for byte.

use crate::actor::ModeView;
use crate::presentation::{
    candidate_tier, first_text, inspector_crumbs, path_segments, pretty_path, remux_audio_text,
    row_error, status_tone, subtitle_ladder, Tier, Tone,
};
use crate::queue::Row;
use crate::snapshot::ItemStatus;
use crate::text::is_js_whitespace;
use mkv_core::manifest::ManifestItem;
use mkv_core::request::{AssignmentKind, TrackAssignment};
use serde::Serialize;

/// The Episode Inspector as a view renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectorView {
    pub id: String,
    /// The row's place in the queue, from `01`.
    pub number: String,
    pub title: String,
    /// The item's folders, from its Item ID.
    pub crumbs: Vec<String>,
    /// Why the item failed, else why its inspection did; `None` hides the error box.
    pub error: Option<String>,
    pub pill: Pill,
    pub video: String,
    /// `Audio` (remux) or `Subtitles` (extract): what `audio` describes.
    pub audio_label: &'static str,
    pub audio: String,
    /// The Settings Draft's output folder as path crumbs (none for `/`); `None` when unset, which
    /// a view shows as the field's own description of its default.
    pub out_crumbs: Option<Vec<String>>,
    pub fonts_head: String,
    pub fonts: String,
    /// The subtitle precedence ladder (remux only; empty for extract).
    pub ladder: Vec<LadderRung>,
    /// The Track Assignment choices (remux only): automatic, each candidate, and the assigned file
    /// when it is none of them. A view adds its own file picker entry.
    pub options: Vec<Choice>,
    pub assignment: Option<TrackAssignment>,
    /// The language select's value: one of `PRESET_SUBTITLE_LANGUAGES` or `custom`.
    pub language_choice: &'static str,
    /// The custom tag field's text, which a view shows only for `custom`.
    pub custom_language: String,
}

/// The subtitle languages the inspector's select offers; any other is a custom tag.
pub const PRESET_SUBTITLE_LANGUAGES: [&str; 3] = ["ind", "eng", "jpn"];

/// The language select's value and the custom tag field's text for `assignment`. With none, the
/// default `ind`; an assignment without a language is a custom tag not typed yet.
fn language_choice(assignment: Option<&TrackAssignment>) -> (&'static str, String) {
    let language = match assignment {
        None => Some("ind"),
        Some(assignment) => assignment.language.as_deref(),
    };
    match PRESET_SUBTITLE_LANGUAGES
        .into_iter()
        .find(|preset| Some(*preset) == language)
    {
        Some(preset) => (preset, String::new()),
        None => ("custom", language.unwrap_or_default().to_string()),
    }
}

/// The Episode Inspector's subtitle fields as typed, named so that no two can be swapped.
#[derive(Debug, Clone, Copy)]
pub struct SubtitleFields<'a> {
    /// The language select's value: `custom` reads `custom_language`, an empty one is `ind`.
    pub language: &'a str,
    pub custom_language: &'a str,
    pub title: &'a str,
    /// The offset in milliseconds.
    pub offset: &'a str,
}

/// The Track Assignment of the subtitle `path` to item `id`, from the inspector's `fields`.
/// Empty text is none.
pub fn subtitle_assignment(id: &str, path: &str, fields: SubtitleFields) -> TrackAssignment {
    let SubtitleFields {
        language,
        custom_language,
        title,
        offset,
    } = fields;
    let trimmed = |text: &str| {
        Some(text.trim_matches(is_js_whitespace).to_string()).filter(|text| !text.is_empty())
    };
    let language = match language {
        "" => Some("ind".into()),
        "custom" => trimmed(custom_language),
        language => Some(language.into()),
    };
    TrackAssignment {
        target_video: id.into(),
        asset_path: path.into(),
        track_type: AssignmentKind::Subtitle,
        language,
        title: trimmed(title),
        offset_ms: trimmed(offset).and_then(|raw| offset_ms(&raw)),
    }
}

/// The largest integer a JavaScript number holds exactly (`Number.MAX_SAFE_INTEGER`).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A typed offset as JavaScript's `Math.trunc(Number(raw))` reads it, which the inspector sent
/// before: a decimal with an optional exponent, truncated to whole milliseconds; none when it
/// is no number or beyond the safe-integer range. Rust's `inf` and `nan` read as no number
/// either way, and a number field holds no hex literal.
fn offset_ms(raw: &str) -> Option<i64> {
    let ms = raw.parse::<f64>().ok()?.trunc();
    (ms.abs() <= MAX_SAFE_INTEGER).then_some(ms as i64)
}

/// The status pill: the row's status, or `Not in batch` for an unselected row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pill {
    pub tone: Tone,
    pub label: String,
}

/// One rung of the ladder as the inspector shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LadderRung {
    pub key: &'static str,
    pub label: &'static str,
    /// What the inspection found for the rung, Finder-style but for the embedded tracks; "" when
    /// nothing.
    pub detail: String,
    pub chosen: bool,
    /// Whether the rung found anything.
    pub found: bool,
    pub tag: &'static str,
}

/// One entry of the Track Assignment select.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// The row the inspector shows and its index: the one picked while the rows hold it, else the
/// first, as the webview chose before S4c. A pick the rows lost is kept, so it shows again
/// should a rescan bring its row back.
pub fn inspected_row<'a>(rows: &'a [Row], pick: Option<&str>) -> Option<(usize, &'a Row)> {
    pick.and_then(|id| rows.iter().position(|row| row.id == id))
        .or((!rows.is_empty()).then_some(0))
        .map(|at| (at, &rows[at]))
}

/// The inspector of the row at index `at` of `view`, for a remux view when `remux`. `entry` is
/// its manifest entry (none for a row only the Job Snapshot lists) and `job_error` the job's
/// error for it.
pub fn inspector_view(
    remux: bool,
    view: &ModeView,
    at: usize,
    row: &Row,
    entry: Option<&ManifestItem>,
    job_error: Option<&str>,
) -> InspectorView {
    let meta = entry.map(|entry| &entry.metadata);
    let inspection_error = entry
        .and_then(|entry| entry.inspection_error.as_deref())
        .filter(|error| !error.is_empty());
    let error = if row.status == ItemStatus::Error {
        row_error(row.status, job_error)
    } else {
        inspection_error
    };
    let video_tag = meta
        .and_then(|meta| meta.video_tag.as_deref())
        .filter(|tag| !tag.is_empty());
    let video = match video_tag {
        Some(tag) if remux => format!("{tag} · copied"),
        Some(tag) => tag.to_string(),
        None if inspection_error.is_some() => "Inspection failed".into(),
        None => "Unknown".into(),
    };
    let draft = &view.draft;
    let audio = match meta {
        None => "Unknown".into(),
        Some(meta) if remux => remux_audio_text(
            Some(meta),
            draft.bitrate.as_deref().unwrap_or_default().trim(),
        ),
        Some(meta) => first_text(&[meta.sub_summary.as_deref()], "None").to_string(),
    };
    let fonts_count = meta.and_then(|meta| meta.fonts_count);
    let fonts_dir = draft.fonts_dir.trim();
    let fonts = match fonts_count {
        None => "Not inspected".into(),
        Some(count) => format!(
            "{count} attached {} · folder: {}",
            if count == 1 { "font" } else { "fonts" },
            if fonts_dir.is_empty() {
                "auto".into()
            } else {
                pretty_path(fonts_dir)
            }
        ),
    };
    let assignment = view.assignments.get(&row.id).cloned();
    let (ladder, options) = if remux {
        subtitle_section(view, row, meta, assignment.as_ref())
    } else {
        (Vec::new(), Vec::new())
    };
    let (language_choice, custom_language) = language_choice(assignment.as_ref());
    let strings = |segments: Vec<&str>| segments.into_iter().map(str::to_string).collect();
    InspectorView {
        id: row.id.clone(),
        number: format!("{:02}", at + 1),
        title: row.name.clone(),
        crumbs: strings(inspector_crumbs(&row.id)),
        error: error.map(str::to_string),
        pill: if row.selected {
            Pill {
                tone: status_tone(row.status),
                label: row.label.clone(),
            }
        } else {
            Pill {
                tone: Tone::Mute,
                label: "Not in batch".into(),
            }
        },
        video,
        audio_label: if remux { "Audio" } else { "Subtitles" },
        audio,
        out_crumbs: Some(draft.out_dir.trim())
            .filter(|out_dir| !out_dir.is_empty())
            .map(|out_dir| strings(path_segments(out_dir))),
        fonts_head: fonts_count.map_or_else(|| "Fonts".into(), |count| format!("Fonts · {count}")),
        fonts,
        ladder,
        options,
        assignment,
        language_choice,
        custom_language,
    }
}

/// The ladder and the Track Assignment choices of a remux row.
fn subtitle_section(
    view: &ModeView,
    row: &Row,
    meta: Option<&mkv_core::manifest::InspectionMetadata>,
    assignment: Option<&TrackAssignment>,
) -> (Vec<LadderRung>, Vec<Choice>) {
    // A row the manifest does not list has no release, so no candidates.
    let candidates = Some(row.release.as_str())
        .filter(|release| !release.is_empty())
        .and_then(|release| view.subtitle_candidates.get(release))
        .map_or(&[][..], Vec::as_slice);
    let assigned = assignment.map(|assignment| assignment.asset_path.to_string_lossy());
    let assets_dir = view.draft.assets_dir.as_deref().unwrap_or_default().trim();
    let ladder = subtitle_ladder(
        &row.release,
        candidates,
        meta.and_then(|meta| meta.sub_summary.as_deref()),
        assigned.as_deref(),
        assets_dir,
    )
    .into_iter()
    .map(|rung| LadderRung {
        key: rung.key,
        label: rung.label,
        found: !rung.detail.is_empty(),
        detail: if rung.key == "embedded" {
            rung.detail
        } else {
            pretty_path(&rung.detail)
        },
        chosen: rung.chosen,
        tag: rung.tag,
    })
    .collect();
    let choice = |value: &str, label: String| Choice {
        value: value.into(),
        label,
    };
    let mut options = vec![choice("", "Automatic (follow the ladder)".into())];
    for path in candidates {
        let tier = match candidate_tier(path, &row.release) {
            Tier::Same => "Same folder",
            Tier::Ancestor => "Parent folder",
        };
        options.push(choice(path, format!("{} ({tier})", pretty_path(path))));
    }
    if let Some(path) =
        assigned.filter(|path| !path.is_empty() && !candidates.iter().any(|c| c == path.as_ref()))
    {
        options.push(choice(
            &path,
            format!("{} (Custom file)", pretty_path(&path)),
        ));
    }
    (ladder, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::ViewState;
    use crate::queue::test_support::manifest_of;

    fn rows(ids: &[&str]) -> Vec<Row> {
        ids.iter().map(|id| Row::test(id)).collect()
    }

    // The first row until a pick, then the pick.
    #[test]
    fn test_the_inspector_shows_the_pick_else_the_first_row() {
        let rows = rows(&["ep1", "ep2"]);
        assert_eq!(inspected_row(&rows, None).map(|(at, _)| at), Some(0));
        assert_eq!(inspected_row(&rows, Some("ep2")).map(|(at, _)| at), Some(1));
        // A pick the rows no longer hold falls back to the first row.
        assert_eq!(
            inspected_row(&rows, Some("gone")).map(|(at, _)| at),
            Some(0)
        );
        assert!(inspected_row(&[], Some("ep1")).is_none());
    }

    /// A remux view whose `Season 1` release holds two candidates, and its manifest entry for
    /// `Season 1/ep1.mkv`, inspected with FLAC audio, a video tag and two fonts.
    fn remux_view() -> (ModeView, ManifestItem) {
        let mut view = ViewState::new(None).remux;
        view.subtitle_candidates.insert(
            "Season 1".into(),
            vec!["Season 1/ep1.ass".into(), "Subs/ep1 [alt].ass".into()],
        );
        let mut entry = manifest_of(&["Season 1/ep1.mkv"]).items.remove(0);
        entry.release = "Season 1".into();
        entry.metadata.video_tag = Some("HEVC 1080p".into());
        entry.metadata.audio_summary = Some("FLAC 2.0".into());
        entry.metadata.has_flac = Some(true);
        entry.metadata.sub_summary = Some("ASS (eng)".into());
        entry.metadata.fonts_count = Some(2);
        (view, entry)
    }

    fn remux_row() -> Row {
        Row {
            release: "Season 1".into(),
            name: "ep1.mkv".into(),
            ..Row::test("Season 1/ep1.mkv")
        }
    }

    #[test]
    fn test_a_remux_inspector_reads_the_inspection_the_draft_and_the_ladder() {
        let (mut view, entry) = remux_view();
        view.draft.out_dir = " /out/Show ".into();
        view.draft.fonts_dir = "/fonts".into();
        let row = remux_row();
        let inspector = inspector_view(true, &view, 2, &row, Some(&entry), None);
        assert_eq!(inspector.number, "03");
        assert_eq!(inspector.title, "ep1.mkv");
        assert_eq!(inspector.crumbs, ["Season 1"]);
        assert_eq!(inspector.error, None);
        assert_eq!(
            inspector.pill,
            Pill {
                tone: Tone::Mute,
                label: "Queued".into()
            }
        );
        assert_eq!(inspector.video, "HEVC 1080p · copied");
        assert_eq!(inspector.audio_label, "Audio");
        assert_eq!(inspector.audio, "FLAC 2.0 → Opus 192k");
        assert_eq!(
            inspector.out_crumbs,
            Some(vec!["out".into(), "Show".into()])
        );
        // A folder set to the root has no crumbs, yet is set: the view draws no placeholder.
        let mut root = view.clone();
        root.draft.out_dir = "/".into();
        let at_root = inspector_view(true, &root, 2, &row, Some(&entry), None);
        assert_eq!(at_root.out_crumbs, Some(vec![]));
        assert_eq!(inspector.fonts_head, "Fonts · 2");
        assert_eq!(inspector.fonts, "2 attached fonts · folder: fonts");
        let ladder: Vec<_> = inspector
            .ladder
            .iter()
            .map(|rung| {
                (
                    rung.key,
                    rung.detail.as_str(),
                    rung.found,
                    rung.chosen,
                    rung.tag,
                )
            })
            .collect();
        assert_eq!(
            ladder,
            [
                ("manual", "", false, false, "None"),
                ("same", "Season 1 › ep1.ass", true, true, "Auto"),
                ("embedded", "ASS (eng)", true, false, "Found"),
                ("ancestor", "Subs › ep1 [alt].ass", true, false, "Found"),
                ("assets", "", false, false, "None"),
            ]
        );
        let options: Vec<_> = inspector
            .options
            .iter()
            .map(|choice| (choice.value.as_str(), choice.label.as_str()))
            .collect();
        assert_eq!(
            options,
            [
                ("", "Automatic (follow the ladder)"),
                ("Season 1/ep1.ass", "Season 1 › ep1.ass (Same folder)"),
                ("Subs/ep1 [alt].ass", "Subs › ep1 [alt].ass (Parent folder)"),
            ]
        );
        assert_eq!(inspector.assignment, None);

        // An assigned file no candidate names is offered as a custom one, and is the ladder's
        // manual rung.
        let assignment = TrackAssignment {
            target_video: row.id.clone(),
            asset_path: "/subs/fixed.ass".into(),
            track_type: Default::default(),
            language: None,
            title: None,
            offset_ms: None,
        };
        view.assignments.insert(row.id.clone(), assignment.clone());
        let inspector = inspector_view(true, &view, 0, &row, Some(&entry), None);
        assert_eq!(
            inspector.options.last(),
            Some(&Choice {
                value: "/subs/fixed.ass".into(),
                label: "subs › fixed.ass (Custom file)".into()
            })
        );
        assert_eq!(
            (inspector.ladder[0].tag, inspector.ladder[0].detail.as_str()),
            ("Used", "subs › fixed.ass")
        );
        assert_eq!(inspector.assignment, Some(assignment));
        // A candidate that is assigned is not offered twice.
        view.assignments.get_mut(&row.id).unwrap().asset_path = "Season 1/ep1.ass".into();
        let inspector = inspector_view(true, &view, 0, &row, Some(&entry), None);
        assert_eq!(inspector.options.len(), 3);
        // An empty path is no custom file: Automatic is the only empty option.
        view.assignments.get_mut(&row.id).unwrap().asset_path = "".into();
        let inspector = inspector_view(true, &view, 0, &row, Some(&entry), None);
        assert_eq!(inspector.options.len(), 3);
    }

    // The webview's former renderSubtitleSection language cases.
    #[test]
    fn test_the_language_select_shows_a_preset_else_the_custom_tag() {
        let with = |language: Option<&str>| TrackAssignment {
            target_video: "ep1".into(),
            asset_path: "/subs/ep1.ass".into(),
            track_type: AssignmentKind::Subtitle,
            language: language.map(str::to_string),
            title: None,
            offset_ms: None,
        };
        let choice = |assignment: Option<TrackAssignment>| language_choice(assignment.as_ref());
        assert_eq!(choice(None), ("ind", String::new()));
        assert_eq!(choice(Some(with(Some("jpn")))), ("jpn", String::new()));
        assert_eq!(choice(Some(with(Some("may")))), ("custom", "may".into()));
        // An assignment without a language is a custom tag not typed yet, not the default.
        assert_eq!(choice(Some(with(None))), ("custom", String::new()));
        assert_eq!(choice(Some(with(Some("")))), ("custom", String::new()));

        let (mut view, entry) = remux_view();
        let row = remux_row();
        view.assignments.insert(row.id.clone(), with(Some("fre")));
        let inspector = inspector_view(true, &view, 0, &row, Some(&entry), None);
        let wire = serde_json::to_value(&inspector).unwrap();
        assert_eq!(
            (&wire["language_choice"], &wire["custom_language"]),
            (&serde_json::json!("custom"), &serde_json::json!("fre"))
        );
    }

    // The webview's former subtitleAssignment cases.
    #[test]
    fn test_a_subtitle_assignment_reads_the_inspectors_fields_as_typed() {
        let assign = |language, custom, title, offset| {
            let fields = SubtitleFields {
                language,
                custom_language: custom,
                title,
                offset,
            };
            subtitle_assignment("ep1", "/subs/ep1.ass", fields)
        };
        assert_eq!(
            assign("eng", "may", "  Signs ", ""),
            TrackAssignment {
                target_video: "ep1".into(),
                asset_path: "/subs/ep1.ass".into(),
                track_type: AssignmentKind::Subtitle,
                language: Some("eng".into()),
                title: Some("Signs".into()),
                offset_ms: None,
            }
        );
        let language = |language, custom| assign(language, custom, "", "").language;
        assert_eq!(language("", "may"), Some("ind".into()));
        assert_eq!(language("custom", " may "), Some("may".into()));
        assert_eq!(language("custom", "  "), None);
        assert_eq!(assign("ind", "", " \t", "").title, None);
        // The offset as Math.trunc(Number(raw)): none when empty, no number or not a safe integer.
        for (raw, expected) in [
            ("", None),
            ("   ", None),
            ("1e3", Some(1000)),
            ("-250.9", Some(-250)),
            (" 42 ", Some(42)),
            ("\u{feff}7\u{a0}", Some(7)),
            (".5", Some(0)),
            ("-0.4", Some(0)),
            ("5.", Some(5)),
            ("+12", Some(12)),
            ("9007199254740991", Some(9_007_199_254_740_991)),
            ("-9007199254740991", Some(-9_007_199_254_740_991)),
            ("9007199254740992", None),
            ("1e20", None),
            ("1e400", None),
            ("abc", None),
            ("12abc", None),
            ("-", None),
            ("Infinity", None),
            ("inf", None),
            ("NaN", None),
        ] {
            assert_eq!(assign("ind", "", "", raw).offset_ms, expected, "{raw:?}");
        }
    }

    #[test]
    fn test_an_inspector_says_why_an_item_or_its_inspection_failed() {
        let (view, mut entry) = remux_view();
        let failed = Row {
            status: ItemStatus::Error,
            label: "Failed".into(),
            ..remux_row()
        };
        let inspector = inspector_view(true, &view, 0, &failed, Some(&entry), Some("mkvmerge 2"));
        assert_eq!(inspector.error.as_deref(), Some("mkvmerge 2"));
        assert_eq!(inspector.pill.tone, Tone::Err);
        // A failure without an error text, or with an empty one, reads the default one.
        for error in [None, Some("")] {
            let inspector = inspector_view(true, &view, 0, &failed, Some(&entry), error);
            assert_eq!(inspector.error.as_deref(), Some("Failed"), "{error:?}");
        }

        entry.metadata = Default::default();
        entry.inspection_error = Some("EBML header missing".into());
        let row = Row {
            selected: false,
            ..remux_row()
        };
        let inspector = inspector_view(true, &view, 0, &row, Some(&entry), None);
        assert_eq!(inspector.error.as_deref(), Some("EBML header missing"));
        assert_eq!(inspector.video, "Inspection failed");
        assert_eq!(
            inspector.pill,
            Pill {
                tone: Tone::Mute,
                label: "Not in batch".into()
            }
        );
        assert_eq!(inspector.audio, "No audio");
        assert_eq!(
            (inspector.fonts_head.as_str(), inspector.fonts.as_str()),
            ("Fonts", "Not inspected")
        );
    }

    #[test]
    fn test_an_extract_inspector_and_a_row_only_the_job_lists() {
        let (_, mut entry) = remux_view();
        let view = ViewState::new(None).extract;
        entry.metadata.fonts_count = Some(1);
        let row = remux_row();
        let inspector = inspector_view(false, &view, 0, &row, Some(&entry), None);
        assert_eq!(inspector.video, "HEVC 1080p");
        assert_eq!(
            (inspector.audio_label, inspector.audio.as_str()),
            ("Subtitles", "ASS (eng)")
        );
        assert_eq!(inspector.fonts, "1 attached font · folder: auto");
        assert!(inspector.ladder.is_empty() && inspector.options.is_empty());
        assert!(inspector.out_crumbs.is_none());
        entry.metadata.sub_summary = None;
        let inspector = inspector_view(false, &view, 0, &row, Some(&entry), None);
        assert_eq!(inspector.audio, "None");

        // No manifest entry: nothing inspected.
        let job_only = Row {
            release: String::new(),
            in_manifest: false,
            ..Row::test("Other/ep9.mkv")
        };
        let inspector = inspector_view(true, &ViewState::new(None).remux, 0, &job_only, None, None);
        assert_eq!(
            (inspector.video.as_str(), inspector.audio.as_str()),
            ("Unknown", "Unknown")
        );
        assert_eq!(inspector.crumbs, ["Other"]);
        assert_eq!(inspector.options.len(), 1);
    }
}
