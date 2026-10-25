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
use mkv_anime_batch_slint_ui::{App, ChipTexts, DraftForm, Mode, PresetItem, Sheet, SheetLimits};
use mkv_core::preset::PresetProfile;
use mkv_core::request::{COMPLEXITY_RANGE, FFMPEG_THREADS_RANGE, IO_JOBS_RANGE, WORKERS_RANGE};
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

/// The bitrate select's entries (index.html `#remux-bitrate`); the form holds the index. An accepted
/// copy, as the webview's: the core publishes no list of them.
pub(crate) const BITRATES: [&str; 5] = ["128k", "160k", "192k", "256k", "320k"];

/// A draft key: a field of the sheet, as `DraftChanges` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DraftKey {
    InputDir,
    OutDir,
    FontsDir,
    AssetsDir,
    Bitrate,
    Complexity,
    SubOffset,
    Workers,
    IoJobs,
    FfmpegThreads,
    DryRun,
    NoCache,
}

impl DraftKey {
    const ALL: [Self; 12] = [
        Self::InputDir,
        Self::OutDir,
        Self::FontsDir,
        Self::AssetsDir,
        Self::Bitrate,
        Self::Complexity,
        Self::SubOffset,
        Self::Workers,
        Self::IoJobs,
        Self::FfmpegThreads,
        Self::DryRun,
        Self::NoCache,
    ];

    /// The name the sheet's fields (settings.slint) give the key.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InputDir => "input_dir",
            Self::OutDir => "out_dir",
            Self::FontsDir => "fonts_dir",
            Self::AssetsDir => "assets_dir",
            Self::Bitrate => "bitrate",
            Self::Complexity => "complexity",
            Self::SubOffset => "sub_offset",
            Self::Workers => "workers",
            Self::IoJobs => "io_jobs",
            Self::FfmpegThreads => "ffmpeg_threads",
            Self::DryRun => "dry_run",
            Self::NoCache => "no_cache",
        }
    }

    /// The key a field of the sheet names, if it names one. The complexity slider has a focus id of
    /// its own (`complexity_slider`), so a focus moving between it and the box is seen leaving one
    /// and entering the other, but it edits the complexity.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        if name == "complexity_slider" {
            return Some(Self::Complexity);
        }
        Self::ALL.into_iter().find(|key| key.as_str() == name)
    }
}

/// The key the core normalizes on edit (draft.rs `clamp_complexity`): a field left, or whose edit
/// the core answers as a no-op, gets its published value back (settings-draft.js `NORMALIZED`).
const NORMALIZED: DraftKey = DraftKey::Complexity;

/// A stepper held down steps again after this, then every [`REPEAT_EVERY`] (app.js
/// `initSteppers`).
pub(crate) const REPEAT_DELAY: Duration = Duration::from_millis(400);
pub(crate) const REPEAT_EVERY: Duration = Duration::from_millis(80);

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
fn text_field(form: &mut DraftForm, key: DraftKey) -> Option<&mut SharedString> {
    use DraftKey::*;
    Some(match key {
        InputDir => &mut form.input_dir,
        OutDir => &mut form.out_dir,
        FontsDir => &mut form.fonts_dir,
        AssetsDir => &mut form.assets_dir,
        Complexity => &mut form.complexity,
        SubOffset => &mut form.sub_offset,
        Workers => &mut form.workers,
        IoJobs => &mut form.io_jobs,
        FfmpegThreads => &mut form.ffmpeg_threads,
        Bitrate | DryRun | NoCache => return None,
    })
}

/// Whether `key`'s field differs between `a` and `b`.
fn differs(a: &DraftForm, b: &DraftForm, key: DraftKey) -> bool {
    use DraftKey::*;
    match key {
        InputDir => a.input_dir != b.input_dir,
        OutDir => a.out_dir != b.out_dir,
        FontsDir => a.fonts_dir != b.fonts_dir,
        AssetsDir => a.assets_dir != b.assets_dir,
        Bitrate => a.bitrate != b.bitrate,
        Complexity => a.complexity != b.complexity,
        SubOffset => a.sub_offset != b.sub_offset,
        Workers => a.workers != b.workers,
        IoJobs => a.io_jobs != b.io_jobs,
        FfmpegThreads => a.ffmpeg_threads != b.ffmpeg_threads,
        DryRun => a.dry_run != b.dry_run,
        NoCache => a.no_cache != b.no_cache,
    }
}

/// Copies `key`'s field from `from` into `to`; true when that changed it.
fn copy_key(to: &mut DraftForm, from: &DraftForm, key: DraftKey) -> bool {
    use DraftKey::*;
    if !differs(to, from, key) {
        return false;
    }
    match key {
        Bitrate => to.bitrate = from.bitrate,
        DryRun => to.dry_run = from.dry_run,
        NoCache => to.no_cache = from.no_cache,
        InputDir => to.input_dir.clone_from(&from.input_dir),
        OutDir => to.out_dir.clone_from(&from.out_dir),
        FontsDir => to.fonts_dir.clone_from(&from.fonts_dir),
        AssetsDir => to.assets_dir.clone_from(&from.assets_dir),
        Complexity => to.complexity.clone_from(&from.complexity),
        SubOffset => to.sub_offset.clone_from(&from.sub_offset),
        Workers => to.workers.clone_from(&from.workers),
        IoJobs => to.io_jobs.clone_from(&from.io_jobs),
        FfmpegThreads => to.ffmpeg_threads.clone_from(&from.ffmpeg_threads),
    }
    true
}

/// The `EditDraft` changes that send `key` as `form` holds it.
pub(crate) fn changes(form: &DraftForm, key: DraftKey) -> DraftChanges {
    use DraftKey::*;
    let mut changes = DraftChanges::default();
    let text = |value: &SharedString| Some(value.to_string());
    match key {
        InputDir => changes.input_dir = text(&form.input_dir),
        OutDir => changes.out_dir = text(&form.out_dir),
        FontsDir => changes.fonts_dir = text(&form.fonts_dir),
        AssetsDir => changes.assets_dir = text(&form.assets_dir),
        Bitrate => {
            // A select with no entry chosen holds the empty value, as a webview `<select>` does.
            let at = usize::try_from(form.bitrate).ok();
            let value = at.and_then(|at| BITRATES.get(at)).copied().unwrap_or("");
            changes.bitrate = Some(value.into());
        }
        Complexity => changes.complexity = text(&form.complexity),
        SubOffset => changes.sub_offset = text(&form.sub_offset),
        Workers => changes.workers = text(&form.workers),
        IoJobs => changes.io_jobs = text(&form.io_jobs),
        FfmpegThreads => changes.ffmpeg_threads = text(&form.ffmpeg_threads),
        DryRun => changes.dry_run = Some(form.dry_run),
        NoCache => changes.no_cache = Some(form.no_cache),
    }
    changes
}

/// The draft field a stepper of `key` steps.
fn step_field(key: DraftKey) -> Option<StepField> {
    use DraftKey::*;
    Some(match key {
        SubOffset => StepField::SubOffset,
        Workers => StepField::Workers,
        IoJobs => StepField::IoJobs,
        FfmpegThreads => StepField::FfmpegThreads,
        InputDir | OutDir | FontsDir | AssetsDir | Bitrate | Complexity | DryRun | NoCache => {
            return None
        }
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

/// The sheet's limits: the complexity slider's range and the number fields' tooltips, from the
/// core's ranges (mkv-core `request.rs`).
pub(crate) fn limits() -> SheetLimits {
    let range =
        |range: &std::ops::RangeInclusive<usize>| format!("{}–{}", range.start(), range.end());
    let complexity = |at: u32| i32::try_from(at).unwrap_or(i32::MAX);
    SheetLimits {
        complexity_min: complexity(*COMPLEXITY_RANGE.start()),
        complexity_max: complexity(*COMPLEXITY_RANGE.end()),
        workers_tip: format!(
            "Episodes processed at once ({}; empty is auto)",
            range(&WORKERS_RANGE)
        )
        .into(),
        io_jobs_tip: format!(
            "Concurrent disk I/O streams ({}; 0 is the same as Workers)",
            range(&IO_JOBS_RANGE)
        )
        .into(),
        ffmpeg_threads_tip: format!(
            "FLAC decoder threads per transcode ({}; empty is auto). Opus encoding is \
             single-threaded",
            range(&FFMPEG_THREADS_RANGE)
        )
        .into(),
    }
}

/// A Preset Profile row's subtitle: the encode it sets (preset-menu.js `presetSubtitle`).
fn preset_sub(bitrate: &str, complexity: impl std::fmt::Display) -> String {
    format!("Opus {bitrate} · complexity {complexity}")
}

/// The Preset Profile rows over the remux `draft`: its preset marked active, and the custom row
/// showing the draft's own encode while its complexity reads as a number, else keeping `shown`,
/// the subtitle it had (preset-menu.js).
pub(crate) fn map_presets(
    presets: &[PresetProfile],
    draft: &SettingsDraft,
    shown: Option<SharedString>,
) -> Vec<PresetItem> {
    let own = match (&draft.bitrate, &draft.complexity) {
        (Some(bitrate), Some(complexity)) => complexity
            .trim()
            .parse::<i64>()
            .ok()
            .map(|complexity| preset_sub(bitrate, complexity)),
        _ => None,
    };
    let own = own.or_else(|| shown.map(String::from));
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

/// A key's edits on their way to the core.
#[derive(Debug)]
struct Pending {
    count: u32,
    /// The published form as the first of them was sent.
    before: DraftForm,
    /// A publication passed the key over meanwhile, holding a value other than `before`'s: the
    /// core applied an edit, and no later publication may come to show it.
    moved: bool,
}

/// Draft edits sent and not answered yet, by mode and key (settings-draft.js `pending`).
#[derive(Debug, Default)]
pub(crate) struct PendingEdits([HashMap<DraftKey, Pending>; 2]);

impl PendingEdits {
    /// An edit of `key` sent while `published` was the mode's published form.
    fn begin(&mut self, mode: Mode, key: DraftKey, published: &DraftForm) {
        self.0[index(mode)]
            .entry(key)
            .or_insert_with(|| Pending {
                count: 0,
                before: published.clone(),
                moved: false,
            })
            .count += 1;
    }

    /// One edit of `key` was answered. Once none is left: `Some(moved)`, whether a publication
    /// passed over a value the core moved the key to meanwhile; `None` for a key none is pending of.
    fn end(&mut self, mode: Mode, key: DraftKey) -> Option<bool> {
        let pending = &mut self.0[index(mode)];
        let entry = pending.get_mut(&key)?;
        if entry.count > 1 {
            entry.count -= 1;
            return None;
        }
        pending.remove(&key).map(|entry| entry.moved)
    }

    /// A publication of `draft` passes over `key` if it is pending; true when it is.
    fn pass_over(&mut self, mode: Mode, key: DraftKey, draft: &DraftForm) -> bool {
        let Some(entry) = self.0[index(mode)].get_mut(&key) else {
            return false;
        };
        entry.moved |= differs(&entry.before, draft, key);
        true
    }

    fn has(&self, mode: Mode, key: DraftKey) -> bool {
        self.0[index(mode)].contains_key(&key)
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
fn has_focus(app: &App, mode: Mode, key: DraftKey) -> bool {
    app.get_sheet_open()
        && app.get_mode() == mode
        && DraftKey::parse(&app.global::<Sheet>().get_focused()) == Some(key)
}

/// Writes the published `key` of `mode` back into its field, unless it has the focus or an edit
/// of it is still on its way (any, with `force`).
pub(crate) fn restore(app: &App, edits: &Edits, mode: Mode, key: DraftKey, force: bool) {
    if !force && (has_focus(app, mode, key) || lock(edits).has(mode, key)) {
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
            let mut pending = lock(edits);
            for key in DraftKey::ALL {
                if !has_focus(app, mode, key) && !pending.pass_over(mode, key, &draft) {
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
    let shown = app
        .get_presets()
        .iter()
        .find(|row| row.id == "custom")
        .map(|row| row.sub);
    let presets = map_presets(&state.presets, &state.remux.draft, shown);
    if !app.get_presets().iter().eq(presets.iter().cloned()) {
        app.set_presets(ModelRc::new(VecModel::from(presets)));
    }
}

/// An edit of `mode`'s `key` was answered: once none of the key is left pending, a refused one puts
/// the published value back (into the focused field too), a no-op one does for the complexity
/// unless it has the focus (settings-draft.js `guarded`), and so does any once a publication passed
/// over the value the core moved the key to, which no later publication may show.
fn settle(app: &App, edits: &Edits, mode: Mode, key: DraftKey, answer: Result<bool, String>) {
    let Some(moved) = lock(edits).end(mode, key) else {
        return;
    };
    match answer {
        Err(_) => restore(app, edits, mode, key, true),
        Ok(false) if key == NORMALIZED => restore(app, edits, mode, key, false),
        Ok(_) if moved => restore(app, edits, mode, key, false),
        Ok(_) => {}
    }
}

/// After a refused Run of `mode`: opens the sheet on the field its draft flags, so the error
/// shows (job-controller.js `start`). False when none is flagged.
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
    fn guarded(&self, app: &App, mode: Mode, key: DraftKey, name: &'static str, intent: Intent) {
        lock(&self.edits).begin(mode, key, &published(app, mode));
        let (window, edits) = (self.send.window.clone(), Arc::clone(&self.edits));
        self.send.send_answered(name, intent, move |answer| {
            let _ =
                window.upgrade_in_event_loop(move |app| settle(&app, &edits, mode, key, answer));
        });
    }

    /// Sends `key` as `mode`'s field holds it.
    fn edit(&self, app: &App, mode: Mode, key: DraftKey) {
        let changes = changes(&form(app, mode), key);
        let intent = Intent::EditDraft {
            mode: pipeline(mode),
            changes,
        };
        self.guarded(app, mode, key, "edit_draft", intent);
    }

    /// Steps `mode`'s `key` once; nothing while a batch locks the fields.
    fn step(&self, app: &App, mode: Mode, key: DraftKey, up: bool) {
        let Some(field) = step_field(key).filter(|_| !app.get_locked()) else {
            return;
        };
        let direction = if up { Direction::Up } else { Direction::Down };
        let intent = Intent::StepDraft {
            mode: pipeline(mode),
            field,
            direction,
        };
        self.guarded(app, mode, key, "step_draft", intent);
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
    /// (app.js `chooseInputDirectory`). Nothing once a batch started meanwhile, nor for a key that
    /// is no folder.
    fn folder_picked(&self, app: &App, mode: Mode, key: DraftKey, picked: Option<String>) {
        let folder = matches!(
            key,
            DraftKey::InputDir | DraftKey::OutDir | DraftKey::FontsDir | DraftKey::AssetsDir
        );
        let Some(path) = picked.filter(|_| folder && !app.get_locked()) else {
            return;
        };
        let mut typed = form(app, mode);
        if let Some(field) = text_field(&mut typed, key) {
            *field = path.as_str().into();
        }
        set_form(app, mode, typed);
        self.edit(app, mode, key);
        if key == DraftKey::InputDir {
            self.scan(mode, path);
        }
    }
}

/// Opens the folder dialog for `mode`'s `key`, one dialog at a time and not while a batch runs.
fn browse(drafts: &Drafts, picking: &Arc<AtomicBool>, app: &App, mode: Mode, key: DraftKey) {
    if app.get_locked() || picking.swap(true, Ordering::Relaxed) {
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
    act: impl Fn(&App, Mode, DraftKey) + 'static,
) -> impl Fn(SharedString) + 'static {
    move |name| {
        if let (Some(app), Some(key)) = (window.upgrade(), DraftKey::parse(&name)) {
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
        if key == DraftKey::InputDir {
            d.rescan(app, mode);
        }
    }));

    // Left, the complexity gets the published value back, and an input folder rescans unless
    // its change (`changed`) just did.
    let (d, shown) = (drafts.clone(), app.as_weak());
    sheet.on_left(move |name, changed| {
        let (Some(app), Some(key)) = (shown.upgrade(), DraftKey::parse(&name)) else {
            return;
        };
        let mode = app.get_mode();
        match key {
            NORMALIZED => restore(&app, &d.edits, mode, key, false),
            DraftKey::InputDir if !changed => d.rescan(&app, mode),
            _ => {}
        }
    });

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

    // A held stepper repeats until released; one of a sheet that closed under the pointer, or
    // locked by a batch that started, stops, as its release may never arrive.
    let repeater = Rc::new(Repeater::default());
    let (d, held, shown) = (drafts.clone(), Rc::clone(&repeater), app.as_weak());
    sheet.on_step_pressed(move |name, up| {
        let (Some(app), Some(key)) = (shown.upgrade(), DraftKey::parse(&name)) else {
            return;
        };
        let mode = app.get_mode();
        let (d, window, stop) = (d.clone(), shown.clone(), Rc::downgrade(&held));
        held.press(move || {
            let Some(app) = window.upgrade() else {
                return;
            };
            if !app.get_sheet_open() || app.get_locked() {
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
    let (shown, on_step) = (app.as_weak(), move |app: &App, key: DraftKey, up| {
        d.step(app, app.get_mode(), key, up);
    });
    sheet.on_step_key(move |name, up| {
        if let (Some(app), Some(key)) = (shown.upgrade(), DraftKey::parse(&name)) {
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
            browse(&d, &picking, &app, app.get_mode(), DraftKey::InputDir);
        }
    });

    // A Preset Profile: remux's, unless a batch locks the form.
    let shown = app.as_weak();
    sheet.on_pick_preset(move |id| {
        if shown.upgrade().is_some_and(|app| !app.get_locked()) {
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
    use mkv_desktop_core::actor::{Core, CoreHandle, Ports};
    use std::cell::Cell;
    use DraftKey::*;

    fn new_app() -> App {
        let app = App::new().unwrap();
        install_models(&app);
        app
    }

    fn type_into(app: &App, key: DraftKey, value: &str) {
        let mut typed = app.get_remux_form();
        *text_field(&mut typed, key).unwrap() = value.into();
        app.set_remux_form(typed);
    }

    /// An edit of remux's `key` sent, as `Drafts::guarded` marks it.
    fn begin(app: &App, edits: &Edits, key: DraftKey) {
        lock(edits).begin(Mode::Remux, key, &app.get_remux_draft());
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
        begin(&app, &edits, OutDir);
        type_into(&app, Workers, "7");
        type_into(&app, OutDir, "/typed");
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
        assert!(lock(&edits).end(Mode::Remux, OutDir).is_some());
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
        type_into(&app, Complexity, "15");
        type_into(&app, Workers, "x");
        let complexity = || app.get_remux_form().complexity;

        for _ in 0..2 {
            begin(&app, &edits, Complexity);
        }
        settle(&app, &edits, Mode::Remux, Complexity, Ok(false));
        assert_eq!(complexity(), "15", "a later edit is still pending");
        settle(&app, &edits, Mode::Remux, Complexity, Ok(false));
        assert_eq!(complexity(), "15", "the focused field is spared");
        sheet.set_focused("".into());
        begin(&app, &edits, Complexity);
        settle(&app, &edits, Mode::Remux, Complexity, Ok(false));
        assert_eq!(
            complexity(),
            "5",
            "a no-op complexity gets the published value"
        );

        sheet.set_focused("workers".into());
        begin(&app, &edits, Workers);
        settle(&app, &edits, Mode::Remux, Workers, Ok(false));
        assert_eq!(
            app.get_remux_form().workers,
            "x",
            "a no-op of another key restores nothing"
        );
        begin(&app, &edits, Workers);
        settle(&app, &edits, Mode::Remux, Workers, Err("locked".into()));
        assert_eq!(
            app.get_remux_form().workers,
            "4",
            "a refusal restores the focused field too"
        );
    }

    /// A publication that passed over a pending key holding the value the core moved it to is
    /// shown once the edit is answered, as no later one may come; one that held the value from
    /// before the edit is not, so a toggled checkbox never flips back.
    #[test]
    fn a_value_passed_over_while_pending_shows_once_answered() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        let edits = publisher.edits();
        let mut state = ViewState::new(None);
        state.remux.draft.workers = "4".into();
        publisher.show(&state);

        // A step up: the publication holding 5 lands before the answer.
        begin(&app, &edits, Workers);
        state.seq = 1;
        state.remux.draft.workers = "5".into();
        publisher.show(&state);
        assert_eq!(
            app.get_remux_form().workers,
            "4",
            "passed over while pending"
        );
        settle(&app, &edits, Mode::Remux, Workers, Ok(true));
        assert_eq!(app.get_remux_form().workers, "5");

        // Dry run toggled: a publication from before the edit lands, then the answer.
        let mut typed = app.get_remux_form();
        typed.dry_run = true;
        app.set_remux_form(typed);
        begin(&app, &edits, DryRun);
        state.seq = 2;
        publisher.show(&state);
        settle(&app, &edits, Mode::Remux, DryRun, Ok(true));
        assert!(
            app.get_remux_form().dry_run,
            "the old value is not put back"
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

    /// The core's View State once `settled` holds of it. A draft Intent is published with the
    /// next coalesced publication (66 ms at most), so it is asked again until then, for 2 s at
    /// most.
    fn state_once(
        runtime: &tokio::runtime::Runtime,
        core: &CoreHandle,
        settled: impl Fn(&ViewState) -> bool,
    ) -> ViewState {
        for _ in 0..200 {
            let state = runtime.block_on(core.state()).unwrap();
            if settled(&state) {
                return state;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the core never published the awaited state");
    }

    /// The sheet's callbacks send the fields as typed, in call order; a change of the input folder
    /// rescans it, the complexity left gets the published value back, a preset is remux's, a
    /// picked folder goes into the field that asked for it, and nothing steps while a batch runs.
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
        let draft = |settled: &dyn Fn(&SettingsDraft) -> bool| {
            state_once(&runtime, &core, |state| settled(&state.remux.draft))
                .remux
                .draft
        };
        app.set_mode(Mode::Remux);
        let typed = |key: DraftKey, value: &str| {
            type_into(&app, key, value);
            sheet.invoke_edited(key.as_str().into());
        };

        typed(InputDir, " /show ");
        typed(Workers, "1");
        typed(Workers, "12");
        sheet.invoke_step_key("workers".into(), true);
        typed(SubOffset, "1");
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

        // Left with nothing pending, the complexity box gets the published value back. Without
        // an event loop no answer comes back to settle an edit: the test clears them as answered.
        let answered = || *lock(&publisher.edits()) = PendingEdits::default();
        answered();
        // The core's publications reached the wired publisher, which could not hop with them: a
        // fresh one shows them.
        let show = |state: &ViewState| SlintPublisher::new(&app).show(state);
        show(&runtime.block_on(core.state()).unwrap());
        app.set_sheet_open(true);
        sheet.invoke_enter("complexity".into());
        type_into(&app, Complexity, "15");
        sheet.invoke_leave("complexity".into());
        assert_eq!(app.get_remux_form().complexity, "3");
        assert_eq!(sheet.get_focused(), "");
        // Left once edited, the edit it sends is pending: the box keeps what is typed until the
        // answer, then shows what the core made of it.
        sheet.invoke_enter("complexity".into());
        type_into(&app, Complexity, "15");
        sheet.set_changed_since(true);
        sheet.invoke_leave("complexity".into());
        assert_eq!(app.get_remux_form().complexity, "15", "kept while pending");
        let state = state_once(&runtime, &core, |state| {
            state.remux.draft.complexity.as_deref() == Some("10")
        });
        answered();
        show(&state);
        assert_eq!(app.get_remux_form().complexity, "10", "clamped by the core");

        // The input folder changed: the edit again, then a scan of the trimmed path.
        sheet.invoke_enter("input_dir".into());
        sheet.set_changed_since(true);
        sheet.invoke_accept("input_dir".into());
        state_once(&runtime, &core, |state| {
            state.remux.directory.as_deref() == Some("/show")
        });

        // Left unchanged, the input folder rescans what it holds, trimmed.
        type_into(&app, InputDir, " /other ");
        sheet.invoke_enter("input_dir".into());
        sheet.invoke_leave("input_dir".into());
        state_once(&runtime, &core, |state| {
            state.remux.directory.as_deref() == Some("/other")
        });

        // A picked folder goes into the field and the mode that asked for it, and an input
        // folder is scanned.
        let drafts = Drafts {
            send: Intents {
                core: core.clone(),
                runtime: runtime.handle().clone(),
                window: app.as_weak(),
            },
            edits: publisher.edits(),
        };
        drafts.folder_picked(&app, Mode::Extract, OutDir, Some("/picked".into()));
        assert_eq!(app.get_extract_form().out_dir, "/picked");
        assert_eq!(app.get_remux_form().out_dir, "", "remux's is left alone");
        drafts.folder_picked(&app, Mode::Extract, InputDir, Some("/extract".into()));
        drafts.folder_picked(&app, Mode::Extract, FontsDir, None);
        drafts.folder_picked(&app, Mode::Remux, Workers, Some("/no-folder".into()));
        assert_eq!(
            app.get_remux_form().workers,
            "13",
            "a key that is no folder"
        );
        let state = state_once(&runtime, &core, |state| {
            state.extract.directory.as_deref() == Some("/extract")
        });
        assert_eq!(state.extract.draft.out_dir, "/picked");
        assert_eq!(state.extract.draft.input_dir, "/extract");
        assert_eq!(
            state.extract.draft.fonts_dir, "",
            "a cancelled dialog sends nothing"
        );

        // Locked while a batch runs: no step is sent, nor a picked folder. The edit sent after
        // them shows when they would have.
        app.set_busy(true);
        sheet.invoke_step_key("workers".into(), true);
        drafts.folder_picked(&app, Mode::Remux, OutDir, Some("/late".into()));
        typed(FontsDir, "/barrier");
        let state = state_once(&runtime, &core, |state| {
            state.remux.draft.fonts_dir == "/barrier"
        });
        assert_eq!(state.remux.draft.workers, "13");
        assert_eq!(state.remux.draft.out_dir, "");
        assert_eq!(app.get_remux_form().out_dir, "");
    }

    /// After a refused Run the sheet opens on the flagged field, or moves the cursor there when
    /// it is open; nothing opens with no field flagged. The narrowest window scrolls the field
    /// into view.
    #[test]
    fn a_refused_run_opens_the_sheet_on_the_flagged_field() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let publisher = SlintPublisher::new(&app);
        app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
        app.show().unwrap();
        let mut state = ViewState::new(None);
        publisher.show(&state);
        assert!(!open_on_flagged(&app, Mode::Remux));
        assert!(!app.get_sheet_open());
        let ticks = || {
            // A tick makes the sheet, whose field takes the focus; the next runs its focus
            // handler, the last the sheet's scroll.
            for _ in 0..3 {
                mock_elapsed_time(Duration::from_millis(1));
            }
        };
        // The field labelled `label` lies within the sheet.
        let visible = |label: &str| {
            use i_slint_backend_testing::{AccessibleRole, ElementHandle};
            let panel = ElementHandle::find_by_accessible_label(&app, "Remux settings")
                .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
                .unwrap();
            let field = ElementHandle::find_by_accessible_label(&app, label)
                .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
                .unwrap();
            let (top, height) = (panel.absolute_position().y, panel.size().height);
            let (at, size) = (field.absolute_position().y, field.size().height);
            at >= top && at + size <= top + height
        };

        // Remux's FFmpeg threads, at the foot of its tallest sheet, flagged with extract shown.
        app.set_mode(Mode::Extract);
        state.seq = 1;
        state.remux.draft.invalid = Some("ffmpeg_threads");
        publisher.show(&state);
        assert!(open_on_flagged(&app, Mode::Remux));
        ticks();
        assert!(app.get_sheet_open());
        assert_eq!(app.get_mode(), Mode::Remux);
        let sheet = app.global::<Sheet>();
        assert_eq!(
            sheet.get_focused(),
            "ffmpeg_threads",
            "the cursor is in the flagged field"
        );
        assert!(
            visible("FFmpeg threads"),
            "the flagged field scrolled into view"
        );

        state.seq = 2;
        state.remux.draft.invalid = Some("input_dir");
        publisher.show(&state);
        assert!(open_on_flagged(&app, Mode::Remux));
        ticks();
        assert_eq!(
            sheet.get_focused(),
            "input_dir",
            "an open sheet moves the cursor"
        );
        assert!(visible("Input folder"), "and scrolls back up to it");
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
            changes(&form, InputDir).input_dir.as_deref(),
            Some(" /show ")
        );
        assert_eq!(changes(&form, Bitrate).bitrate.as_deref(), Some("192k"));
        assert_eq!(changes(&form, DryRun).dry_run, Some(true));
        let none = DraftForm {
            bitrate: -1,
            ..form.clone()
        };
        assert_eq!(changes(&none, Bitrate).bitrate.as_deref(), Some(""));
        // Each key sets its own field alone, under the name its field gives it, and copies only
        // its own field.
        for key in DraftKey::ALL {
            let set = set_fields(&changes(&form, key));
            assert_eq!(set, [key.as_str()], "{key:?}");
            assert_eq!(DraftKey::parse(key.as_str()), Some(key));
            let mut copied = DraftForm::default();
            assert!(
                copy_key(&mut copied, &form, key) || !differs(&DraftForm::default(), &form, key)
            );
            for other in DraftKey::ALL.into_iter().filter(|other| *other != key) {
                assert!(
                    !differs(&copied, &DraftForm::default(), other),
                    "{key:?} copied {other:?}"
                );
            }
        }
        assert_eq!(DraftKey::parse("complexity_slider"), Some(Complexity));
        assert_eq!(DraftKey::parse("nope"), None);
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
    fn the_limits_come_from_the_cores_ranges() {
        let limits = limits();
        assert_eq!((limits.complexity_min, limits.complexity_max), (0, 10));
        assert_eq!(
            limits.workers_tip,
            "Episodes processed at once (1–64; empty is auto)"
        );
        assert_eq!(
            limits.io_jobs_tip,
            "Concurrent disk I/O streams (0–64; 0 is the same as Workers)"
        );
        assert_eq!(
            limits.ffmpeg_threads_tip,
            "FLAC decoder threads per transcode (1–32; empty is auto). Opus encoding is \
             single-threaded"
        );
    }

    #[test]
    fn the_presets_mark_the_drafts_and_the_custom_row_shows_its_encode() {
        let mut draft = SettingsDraft::new(PipelineMode::Remux);
        let rows = map_presets(&get_preset_profiles(), &draft, None);
        let balanced = rows.iter().find(|row| row.id == "balanced").unwrap();
        assert!(balanced.active);
        assert_eq!(balanced.sub, "Opus 192k · complexity 5");
        assert_eq!(balanced.a11y, "Balanced, Opus 192k · complexity 5");
        assert_eq!(rows.iter().filter(|row| row.active).count(), 1);

        draft.bitrate = Some("320k".into());
        draft.complexity = Some("9".into());
        draft.preset = Some("custom".into());
        let rows = map_presets(&get_preset_profiles(), &draft, None);
        let custom = rows.iter().find(|row| row.id == "custom").unwrap();
        assert!(custom.active);
        assert_eq!(custom.sub, "Opus 320k · complexity 9");
        // The other rows keep their profile's encode.
        let balanced = rows.iter().find(|row| row.id == "balanced").unwrap();
        assert_eq!(balanced.sub, "Opus 192k · complexity 5");

        // A complexity that is not a number yet keeps the subtitle shown.
        draft.complexity = Some(String::new());
        let rows = map_presets(&get_preset_profiles(), &draft, Some(custom.sub.clone()));
        let custom = rows.iter().find(|row| row.id == "custom").unwrap();
        assert_eq!(custom.sub, "Opus 320k · complexity 9");
    }

    #[test]
    fn pending_edits_count_per_mode_and_key() {
        let (mut pending, published) = (PendingEdits::default(), DraftForm::default());
        pending.begin(Mode::Remux, Workers, &published);
        pending.begin(Mode::Remux, Workers, &published);
        assert!(pending.has(Mode::Remux, Workers));
        assert!(!pending.has(Mode::Extract, Workers));
        assert_eq!(pending.end(Mode::Remux, Workers), None, "one is left");
        assert_eq!(pending.end(Mode::Remux, Workers), Some(false));
        assert!(!pending.has(Mode::Remux, Workers));
        assert_eq!(
            pending.end(Mode::Remux, Workers),
            None,
            "an answer of a key none is pending of settles nothing"
        );
        assert_eq!(pending.end(Mode::Extract, IoJobs), None);
    }

    /// Ticks the mock clock until the sheet made or moved its focus: a tick makes the sheet,
    /// whose field takes the focus; the next runs its focus handler, the last the sheet's scroll.
    fn ticks() {
        for _ in 0..3 {
            mock_elapsed_time(Duration::from_millis(1));
        }
    }

    /// Return pressed, and released, in the window.
    fn press_return(app: &App) {
        use slint::platform::{Key, WindowEvent};
        let text: SharedString = Key::Return.into();
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
        ticks();
    }

    /// Opened on another mode, the sheet leaves its focused field first, under the field's own
    /// mode: an edited one is committed there, not into the mode shown next.
    #[test]
    fn opening_the_sheet_on_another_mode_leaves_the_focused_field_under_its_own() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        let sheet = app.global::<Sheet>();
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        for left in [false, true] {
            let (calls, shown) = (Rc::clone(&calls), app.as_weak());
            let record = move |key: SharedString| {
                let mode = shown.upgrade().unwrap().get_mode();
                calls.borrow_mut().push((left, key.to_string(), mode));
            };
            if left {
                sheet.on_left(move |key, _| record(key));
            } else {
                sheet.on_committed(record);
            }
        }
        app.invoke_open_sheet(Mode::Remux, "".into());
        sheet.invoke_enter("input_dir".into());
        sheet.set_changed_since(true);

        app.invoke_open_sheet(Mode::Extract, "input_dir".into());
        assert_eq!(
            *calls.borrow(),
            [
                (false, "input_dir".to_string(), Mode::Remux),
                (true, "input_dir".to_string(), Mode::Remux),
            ],
            "committed, then left, as remux's"
        );
        assert_eq!(app.get_mode(), Mode::Extract);
        assert!(!sheet.get_changed_since());
    }

    /// Done takes the focus when the sheet opens on no field, or on a key it shows no field of
    /// (one only the other mode's form has too), and when an open sheet is asked for no field; a field asked for again keeps it.
    #[test]
    fn the_sheet_focuses_done_unless_a_field_takes_the_focus() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
        app.show().unwrap();
        let sheet = app.global::<Sheet>();
        let open = |key: &str| {
            app.invoke_open_sheet(Mode::Remux, key.into());
            ticks();
        };

        open("nope");
        assert!(app.get_sheet_open());
        press_return(&app);
        assert!(
            !app.get_sheet_open(),
            "Done had the focus: Return closed it"
        );
        // A key only the other mode's form has (a flag from remux, the extract sheet open).
        app.invoke_open_sheet(Mode::Extract, "assets_dir".into());
        ticks();
        assert_eq!(sheet.get_focused(), "");
        press_return(&app);
        assert!(!app.get_sheet_open(), "Done had the focus");

        let left = Rc::new(std::cell::Cell::new(0));
        let count = Rc::clone(&left);
        sheet.on_left(move |_, _| count.set(count.get() + 1));
        open("input_dir");
        open("input_dir");
        assert_eq!(
            sheet.get_focused(),
            "input_dir",
            "the field keeps the focus"
        );
        assert_eq!(left.get(), 0, "a field that keeps the focus is not left");
        open("");
        assert_eq!(sheet.get_focused(), "");
        press_return(&app);
        assert!(!app.get_sheet_open(), "Done took the focus from the field");
        app.hide().unwrap();
    }

    /// The checkboxes send their edits when activated as a user does, here through a screen
    /// reader's default action; a field drawing its own placeholder still reads it out.
    #[test]
    fn the_checkboxes_send_their_edits_and_the_fields_read_their_placeholders() {
        use i_slint_backend_testing::{AccessibleRole, ElementHandle};
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        // Tall enough for the whole sheet: a search skips the controls scrolled out of view.
        app.window()
            .set_size(slint::LogicalSize::new(920.0, 1200.0));
        app.show().unwrap();
        let edited = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sent = Rc::clone(&edited);
        app.global::<Sheet>()
            .on_edited(move |key| sent.borrow_mut().push(key.to_string()));
        app.invoke_open_sheet(Mode::Remux, "".into());
        ticks();

        for label in [
            "Dry run (plan only, write nothing)",
            "Bypass inspection cache",
        ] {
            ElementHandle::find_by_accessible_label(&app, label)
                .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
                .unwrap()
                .invoke_accessible_default_action();
        }
        assert_eq!(*edited.borrow(), ["dry_run", "no_cache"]);
        let typed = app.get_remux_form();
        assert!(typed.dry_run && typed.no_cache);

        let input = ElementHandle::find_by_accessible_label(&app, "Input folder")
            .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .unwrap();
        assert_eq!(
            input.accessible_placeholder_text().as_deref(),
            Some("Release folder with the MKV episodes")
        );
        app.hide().unwrap();
    }

    /// The box and the slider of the complexity have focus ids of their own, so a focus moving
    /// between them, whichever handler runs first, leaves the complexity focused.
    #[test]
    fn the_focus_moves_between_the_complexity_box_and_slider() {
        i_slint_backend_testing::init_no_event_loop();
        let app = new_app();
        app.set_mode(Mode::Remux);
        app.set_sheet_open(true);
        let sheet = app.global::<Sheet>();
        let complexity_focused = || has_focus(&app, Mode::Remux, Complexity);
        // Box to slider: the slider's handler first, then the box's.
        sheet.invoke_enter("complexity".into());
        sheet.invoke_enter("complexity_slider".into());
        sheet.invoke_leave("complexity".into());
        assert_eq!(sheet.get_focused(), "complexity_slider");
        assert!(complexity_focused());
        // Slider to box: the slider's handler first.
        sheet.invoke_leave("complexity_slider".into());
        sheet.invoke_enter("complexity".into());
        assert_eq!(sheet.get_focused(), "complexity");
        assert!(complexity_focused());
    }
}
