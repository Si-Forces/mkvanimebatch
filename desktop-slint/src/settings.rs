//! The settings sheet (P4): each mode's published Settings Draft mapped into the sheet's fields,
//! the edits its fields send, the steppers' hold-to-repeat, the folder dialog behind Browse…, the
//! Preset Profiles and the chips. `settings-draft.js`, `settings-chips.js`, `preset-menu.js` and
//! `app.js` (`initSteppers`, `chooseInputDirectory`) are the reference.
//!
//! The write-back rules are `settings-draft.js`'s: a publication never writes a field that has the
//! focus, so typing keeps its caret, nor a key whose edit is still on its way to the core, since a
//! state published before the core applied it holds the value from before the edit. Once the
//! core answers the last pending edit of a key, a refused one puts the published value back (into
//! the focused field too) and a no-op one (`false`) does only for the complexity, which the core
//! normalizes; the complexity also gets the published value back when it is left.

use crate::intents::{pipeline, Intents, Picking};
use crate::publisher::index;
use mkv_anime_batch_slint_ui::{App, ChipTexts, DraftForm, Mode, PresetItem, Sheet};
use mkv_core::preset::PresetProfile;
use mkv_desktop_core::actor::{Intent, ViewState};
use mkv_desktop_core::draft::{DraftChanges, SettingsDraft, StepField};
use mkv_desktop_core::presentation::Chips;
use mkv_desktop_core::queue::Direction;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// The bitrate select's entries (index.html `#remux-bitrate`); the form holds the index.
pub(crate) const BITRATES: [&str; 5] = ["128k", "160k", "192k", "256k", "320k"];

/// The draft keys, as the sheet's fields name them and `DraftChanges` spells them.
const KEYS: [&str; 12] = [
    "input_dir",
    "out_dir",
    "fonts_dir",
    "assets_dir",
    "bitrate",
    "complexity",
    "sub_offset",
    "workers",
    "io_jobs",
    "ffmpeg_threads",
    "dry_run",
    "no_cache",
];

/// The key the core normalizes on edit (draft.rs `clamp_complexity`): a field left, or whose edit
/// the core answers as a no-op, gets its published value back (settings-draft.js `NORMALIZED`).
const NORMALIZED: &str = "complexity";

/// A stepper held down steps again after this, then every [`REPEAT_EVERY`] (app.js
/// `initSteppers`).
pub(crate) const REPEAT_DELAY: Duration = Duration::from_millis(400);
pub(crate) const REPEAT_EVERY: Duration = Duration::from_millis(80);

/// The draft key a field names, if it names one.
fn key_of(name: &str) -> Option<&'static str> {
    KEYS.iter().copied().find(|key| *key == name)
}

/// `draft` as the sheet's fields show it; the fields a mode lacks are empty.
pub(crate) fn map_draft(draft: &SettingsDraft) -> DraftForm {
    let text = |value: &Option<String>| SharedString::from(value.as_deref().unwrap_or_default());
    let bitrate = draft
        .bitrate
        .as_deref()
        .and_then(|bitrate| BITRATES.iter().position(|entry| *entry == bitrate))
        .and_then(|at| i32::try_from(at).ok())
        .unwrap_or(-1);
    DraftForm {
        input_dir: draft.input_dir.as_str().into(),
        out_dir: draft.out_dir.as_str().into(),
        fonts_dir: draft.fonts_dir.as_str().into(),
        assets_dir: text(&draft.assets_dir),
        bitrate,
        complexity: text(&draft.complexity),
        sub_offset: text(&draft.sub_offset),
        workers: draft.workers.as_str().into(),
        io_jobs: draft.io_jobs.as_str().into(),
        ffmpeg_threads: text(&draft.ffmpeg_threads),
        dry_run: draft.dry_run,
        no_cache: draft.no_cache,
    }
}

/// The text field of `key` in `form`; `None` for the select and the checkboxes.
fn text_field<'a>(form: &'a mut DraftForm, key: &str) -> Option<&'a mut SharedString> {
    Some(match key {
        "input_dir" => &mut form.input_dir,
        "out_dir" => &mut form.out_dir,
        "fonts_dir" => &mut form.fonts_dir,
        "assets_dir" => &mut form.assets_dir,
        "complexity" => &mut form.complexity,
        "sub_offset" => &mut form.sub_offset,
        "workers" => &mut form.workers,
        "io_jobs" => &mut form.io_jobs,
        "ffmpeg_threads" => &mut form.ffmpeg_threads,
        _ => return None,
    })
}

/// Copies `key`'s field from `from` into `to`; true when that changed it.
fn copy_key(to: &mut DraftForm, from: &DraftForm, key: &str) -> bool {
    fn put<T: PartialEq + Clone>(to: &mut T, from: &T) -> bool {
        let changed = to != from;
        if changed {
            to.clone_from(from);
        }
        changed
    }
    match key {
        "bitrate" => put(&mut to.bitrate, &from.bitrate),
        "dry_run" => put(&mut to.dry_run, &from.dry_run),
        "no_cache" => put(&mut to.no_cache, &from.no_cache),
        _ => {
            let mut from = from.clone();
            match (text_field(to, key), text_field(&mut from, key)) {
                (Some(to), Some(from)) => put(to, from),
                _ => false,
            }
        }
    }
}

/// The `EditDraft` changes that send `key` as `form` holds it.
pub(crate) fn changes(form: &DraftForm, key: &str) -> DraftChanges {
    let mut changes = DraftChanges::default();
    let text = |value: &SharedString| Some(value.to_string());
    match key {
        "input_dir" => changes.input_dir = text(&form.input_dir),
        "out_dir" => changes.out_dir = text(&form.out_dir),
        "fonts_dir" => changes.fonts_dir = text(&form.fonts_dir),
        "assets_dir" => changes.assets_dir = text(&form.assets_dir),
        "bitrate" => {
            // A select with no entry chosen holds the empty value, as a webview `<select>` does.
            let at = usize::try_from(form.bitrate).ok();
            let value = at.and_then(|at| BITRATES.get(at)).copied().unwrap_or("");
            changes.bitrate = Some(value.into());
        }
        "complexity" => changes.complexity = text(&form.complexity),
        "sub_offset" => changes.sub_offset = text(&form.sub_offset),
        "workers" => changes.workers = text(&form.workers),
        "io_jobs" => changes.io_jobs = text(&form.io_jobs),
        "ffmpeg_threads" => changes.ffmpeg_threads = text(&form.ffmpeg_threads),
        "dry_run" => changes.dry_run = Some(form.dry_run),
        "no_cache" => changes.no_cache = Some(form.no_cache),
        _ => {}
    }
    changes
}

/// The draft field a stepper of `key` steps.
fn step_field(key: &str) -> Option<StepField> {
    Some(match key {
        "sub_offset" => StepField::SubOffset,
        "workers" => StepField::Workers,
        "io_jobs" => StepField::IoJobs,
        "ffmpeg_threads" => StepField::FfmpegThreads,
        _ => return None,
    })
}

/// The chips and the Workers and Disk I/O placeholders: the published hint, else the fields' own
/// `auto` (settings-chips.js).
pub(crate) fn map_chips(chips: &Chips) -> ChipTexts {
    let hint = |hint: &Option<String>| SharedString::from(hint.as_deref().unwrap_or("auto"));
    ChipTexts {
        workers: chips.workers.as_str().into(),
        io_jobs: chips.io_jobs.as_str().into(),
        workers_hint: hint(&chips.workers_hint),
        io_jobs_hint: hint(&chips.io_jobs_hint),
    }
}

/// A Preset Profile row's subtitle: the encode it sets (preset-menu.js `presetSubtitle`).
fn preset_sub(bitrate: &str, complexity: impl std::fmt::Display) -> String {
    format!("Opus {bitrate} · complexity {complexity}")
}

/// The Preset Profile rows over the remux `draft`: its preset marked active, and the custom row
/// showing the draft's own encode while its complexity reads as a number (preset-menu.js).
pub(crate) fn map_presets(presets: &[PresetProfile], draft: &SettingsDraft) -> Vec<PresetItem> {
    let own = match (&draft.bitrate, &draft.complexity) {
        (Some(bitrate), Some(complexity)) => complexity
            .trim()
            .parse::<i64>()
            .ok()
            .map(|complexity| preset_sub(bitrate, complexity)),
        _ => None,
    };
    presets
        .iter()
        .map(|preset| {
            let sub = match &own {
                Some(own) if preset.id == "custom" => own.clone(),
                _ => preset_sub(&preset.bitrate, preset.complexity),
            };
            PresetItem {
                a11y: format!("{}, {sub}", preset.name).into(),
                id: preset.id.as_str().into(),
                name: preset.name.as_str().into(),
                sub: sub.into(),
                active: draft.preset.as_deref() == Some(preset.id.as_str()),
            }
        })
        .collect()
}

/// Draft edits sent and not answered yet, by mode and key (settings-draft.js `pending`).
#[derive(Debug, Default)]
pub(crate) struct PendingEdits([HashMap<&'static str, u32>; 2]);

impl PendingEdits {
    fn begin(&mut self, mode: Mode, key: &'static str) {
        *self.0[index(mode)].entry(key).or_default() += 1;
    }

    /// One edit of `key` was answered; true when none is left.
    fn end(&mut self, mode: Mode, key: &'static str) -> bool {
        let pending = &mut self.0[index(mode)];
        match pending.get_mut(key) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            _ => {
                pending.remove(key);
                true
            }
        }
    }

    fn has(&self, mode: Mode, key: &str) -> bool {
        self.0[index(mode)].contains_key(key)
    }
}

/// The shared pending edits, as the publisher and the sheet's callbacks hold them.
pub(crate) type Edits = Arc<Mutex<PendingEdits>>;

fn lock(edits: &Edits) -> std::sync::MutexGuard<'_, PendingEdits> {
    edits.lock().unwrap_or_else(PoisonError::into_inner)
}

fn form(app: &App, mode: Mode) -> DraftForm {
    match mode {
        Mode::Remux => app.get_remux_form(),
        Mode::Extract => app.get_extract_form(),
    }
}

fn set_form(app: &App, mode: Mode, form: DraftForm) {
    match mode {
        Mode::Remux => app.set_remux_form(form),
        Mode::Extract => app.set_extract_form(form),
    }
}

fn published(app: &App, mode: Mode) -> DraftForm {
    match mode {
        Mode::Remux => app.get_remux_draft(),
        Mode::Extract => app.get_extract_draft(),
    }
}

/// Whether `mode`'s field of `key` has the focus: the sheet is open on `mode` and holds it there.
fn has_focus(app: &App, mode: Mode, key: &str) -> bool {
    app.get_sheet_open() && app.get_mode() == mode && app.global::<Sheet>().get_focused() == key
}

/// Writes the published `key` of `mode` back into its field, unless it has the focus (any, with
/// `force`).
pub(crate) fn restore(app: &App, mode: Mode, key: &str, force: bool) {
    if !force && has_focus(app, mode, key) {
        return;
    }
    let mut typed = form(app, mode);
    if copy_key(&mut typed, &published(app, mode), key) {
        set_form(app, mode, typed);
    }
}

/// Shows `state`'s drafts, flags, chips and Preset Profiles, on the event loop. Each mode's
/// published draft is written into its fields but those `edits` holds pending and the one with
/// the focus.
pub(crate) fn show(app: &App, state: &ViewState, edits: &Edits) {
    for (mode, view) in [(Mode::Remux, &state.remux), (Mode::Extract, &state.extract)] {
        let draft = map_draft(&view.draft);
        let invalid = SharedString::from(view.draft.invalid.unwrap_or_default());
        let mut typed = form(app, mode);
        let mut changed = false;
        {
            let pending = lock(edits);
            for key in KEYS {
                if !pending.has(mode, key) && !has_focus(app, mode, key) {
                    changed |= copy_key(&mut typed, &draft, key);
                }
            }
        }
        match mode {
            Mode::Remux => {
                app.set_remux_draft(draft);
                app.set_remux_invalid(invalid);
            }
            Mode::Extract => {
                app.set_extract_draft(draft);
                app.set_extract_invalid(invalid);
            }
        }
        if changed {
            set_form(app, mode, typed);
        }
    }
    let chips = map_chips(&state.chips);
    if app.get_chips() != chips {
        app.set_chips(chips);
    }
    let presets = map_presets(&state.presets, &state.remux.draft);
    if !app.get_presets().iter().eq(presets.iter().cloned()) {
        app.set_presets(ModelRc::new(VecModel::from(presets)));
    }
}

/// An edit of `mode`'s `key` was answered: once none of the key is left pending, a refused one puts
/// the published value back (into the focused field too), and a no-op one does for the complexity
/// unless it has the focus (settings-draft.js `guarded`).
fn settle(app: &App, edits: &Edits, mode: Mode, key: &'static str, answer: Result<bool, String>) {
    if !lock(edits).end(mode, key) {
        return;
    }
    match answer {
        Err(_) => restore(app, mode, key, true),
        Ok(false) if key == NORMALIZED => restore(app, mode, key, false),
        Ok(_) => {}
    }
}

/// After a refused Run of `mode`: opens the sheet on the field its draft flags, so the error
/// shows (job-controller.js `start`). False when none is flagged.
#[allow(
    dead_code,
    reason = "the Run button (P5) calls it once a Run is refused"
)]
pub(crate) fn open_on_flagged(app: &App, mode: Mode) -> bool {
    let invalid = match mode {
        Mode::Remux => app.get_remux_invalid(),
        Mode::Extract => app.get_extract_invalid(),
    };
    if invalid.is_empty() {
        return false;
    }
    app.invoke_open_sheet(mode, invalid);
    true
}

/// A stepper held down: a step at once, then every [`REPEAT_EVERY`] once held for
/// [`REPEAT_DELAY`], until released.
#[derive(Default)]
pub(crate) struct Repeater {
    delay: Timer,
    every: Timer,
}

impl Repeater {
    pub(crate) fn press(self: &Rc<Self>, step: impl Fn() + 'static) {
        self.release();
        let step = Rc::new(step);
        step();
        let this: Weak<Self> = Rc::downgrade(self);
        self.delay
            .start(TimerMode::SingleShot, REPEAT_DELAY, move || {
                if let Some(this) = this.upgrade() {
                    let step = Rc::clone(&step);
                    this.every
                        .start(TimerMode::Repeated, REPEAT_EVERY, move || step());
                }
            });
    }

    pub(crate) fn release(&self) {
        self.delay.stop();
        self.every.stop();
    }
}

/// The sheet's sends: the core, and the edits on their way to it.
#[derive(Clone)]
struct Drafts {
    send: Intents,
    edits: Edits,
}

impl Drafts {
    /// Sends `intent`, which edits `mode`'s draft `key`, pending until answered; then, once no
    /// edit of the key is left pending, puts the published value back as the rules say.
    fn guarded(&self, mode: Mode, key: &'static str, name: &'static str, intent: Intent) {
        lock(&self.edits).begin(mode, key);
        let (window, edits) = (self.send.window.clone(), Arc::clone(&self.edits));
        self.send.send_answered(name, intent, move |answer| {
            let _ =
                window.upgrade_in_event_loop(move |app| settle(&app, &edits, mode, key, answer));
        });
    }

    /// Sends `key` as `mode`'s field holds it.
    fn edit(&self, app: &App, mode: Mode, key: &'static str) {
        let changes = changes(&form(app, mode), key);
        let intent = Intent::EditDraft {
            mode: pipeline(mode),
            changes,
        };
        self.guarded(mode, key, "edit_draft", intent);
    }

    /// Steps `mode`'s `key` once; nothing while a batch locks the fields.
    fn step(&self, app: &App, mode: Mode, key: &'static str, up: bool) {
        let Some(field) = step_field(key).filter(|_| !app.get_busy()) else {
            return;
        };
        let direction = if up { Direction::Up } else { Direction::Down };
        let intent = Intent::StepDraft {
            mode: pipeline(mode),
            field,
            direction,
        };
        self.guarded(mode, key, "step_draft", intent);
    }

    /// Scans `mode`'s input folder `path` (job-controller.js's input-dir rescan).
    fn scan(&self, mode: Mode, path: String) {
        let intent = Intent::ScanDirectory {
            mode: pipeline(mode),
            path,
            force: false,
            no_cache: false,
        };
        self.send.send("scan_directory", intent, || {});
    }

    /// The input folder as typed, trimmed, scanned.
    fn rescan(&self, app: &App, mode: Mode) {
        self.scan(mode, form(app, mode).input_dir.trim().to_string());
    }

    /// The folder dialog behind Browse… (and Open folder) answered for `mode`'s `key`, which
    /// opened it: the folder goes into that field and is sent, and an input folder is scanned
    /// (app.js `chooseInputDirectory`). Nothing once a batch started meanwhile.
    fn folder_picked(&self, app: &App, mode: Mode, key: &'static str, picked: Option<String>) {
        let Some(path) = picked.filter(|_| !app.get_busy()) else {
            return;
        };
        let mut typed = form(app, mode);
        if let Some(field) = text_field(&mut typed, key) {
            *field = path.as_str().into();
        }
        set_form(app, mode, typed);
        self.edit(app, mode, key);
        if key == "input_dir" {
            self.scan(mode, path);
        }
    }
}

/// Opens the folder dialog for `mode`'s `key`, one dialog at a time and not while a batch runs.
fn browse(drafts: &Drafts, picking: &Arc<AtomicBool>, app: &App, mode: Mode, key: &'static str) {
    if app.get_busy() || picking.swap(true, Ordering::Relaxed) {
        return;
    }
    let (drafts, picking, window) = (drafts.clone(), Picking(Arc::clone(picking)), app.as_weak());
    // The dialog blocks until it is answered: never on the event loop.
    drafts.send.runtime.clone().spawn_blocking(move || {
        let picked = rfd::FileDialog::new().pick_folder();
        // Back on the event loop, so the Intents keep their order with the other callbacks'.
        let _ = window.upgrade_in_event_loop(move |app| {
            drop(picking);
            let picked = picked.map(|path| path.to_string_lossy().into_owned());
            drafts.folder_picked(&app, mode, key, picked);
        });
    });
}

/// A callback that runs `act` on the window and the mode it shows with the draft key it is called
/// with, if it is one.
fn on_key(
    window: slint::Weak<App>,
    act: impl Fn(&App, Mode, &'static str) + 'static,
) -> impl Fn(SharedString) + 'static {
    move |name| {
        if let (Some(app), Some(key)) = (window.upgrade(), key_of(&name)) {
            act(&app, app.get_mode(), key);
        }
    }
}

/// Wires the sheet's fields, its steppers, Browse…, Open folder and the Preset Profiles. Each acts
/// on the mode the window shows (the sheet's), the presets on remux.
pub(crate) fn wire(app: &App, send: Intents, edits: Edits) {
    let drafts = Drafts { send, edits };
    let sheet = app.global::<Sheet>();

    let d = drafts.clone();
    sheet.on_edited(on_key(app.as_weak(), move |app: &App, mode, key| {
        d.edit(app, mode, key)
    }));

    // A change sends the edit again, as the webview's `change` does; the input folder rescans.
    let d = drafts.clone();
    sheet.on_committed(on_key(app.as_weak(), move |app: &App, mode, key| {
        d.edit(app, mode, key);
        if key == "input_dir" {
            d.rescan(app, mode);
        }
    }));

    let d = drafts.clone();
    sheet.on_left(on_key(
        app.as_weak(),
        move |app: &App, mode, key| match key {
            NORMALIZED => restore(app, mode, key, false),
            "input_dir" => d.rescan(app, mode),
            _ => {}
        },
    ));

    // The slider: the box follows it at once, and the value is sent.
    let (d, shown) = (drafts.clone(), app.as_weak());
    sheet.on_slid(move |value| {
        if let Some(app) = shown.upgrade() {
            let mode = app.get_mode();
            let mut typed = form(&app, mode);
            typed.complexity = value.round().to_string().into();
            set_form(&app, mode, typed);
            d.edit(&app, mode, NORMALIZED);
        }
    });

    // A held stepper repeats until released; a stepper of a sheet that closed under the pointer
    // stops, as its release can no longer arrive.
    let repeater = Rc::new(Repeater::default());
    let (d, held, shown) = (drafts.clone(), Rc::clone(&repeater), app.as_weak());
    sheet.on_step_pressed(move |name, up| {
        let (Some(app), Some(key)) = (shown.upgrade(), key_of(&name)) else {
            return;
        };
        let mode = app.get_mode();
        let (d, window, stop) = (d.clone(), shown.clone(), Rc::downgrade(&held));
        held.press(move || {
            let Some(app) = window.upgrade() else {
                return;
            };
            if !app.get_sheet_open() {
                if let Some(repeater) = stop.upgrade() {
                    repeater.release();
                }
                return;
            }
            d.step(&app, mode, key, up);
        });
    });
    sheet.on_step_released(move || repeater.release());

    let d = drafts.clone();
    let (shown, on_step) = (app.as_weak(), move |app: &App, key: &'static str, up| {
        d.step(app, app.get_mode(), key, up);
    });
    sheet.on_step_key(move |name, up| {
        if let (Some(app), Some(key)) = (shown.upgrade(), key_of(&name)) {
            on_step(&app, key, up);
        }
    });

    // One folder dialog at a time, for Browse… and Open folder: it has no parent window, so it
    // does not block a second click.
    let picking = Arc::new(AtomicBool::new(false));
    let (d, folders) = (drafts.clone(), Arc::clone(&picking));
    sheet.on_browse(on_key(app.as_weak(), move |app: &App, mode, key| {
        browse(&d, &folders, app, mode, key);
    }));
    // Open folder (the Library, the empty list's card) is Browse… on the input folder.
    let (d, shown) = (drafts.clone(), app.as_weak());
    app.on_open_folder(move || {
        if let Some(app) = shown.upgrade() {
            browse(&d, &picking, &app, app.get_mode(), "input_dir");
        }
    });

    // A Preset Profile: remux's, unless a batch locks the form.
    let shown = app.as_weak();
    sheet.on_pick_preset(move |id| {
        if shown.upgrade().is_some_and(|app| !app.get_busy()) {
            let intent = Intent::SetPreset {
                mode: pipeline(Mode::Remux),
                preset: id.into(),
            };
            drafts.send.send("set_preset", intent, || {});
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::tests::{Episodes, NoJobs, NoSamples};
    use crate::publisher::{install_models, SlintPublisher};
    use i_slint_backend_testing::mock_elapsed_time;
    use mkv_core::pipeline::PipelineMode;
    use mkv_core::preset::get_preset_profiles;
    use mkv_desktop_core::actor::{Core, Ports};
    use std::cell::Cell;

    fn new_app() -> App {
        let app = App::new().unwrap();
        install_models(&app);
        app
    }

    fn type_into(app: &App, key: &str, value: &str) {
        let mut typed = app.get_remux_form();
        *text_field(&mut typed, key).unwrap() = value.into();
        app.set_remux_form(typed);
    }

    /// A publication writes each field but the one with the focus and those with an edit on its
    /// way; the sheet closed, no field has the focus.
    #[test]
    fn a_publication_writes_every_field_but_the_focused_and_the_pending_ones() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        let edits = publisher.edits();
        let mut state = ViewState::new(None);
        state.remux.draft.workers = "4".into();
        state.remux.draft.out_dir = "/out".into();
        state.extract.draft.workers = "2".into();
        state.remux.draft.invalid = Some("workers");
        publisher.show(&state);
        let typed = app.get_remux_form();
        assert_eq!(
            (typed.workers.as_str(), typed.out_dir.as_str()),
            ("4", "/out")
        );
        assert_eq!(app.get_extract_form().workers, "2");
        assert_eq!(app.get_remux_invalid(), "workers");
        assert_eq!(app.get_extract_invalid(), "");

        // The remux sheet open with the cursor in Workers, and an output folder edit pending.
        app.set_mode(Mode::Remux);
        app.set_sheet_open(true);
        app.global::<Sheet>().set_focused("workers".into());
        lock(&edits).begin(Mode::Remux, "out_dir");
        type_into(&app, "workers", "7");
        type_into(&app, "out_dir", "/typed");
        state.seq = 1;
        state.remux.draft.workers = "5".into();
        state.remux.draft.out_dir = "/out2".into();
        state.remux.draft.fonts_dir = "/fonts".into();
        state.extract.draft.workers = "3".into();
        publisher.show(&state);
        let typed = app.get_remux_form();
        assert_eq!(typed.workers, "7", "the focused field keeps what is typed");
        assert_eq!(typed.out_dir, "/typed", "a pending key keeps what is typed");
        assert_eq!(typed.fonts_dir, "/fonts");
        assert_eq!(
            app.get_remux_draft().workers,
            "5",
            "the published draft is kept"
        );
        assert_eq!(
            app.get_extract_form().workers,
            "3",
            "extract's Workers has no focus"
        );

        // Closed, the sheet holds no focus; answered, the key is no longer pending.
        app.set_sheet_open(false);
        assert!(lock(&edits).end(Mode::Remux, "out_dir"));
        state.seq = 2;
        publisher.show(&state);
        let typed = app.get_remux_form();
        assert_eq!(
            (typed.workers.as_str(), typed.out_dir.as_str()),
            ("5", "/out2")
        );
    }

    /// The last answer of a key decides: refused, the published value comes back even into the
    /// focused field; a no-op, only the complexity's, and only while unfocused.
    #[test]
    fn an_answer_restores_the_published_value_by_the_rules() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        let edits = publisher.edits();
        let mut state = ViewState::new(None);
        state.remux.draft.workers = "4".into();
        publisher.show(&state);
        app.set_mode(Mode::Remux);
        app.set_sheet_open(true);
        let sheet = app.global::<Sheet>();
        sheet.set_focused("complexity".into());
        type_into(&app, "complexity", "15");
        type_into(&app, "workers", "x");
        let complexity = || app.get_remux_form().complexity;

        for _ in 0..2 {
            lock(&edits).begin(Mode::Remux, "complexity");
        }
        settle(&app, &edits, Mode::Remux, "complexity", Ok(false));
        assert_eq!(complexity(), "15", "a later edit is still pending");
        settle(&app, &edits, Mode::Remux, "complexity", Ok(false));
        assert_eq!(complexity(), "15", "the focused field is spared");
        sheet.set_focused("".into());
        lock(&edits).begin(Mode::Remux, "complexity");
        settle(&app, &edits, Mode::Remux, "complexity", Ok(false));
        assert_eq!(
            complexity(),
            "5",
            "a no-op complexity gets the published value"
        );

        sheet.set_focused("workers".into());
        lock(&edits).begin(Mode::Remux, "workers");
        settle(&app, &edits, Mode::Remux, "workers", Ok(false));
        assert_eq!(
            app.get_remux_form().workers,
            "x",
            "a no-op of another key restores nothing"
        );
        lock(&edits).begin(Mode::Remux, "workers");
        settle(&app, &edits, Mode::Remux, "workers", Err("locked".into()));
        assert_eq!(
            app.get_remux_form().workers,
            "4",
            "a refusal restores the focused field too"
        );
    }

    #[test]
    fn a_held_stepper_repeats_after_400_ms_then_every_80_ms_until_released() {
        i_slint_backend_testing::init_no_event_loop();
        let steps = Rc::new(Cell::new(0));
        let repeater = Rc::new(Repeater::default());
        let press = || {
            let counter = Rc::clone(&steps);
            repeater.press(move || counter.set(counter.get() + 1));
        };
        let after = |ms| mock_elapsed_time(Duration::from_millis(ms));

        press();
        assert_eq!(steps.get(), 1, "a press steps at once");
        after(399);
        assert_eq!(steps.get(), 1);
        after(1);
        assert_eq!(steps.get(), 1, "the repeat starts at 400 ms");
        after(79);
        assert_eq!(steps.get(), 1);
        after(1);
        assert_eq!(steps.get(), 2, "then steps at 480 ms");
        for expected in 3..6 {
            after(80);
            assert_eq!(steps.get(), expected);
        }
        repeater.release();
        after(1000);
        assert_eq!(steps.get(), 5, "released, it stops");

        // A press released before 400 ms steps once.
        press();
        after(300);
        repeater.release();
        after(1000);
        assert_eq!(steps.get(), 6);
    }

    /// The sheet's callbacks send the fields as typed, in call order; a change of the input folder
    /// rescans it, the complexity left gets the published value back, a preset is remux's, and
    /// nothing steps while a batch runs.
    #[test]
    fn the_fields_send_their_edits_as_typed_and_in_call_order() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        let ports = Ports {
            inspector: Episodes,
            launcher: NoJobs,
            sampler: NoSamples,
        };
        let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
        crate::intents::wire(
            &app,
            core.clone(),
            runtime.handle().clone(),
            publisher.clone(),
        );
        let sheet = app.global::<Sheet>();
        let state = || runtime.block_on(core.state()).unwrap();
        // A draft Intent is published with the next coalesced publication (66 ms at most): the
        // remux draft once `settled` holds of it.
        let draft = |settled: &dyn Fn(&SettingsDraft) -> bool| {
            for _ in 0..200 {
                let draft = state().remux.draft;
                if settled(&draft) {
                    return draft;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            state().remux.draft
        };
        app.set_mode(Mode::Remux);
        let typed = |key: &str, value: &str| {
            type_into(&app, key, value);
            sheet.invoke_edited(key.into());
        };

        typed("input_dir", " /show ");
        typed("workers", "1");
        typed("workers", "12");
        sheet.invoke_step_key("workers".into(), true);
        typed("sub_offset", "1");
        sheet.invoke_step_key("sub_offset".into(), false);
        let sent = draft(&|draft| draft.sub_offset.as_deref() == Some("0.5"));
        assert_eq!(sent.input_dir, " /show ", "as typed, untrimmed");
        assert_eq!(sent.workers, "13", "1, then 12, then a step up");
        assert_eq!(sent.sub_offset.as_deref(), Some("0.5"));

        let mut form = app.get_remux_form();
        form.dry_run = true;
        form.bitrate = 4;
        app.set_remux_form(form);
        sheet.invoke_edited("dry_run".into());
        sheet.invoke_edited("bitrate".into());
        let sent = draft(&|draft| draft.bitrate.as_deref() == Some("320k"));
        assert!(sent.dry_run);
        assert_eq!(sent.bitrate.as_deref(), Some("320k"));
        assert_eq!(sent.preset.as_deref(), Some("custom"));

        sheet.invoke_pick_preset("hifi".into());
        let sent = draft(&|draft| draft.preset.as_deref() == Some("hifi"));
        assert_eq!(sent.preset.as_deref(), Some("hifi"));
        assert_eq!(sent.complexity.as_deref(), Some("10"));

        // The slider sends its whole value, and the box follows it at once.
        sheet.invoke_slid(3.4);
        assert_eq!(app.get_remux_form().complexity, "3");
        let sent = draft(&|draft| draft.complexity.as_deref() == Some("3"));
        assert_eq!(sent.complexity.as_deref(), Some("3"));

        // Left, the complexity box gets the published value back.
        publisher.show(&state());
        app.set_sheet_open(true);
        sheet.invoke_enter("complexity".into());
        type_into(&app, "complexity", "15");
        sheet.invoke_leave("complexity".into());
        assert_eq!(app.get_remux_form().complexity, "3");
        assert_eq!(sheet.get_focused(), "");

        // The input folder changed: the edit again, then a scan of the trimmed path.
        sheet.invoke_enter("input_dir".into());
        sheet.set_changed_since(true);
        sheet.invoke_accept("input_dir".into());
        let mut scanned = false;
        for _ in 0..200 {
            if state().remux.directory.as_deref() == Some("/show") {
                scanned = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(scanned, "the input folder was scanned");

        // Locked while a batch runs: no step is sent.
        app.set_busy(true);
        sheet.invoke_step_key("workers".into(), true);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(state().remux.draft.workers, "13");
    }

    /// After a refused Run the sheet opens on the flagged field, or moves the cursor there when
    /// it is open; nothing opens with no field flagged.
    #[test]
    fn a_refused_run_opens_the_sheet_on_the_flagged_field() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
        app.show().unwrap();
        let mut state = ViewState::new(None);
        publisher.show(&state);
        assert!(!open_on_flagged(&app, Mode::Extract));
        assert!(!app.get_sheet_open());

        state.seq = 1;
        state.extract.draft.invalid = Some("workers");
        publisher.show(&state);
        assert!(open_on_flagged(&app, Mode::Extract));
        // A tick makes the sheet, whose field takes the focus; the next runs its focus handler.
        mock_elapsed_time(Duration::from_millis(1));
        mock_elapsed_time(Duration::from_millis(1));
        assert!(app.get_sheet_open());
        assert_eq!(app.get_mode(), Mode::Extract);
        let sheet = app.global::<Sheet>();
        assert_eq!(
            sheet.get_focused(),
            "workers",
            "the cursor is in the flagged field"
        );

        state.seq = 2;
        state.extract.draft.invalid = Some("input_dir");
        publisher.show(&state);
        assert!(open_on_flagged(&app, Mode::Extract));
        // A tick makes the sheet, whose field takes the focus; the next runs its focus handler.
        mock_elapsed_time(Duration::from_millis(1));
        mock_elapsed_time(Duration::from_millis(1));
        assert_eq!(
            sheet.get_focused(),
            "input_dir",
            "an open sheet moves the cursor"
        );
        app.hide().unwrap();
    }

    #[test]
    fn a_draft_maps_into_the_fields_and_each_key_back_into_its_edit() {
        let mut draft = SettingsDraft::new(PipelineMode::Remux);
        draft.input_dir = " /show ".into();
        draft.workers = "4".into();
        draft.dry_run = true;
        let form = map_draft(&draft);
        assert_eq!(form.input_dir, " /show ");
        assert_eq!(form.bitrate, 2, "192k, the default preset's");
        assert_eq!(form.complexity, "5");
        assert_eq!(form.sub_offset, "0");
        assert!(form.dry_run);
        // Extract lacks the remux fields: they stay empty.
        let extract = map_draft(&SettingsDraft::new(PipelineMode::Extract));
        assert_eq!((extract.bitrate, extract.complexity.as_str()), (-1, ""));

        // Sent as typed, untrimmed; the select sends its entry's value.
        assert_eq!(
            changes(&form, "input_dir").input_dir.as_deref(),
            Some(" /show ")
        );
        assert_eq!(changes(&form, "bitrate").bitrate.as_deref(), Some("192k"));
        assert_eq!(changes(&form, "dry_run").dry_run, Some(true));
        let none = DraftForm {
            bitrate: -1,
            ..form.clone()
        };
        assert_eq!(changes(&none, "bitrate").bitrate.as_deref(), Some(""));
        // Each key sets its own field alone.
        for key in KEYS {
            let set = set_fields(&changes(&form, key));
            assert_eq!(set, [key], "{key}");
        }
    }

    /// The `DraftChanges` fields that are set, by name.
    fn set_fields(changes: &DraftChanges) -> Vec<&'static str> {
        let DraftChanges {
            input_dir,
            out_dir,
            fonts_dir,
            assets_dir,
            bitrate,
            complexity,
            sub_offset,
            workers,
            io_jobs,
            ffmpeg_threads,
            dry_run,
            no_cache,
        } = changes;
        let texts = [
            ("input_dir", input_dir),
            ("out_dir", out_dir),
            ("fonts_dir", fonts_dir),
            ("assets_dir", assets_dir),
            ("bitrate", bitrate),
            ("complexity", complexity),
            ("sub_offset", sub_offset),
            ("workers", workers),
            ("io_jobs", io_jobs),
            ("ffmpeg_threads", ffmpeg_threads),
        ];
        let mut set: Vec<_> = texts
            .into_iter()
            .filter(|(_, value)| value.is_some())
            .map(|(key, _)| key)
            .collect();
        set.extend(dry_run.map(|_| "dry_run"));
        set.extend(no_cache.map(|_| "no_cache"));
        set
    }

    #[test]
    fn the_chips_show_the_hints_once_known_else_auto() {
        let chips = Chips {
            workers: "Standby (4 Workers)".into(),
            io_jobs: "Idle".into(),
            workers_hint: Some("auto (4)".into()),
            io_jobs_hint: None,
        };
        let texts = map_chips(&chips);
        assert_eq!(texts.workers, "Standby (4 Workers)");
        assert_eq!(texts.io_jobs, "Idle");
        assert_eq!(texts.workers_hint, "auto (4)");
        assert_eq!(texts.io_jobs_hint, "auto");
    }

    #[test]
    fn the_presets_mark_the_drafts_and_the_custom_row_shows_its_encode() {
        let mut draft = SettingsDraft::new(PipelineMode::Remux);
        let rows = map_presets(&get_preset_profiles(), &draft);
        let balanced = rows.iter().find(|row| row.id == "balanced").unwrap();
        assert!(balanced.active);
        assert_eq!(balanced.sub, "Opus 192k · complexity 5");
        assert_eq!(balanced.a11y, "Balanced, Opus 192k · complexity 5");
        assert_eq!(rows.iter().filter(|row| row.active).count(), 1);

        draft.bitrate = Some("320k".into());
        draft.complexity = Some("9".into());
        draft.preset = Some("custom".into());
        let rows = map_presets(&get_preset_profiles(), &draft);
        let custom = rows.iter().find(|row| row.id == "custom").unwrap();
        assert!(custom.active);
        assert_eq!(custom.sub, "Opus 320k · complexity 9");
        // The other rows keep their profile's encode.
        let balanced = rows.iter().find(|row| row.id == "balanced").unwrap();
        assert_eq!(balanced.sub, "Opus 192k · complexity 5");
    }

    #[test]
    fn pending_edits_count_per_mode_and_key() {
        let mut pending = PendingEdits::default();
        pending.begin(Mode::Remux, "workers");
        pending.begin(Mode::Remux, "workers");
        assert!(pending.has(Mode::Remux, "workers"));
        assert!(!pending.has(Mode::Extract, "workers"));
        assert!(!pending.end(Mode::Remux, "workers"), "one is left");
        assert!(pending.end(Mode::Remux, "workers"));
        assert!(!pending.has(Mode::Remux, "workers"));
    }
}
