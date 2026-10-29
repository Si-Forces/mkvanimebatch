//! The Library and the Episode Inspector (P3): the published `LibraryEntry`s and `InspectorView`
//! mapped into the window's structs (the Library and the subtitle section off the UI thread in the
//! Publisher, as the rows are; the Inspector, whose crumbs are models, on the event loop), and the
//! subtitle fields written back into the typed ones. Every text the two show that the published
//! view does not carry is here; `app.js` (`renderLibrary`, `renderInspector`) is the reference,
//! and `index.html` holds the webview's copy of each fixed text named below.

use crate::queue::tone;
use mkv_anime_batch_slint_ui::{App, Inspector, LibraryItem, Mode, Rung};
use mkv_core::request::TrackAssignment;
use mkv_desktop_core::actor::ModeView;
use mkv_desktop_core::inspector::{Choice, InspectorView, LadderRung};
use mkv_desktop_core::presentation::LibraryEntry;
use slint::{ModelRc, SharedString, VecModel};

/// The assignment select's last entry, which opens the subtitle file dialog: the view's own
/// action, not a choice the core publishes (app.js `BROWSE_SUBTITLE`).
pub(crate) const CHOOSE_FILE: &str = "Choose a file…";

/// The language select: each entry's value, as `AssignSubtitle` sends it, and its label
/// (index.html `#sub-lang-select`). The last is the custom tag.
pub(crate) const LANGUAGES: [(&str, &str); 4] = [
    ("ind", "Indonesian (ind)"),
    ("eng", "English (eng)"),
    ("jpn", "Japanese (jpn)"),
    ("custom", "Custom tag…"),
];

/// The subtitle file dialog's filter, as the Tauri host's `pick_subtitle_file` (commands.rs).
pub(crate) const SUBTITLE_FILTER: (&str, [&str; 5]) =
    ("Subtitle Files", ["ass", "ssa", "srt", "vtt", "sub"]);

/// What the Inspector's output folder shows when none is set: the output field's own
/// description of its default (index.html `#remux-out-dir`, `#extract-out-dir` placeholders; P4's
/// settings sheet shows the same texts, so keep the three in step).
fn default_out(mode: Mode) -> &'static str {
    match mode {
        Mode::Remux => "Output folder next to the input",
        Mode::Extract => "Same folder as the input",
    }
}

fn to_int(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// A select's index when no entry holds the value: the `Select` shows nothing.
const NO_SELECTION: i32 = -1;

/// The path between ` › `, as the webview's crumbs `title` joins them.
fn joined(crumbs: &[String]) -> SharedString {
    crumbs.join(crate::queue::CRUMB_SEPARATOR).into()
}

fn model(texts: Vec<SharedString>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(texts))
}

/// The Library's entries: the input folder first, each with its count.
pub(crate) fn map_library(library: &[LibraryEntry]) -> Vec<LibraryItem> {
    library
        .iter()
        .map(|entry| LibraryItem {
            release: entry.release.as_deref().unwrap_or_default().into(),
            root: entry.release.is_none(),
            label: entry.label.as_str().into(),
            tooltip: entry.tooltip.as_str().into(),
            count: to_int(entry.count),
            a11y: format!("{} {}", entry.label, entry.count).into(),
        })
        .collect()
}

/// The Inspector of `mode`, or its empty state for none, of an episode in `directory` (the
/// mode's, as the core names it). Its crumbs are models: call it on the event loop.
pub(crate) fn map_inspector(
    view: Option<&InspectorView>,
    mode: Mode,
    directory: &str,
) -> Inspector {
    let Some(view) = view else {
        return Inspector::default();
    };
    // Each crumb but the last followed by "›"; the layout spaces the crumbs.
    let texts = |crumbs: &[String]| {
        let last = crumbs.len().saturating_sub(1);
        let separator = crate::queue::CRUMB_SEPARATOR.trim_end();
        crumbs
            .iter()
            .enumerate()
            .map(|(at, crumb)| {
                if at < last {
                    format!("{crumb}{separator}").into()
                } else {
                    SharedString::from(crumb)
                }
            })
            .collect()
    };
    let (out_crumbs, out_path, out_plain) = match &view.out_crumbs {
        Some(out) => (texts(out), joined(out), false),
        None => {
            let default = SharedString::from(default_out(mode));
            (vec![default.clone()], default, true)
        }
    };
    let assignment = view.assignment.as_ref();
    let asset_path = assignment.map_or_else(String::new, |assignment| {
        assignment.asset_path.to_string_lossy().into_owned()
    });
    Inspector {
        shown: true,
        directory: directory.into(),
        id: view.id.as_str().into(),
        number: view.number.as_str().into(),
        title: view.title.as_str().into(),
        crumbs: model(texts(&view.crumbs)),
        path: joined(&view.crumbs),
        error: view.error.as_deref().unwrap_or_default().into(),
        pill_label: view.pill.label.as_str().into(),
        pill_tone: tone(view.pill.tone),
        video: view.video.as_str().into(),
        audio_label: view.audio_label.into(),
        audio: view.audio.as_str().into(),
        out_crumbs: model(out_crumbs),
        out_path,
        out_plain,
        // Upper case, as style.css draws the section headings.
        fonts_head: view.fonts_head.to_uppercase().into(),
        fonts: view.fonts.as_str().into(),
        subtitles: mode == Mode::Remux,
        assigned: assignment.is_some(),
        // The select shows what is assigned, else Automatic (app.js `select.value`); none when
        // no entry has that value.
        choice: view
            .options
            .iter()
            .position(|choice| choice.value == asset_path)
            .map_or(NO_SELECTION, to_int),
        language: LANGUAGES
            .iter()
            .position(|(value, _)| *value == view.language_choice)
            .map_or(NO_SELECTION, to_int),
        asset_path: asset_path.into(),
        custom_language: view.custom_language.as_str().into(),
        track_title: assignment
            .and_then(|assignment| assignment.title.as_deref())
            .unwrap_or_default()
            .into(),
        offset: assignment
            .and_then(|assignment| assignment.offset_ms)
            .map_or_else(String::new, |ms| ms.to_string())
            .into(),
    }
}

/// The subtitle section's ladder and select entries; the select's values for the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Subtitles {
    pub(crate) ladder: Vec<Rung>,
    /// Every published choice's label, then [`CHOOSE_FILE`].
    pub(crate) options: Vec<SharedString>,
    /// Every published choice's value.
    pub(crate) values: Vec<SharedString>,
}

pub(crate) fn map_subtitles(view: &InspectorView) -> Subtitles {
    let ladder = view
        .ladder
        .iter()
        .enumerate()
        .map(|(at, rung)| {
            let num = (at + 1).to_string();
            let mut a11y = format!("{num} {}", rung.label);
            if rung.found {
                a11y = format!("{a11y}, {}", rung.detail);
            }
            Rung {
                num: num.into(),
                label: rung.label.into(),
                detail: rung.detail.as_str().into(),
                tag: rung.tag.into(),
                found: rung.found,
                chosen: rung.chosen,
                a11y: format!("{a11y}, {}", rung.tag).into(),
            }
        })
        .collect();
    let options = view
        .options
        .iter()
        .map(|choice| choice.label.as_str())
        .chain([CHOOSE_FILE])
        .map(SharedString::from)
        .collect();
    let values = view
        .options
        .iter()
        .map(|choice| SharedString::from(choice.value.as_str()))
        .collect();
    Subtitles {
        ladder,
        options,
        values,
    }
}

/// What the subtitle section redraws on, so a progress publication redraws none of it and the
/// typed fields keep what is typed (app.js `lastAssignKey`): the folder too, so the same id in
/// another folder is another episode.
type FormKey = (
    Option<String>,
    String,
    Vec<LadderRung>,
    Vec<Choice>,
    Option<TrackAssignment>,
);

fn form_key(directory: &Option<String>, view: &InspectorView) -> FormKey {
    (
        directory.clone(),
        view.id.clone(),
        view.ladder.clone(),
        view.options.clone(),
        view.assignment.clone(),
    )
}

/// What the event loop writes into a mode's Inspector.
#[derive(Debug)]
pub(crate) struct InspectorUpdate {
    /// The Inspector to show ([`map_inspector`] on the event loop); none for its empty state.
    pub(crate) view: Option<InspectorView>,
    /// The mode's folder, the episode's (`ModeView::directory`).
    pub(crate) directory: String,
    /// The subtitle section to redraw, with the typed fields written back from the Inspector
    /// mapped from `view`;
    /// none while it keeps what it shows.
    pub(crate) subtitles: Option<Subtitles>,
}

/// One mode's Library and Inspector between the Publisher and the window: what the window was
/// last given and what is staged for the next take.
#[derive(Default)]
pub(crate) struct InspectorList {
    library: Option<Vec<LibraryEntry>>,
    /// The folder and the Inspector last staged.
    inspector: Option<(Option<String>, Option<InspectorView>)>,
    key: Option<FormKey>,
    /// Whether the form key changed since the last take.
    form: bool,
    staged_library: Option<Vec<LibraryItem>>,
    staged: Option<InspectorUpdate>,
}

impl InspectorList {
    /// Stages what changed in `view` for `mode`.
    pub(crate) fn put(&mut self, view: &ModeView, mode: Mode) {
        if self.library.as_ref() != Some(&view.library) {
            self.staged_library = Some(map_library(&view.library));
            self.library = Some(view.library.clone());
        }
        let shown = (view.directory.clone(), view.inspector.clone());
        if self.inspector.as_ref() == Some(&shown) {
            return;
        }
        let key = view
            .inspector
            .as_ref()
            .map(|inspector| form_key(&view.directory, inspector));
        self.form |= key != self.key && mode == Mode::Remux;
        self.key = key;
        self.staged = Some(InspectorUpdate {
            view: view.inspector.clone(),
            directory: view.directory.clone().unwrap_or_default(),
            subtitles: view
                .inspector
                .as_ref()
                .filter(|_| self.form)
                .map(map_subtitles),
        });
        self.inspector = Some(shown);
    }

    /// The Library and the Inspector to apply now, if they changed.
    pub(crate) fn take(&mut self) -> (Option<Vec<LibraryItem>>, Option<InspectorUpdate>) {
        let staged = self.staged.take();
        if staged.is_some() {
            self.form = false;
        }
        (self.staged_library.take(), staged)
    }
}

/// The language tag field's longest text: an ISO 639-2 code (index.html `#sub-lang-custom`
/// `maxlength="3"`).
const TAG_LEN: usize = 3;

/// `text` cut to the language tag field's length, or none when it fits.
pub(crate) fn capped_tag(text: &str) -> Option<String> {
    text.char_indices()
        .nth(TAG_LEN)
        .map(|(at, _)| text[..at].to_string())
}

/// The same episode: the same id in the same folder, compared as the core names them (a relative
/// id alone is no identity; the crumbs are display only).
fn same_episode(a: &Inspector, b: &Inspector) -> bool {
    a.shown && b.shown && a.directory == b.directory && a.id == b.id
}

/// Writes `inspector`'s published subtitle fields into the typed ones, over `previous`, the
/// remux Inspector shown until now. A text field typed into and not committed yet keeps what is
/// typed while the same episode stays assigned, for its commit to send (app.js redraws over it);
/// another episode's, or one no longer assigned, drops it.
pub(crate) fn write_form(app: &App, previous: &Inspector, inspector: &Inspector) {
    app.set_sub_choice(inspector.choice);
    app.set_sub_language(inspector.language);
    if app.get_sub_dirty() && inspector.assigned && same_episode(previous, inspector) {
        return;
    }
    app.set_sub_dirty(false);
    app.set_sub_custom(inspector.custom_language.clone());
    app.set_sub_title(inspector.track_title.clone());
    app.set_sub_offset(inspector.offset.clone());
}

/// Puts the typed subtitle fields back on what the remux Inspector shows, after an Intent was
/// answered (refused too) or the file dialog was cancelled.
pub(crate) fn restore_form(app: &App) {
    let shown = app.get_remux_inspector();
    write_form(app, &shown, &shown);
}

/// The typed subtitle fields, as `AssignSubtitle` sends them: language, custom tag, title,
/// offset.
pub(crate) fn typed_fields(app: &App) -> (String, String, String, String) {
    let language = usize::try_from(app.get_sub_language())
        .ok()
        .and_then(|at| LANGUAGES.get(at))
        .map_or("", |(value, _)| value);
    (
        language.to_string(),
        app.get_sub_custom().into(),
        app.get_sub_title().into(),
        app.get_sub_offset().into(),
    )
}

/// A remux row's Inspector: queued at 40%, nothing assigned, two ladder rungs and two choices.
#[cfg(test)]
pub(crate) fn sample_view() -> InspectorView {
    let rung = |key, label, detail: &str, chosen, tag| LadderRung {
        key,
        label,
        detail: detail.into(),
        chosen,
        found: !detail.is_empty(),
        tag,
    };
    InspectorView {
        id: "Season 1/ep1.mkv".into(),
        number: "03".into(),
        title: "ep1.mkv".into(),
        crumbs: vec!["media".into(), "Season 1".into()],
        error: None,
        pill: mkv_desktop_core::inspector::Pill {
            tone: mkv_desktop_core::presentation::Tone::Info,
            label: "40% · 3s".into(),
        },
        video: "HEVC 1080p · copied".into(),
        audio_label: "Audio",
        audio: "FLAC 2.0 → Opus 192k".into(),
        out_crumbs: None,
        fonts_head: "Fonts · 2".into(),
        fonts: "2 attached fonts · folder: auto".into(),
        ladder: vec![
            rung("manual", "Manual pick", "", false, "None"),
            rung("same", "Same folder", "Season 1 › ep1.ass", true, "Auto"),
        ],
        options: vec![
            Choice {
                value: String::new(),
                label: "Automatic (follow the ladder)".into(),
            },
            Choice {
                value: "Season 1/ep1.ass".into(),
                label: "Season 1 › ep1.ass (Same folder)".into(),
            },
        ],
        assignment: None,
        language_choice: "ind",
        custom_language: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_anime_batch_slint_ui::RowTone;
    use mkv_core::request::AssignmentKind;
    use mkv_desktop_core::actor::ViewState;
    use slint::Model;

    fn assignment(language: Option<&str>) -> TrackAssignment {
        TrackAssignment {
            target_video: "Season 1/ep1.mkv".into(),
            asset_path: "Season 1/ep1.ass".into(),
            track_type: AssignmentKind::Subtitle,
            language: language.map(Into::into),
            title: Some("Signs".into()),
            offset_ms: Some(-250),
        }
    }

    /// The language select's presets are the core's, in its order, then the custom tag.
    #[test]
    fn the_language_select_holds_the_core_presets_then_custom() {
        let values: Vec<_> = LANGUAGES.iter().map(|(value, _)| *value).collect();
        assert_eq!(
            values[..3],
            mkv_desktop_core::inspector::PRESET_SUBTITLE_LANGUAGES
        );
        assert_eq!(values[3], "custom");
    }

    #[test]
    fn the_language_tag_is_cut_to_three_characters() {
        assert_eq!(capped_tag("may"), None);
        assert_eq!(capped_tag("ma"), None);
        assert_eq!(capped_tag("mayxx"), Some("may".into()));
        assert_eq!(capped_tag("ábcd"), Some("ábc".into()));
    }

    #[test]
    fn the_library_maps_its_folder_then_each_release_with_its_count() {
        let entry = |release: Option<&str>, label: &str, count| LibraryEntry {
            release: release.map(Into::into),
            label: label.into(),
            tooltip: format!("media › {label}"),
            count,
            group_label: label.into(),
        };
        let items = map_library(&[entry(None, "Show", 3), entry(Some("S2"), "S2", 2)]);
        let shown: Vec<_> = items
            .iter()
            .map(|item| {
                (
                    item.release.as_str(),
                    item.root,
                    item.label.as_str(),
                    item.count,
                    item.a11y.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("", true, "Show", 3, "Show 3"),
                ("S2", false, "S2", 2, "S2 2")
            ]
        );
        assert_eq!(items[1].tooltip, "media › S2");
    }

    #[test]
    fn the_inspector_maps_every_field_and_the_published_subtitle_fields() {
        let mut view = sample_view();
        let insp = map_inspector(Some(&view), Mode::Remux, "");
        assert!(insp.shown && insp.subtitles && !insp.assigned);
        assert_eq!(
            (insp.number.as_str(), insp.title.as_str()),
            ("03", "ep1.mkv")
        );
        let texts = |model: &ModelRc<SharedString>| model.iter().collect::<Vec<_>>();
        assert_eq!(texts(&insp.crumbs), ["media ›", "Season 1"]);
        assert_eq!(insp.path, "media › Season 1");
        assert_eq!(insp.error, "");
        assert_eq!(
            (insp.pill_label.as_str(), insp.pill_tone),
            ("40% · 3s", RowTone::Info)
        );
        assert_eq!(
            (insp.audio_label.as_str(), insp.audio.as_str()),
            ("Audio", "FLAC 2.0 → Opus 192k")
        );
        // No output folder: the setting's default, as the field's placeholder says it.
        assert_eq!(
            (
                texts(&insp.out_crumbs),
                insp.out_path.as_str(),
                insp.out_plain
            ),
            (
                vec!["Output folder next to the input".into()],
                "Output folder next to the input",
                true
            )
        );
        let extract = map_inspector(Some(&view), Mode::Extract, "");
        assert_eq!(extract.out_path, "Same folder as the input");
        assert!(!extract.subtitles);
        assert_eq!(insp.fonts_head, "FONTS · 2");
        // Automatic, Indonesian, nothing typed.
        assert_eq!((insp.choice, insp.language), (0, 0));
        assert_eq!((insp.track_title.as_str(), insp.offset.as_str()), ("", ""));

        view.error = Some("mkvmerge 2".into());
        view.out_crumbs = Some(vec!["out".into(), "Show".into()]);
        view.assignment = Some(assignment(Some("may")));
        view.language_choice = "custom";
        view.custom_language = "may".into();
        let insp = map_inspector(Some(&view), Mode::Remux, "");
        assert_eq!(insp.error, "mkvmerge 2");
        assert_eq!(
            (
                texts(&insp.out_crumbs),
                insp.out_path.as_str(),
                insp.out_plain
            ),
            (vec!["out ›".into(), "Show".into()], "out › Show", false)
        );
        assert!(insp.assigned);
        assert_eq!(insp.asset_path, "Season 1/ep1.ass");
        assert_eq!((insp.choice, insp.language), (1, 3));
        assert_eq!(
            (
                insp.custom_language.as_str(),
                insp.track_title.as_str(),
                insp.offset.as_str()
            ),
            ("may", "Signs", "-250")
        );
        assert!(!map_inspector(None, Mode::Remux, "").shown);
    }

    #[test]
    fn the_subtitle_section_lists_the_ladder_and_the_choices_then_the_file_dialog() {
        let subtitles = map_subtitles(&sample_view());
        let rungs: Vec<_> = subtitles
            .ladder
            .iter()
            .map(|rung| {
                (
                    rung.num.as_str(),
                    rung.found,
                    rung.chosen,
                    rung.a11y.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rungs,
            [
                ("1", false, false, "1 Manual pick, None"),
                ("2", true, true, "2 Same folder, Season 1 › ep1.ass, Auto"),
            ]
        );
        assert_eq!(
            subtitles.options,
            [
                "Automatic (follow the ladder)",
                "Season 1 › ep1.ass (Same folder)",
                CHOOSE_FILE
            ]
        );
        assert_eq!(subtitles.values, ["", "Season 1/ep1.ass"]);
    }

    /// The typed fields are written back only when the inspected row, its ladder, its choices or
    /// its assignment changed: a progress publication keeps what is typed.
    #[test]
    fn the_subtitle_fields_are_written_back_only_when_the_assignment_key_changes() {
        let mut view = ViewState::new(None).remux;
        let mut list = InspectorList::default();
        let mut put = |view: &ModeView| {
            list.put(view, Mode::Remux);
            list.take()
        };
        let (library, inspector) = put(&view);
        assert_eq!(library.map(|items| items.len()), Some(0));
        assert!(inspector.is_some_and(|update| update.view.is_none()));

        view.inspector = Some(sample_view());
        let (library, inspector) = put(&view);
        assert!(
            library.is_none(),
            "an unchanged Library is not written again"
        );
        assert!(inspector.is_some_and(|update| update.subtitles.is_some()));
        // Progress: the pill changes, the subtitle section does not.
        view.inspector.as_mut().unwrap().pill.label = "50% · 2s".into();
        let (_, inspector) = put(&view);
        let update = inspector.unwrap();
        assert_eq!(update.view.unwrap().pill.label, "50% · 2s");
        assert!(update.subtitles.is_none());
        // Nothing changed: nothing is written.
        assert!(put(&view).1.is_none());
        // An assignment: written back.
        view.inspector.as_mut().unwrap().assignment = Some(assignment(Some("eng")));
        assert!(put(&view).1.unwrap().subtitles.is_some());

        // Two publications in one hop: a key change in the first is still written back.
        view.inspector.as_mut().unwrap().assignment = None;
        list.put(&view, Mode::Remux);
        view.inspector.as_mut().unwrap().pill.label = "60% · 1s".into();
        list.put(&view, Mode::Remux);
        assert!(list.take().1.unwrap().subtitles.is_some());

        // Extract has no subtitle section.
        let mut extract = InspectorList::default();
        extract.put(&view, Mode::Extract);
        assert!(extract.take().1.unwrap().subtitles.is_none());
    }
}
