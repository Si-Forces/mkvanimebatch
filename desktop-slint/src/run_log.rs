//! The Run Log drawer (P5): each mode's console, kept off the UI thread from the core's
//! `LogBatch`es and the `get_log` read at boot, the Log toggle's error badge, Copy, and the drawer
//! opening and closing. `log-view.js` and `app.js` (`setLogOpen`, `copyLog`, `boot`) are the
//! reference; the core decides what a console keeps, its display text and its error count.

use crate::publisher::SlintPublisher;
use mkv_anime_batch_slint_ui::{App, LogBadge, LogRow, LogTone, Mode};
use mkv_core::log_line::LogKind;
use mkv_core::pipeline::PipelineMode;
use mkv_desktop_core::actor::CoreHandle;
use mkv_desktop_core::log::{LogBatch, LogView, ShownLine};
use slint::{ComponentHandle, Model, Timer, TimerMode, VecModel};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

/// How long Copy says "Copied" or "Copy failed" (app.js `copyLog`).
const COPY_LABEL_FOR: Duration = Duration::from_millis(1500);

/// The tone a line of `kind` is drawn in (log-view.js `KIND_CLASS`).
fn tone(kind: LogKind) -> LogTone {
    match kind {
        LogKind::Error => LogTone::Err,
        LogKind::Warning => LogTone::Warn,
        LogKind::Skipped => LogTone::Skip,
        LogKind::Succeeded => LogTone::Ok,
        LogKind::Step => LogTone::Info,
        LogKind::Cache => LogTone::Done,
        LogKind::Note => LogTone::Plain,
    }
}

/// A line as its console draws it: paths Finder-style, the raw text kept for Copy.
fn map_line(line: &ShownLine) -> LogRow {
    LogRow {
        text: line.display.as_deref().unwrap_or(&line.text).into(),
        tone: tone(line.kind),
    }
}

/// The Log toggle's badge: the error count and its spoken form ("1 error"), or empty, which
/// hides it.
fn badge(errors: usize) -> LogBadge {
    match errors {
        0 => LogBadge::default(),
        1 => LogBadge {
            count: "1".into(),
            description: "1 error".into(),
        },
        n => LogBadge {
            count: n.to_string().into(),
            description: format!("{n} errors").into(),
        },
    }
}

/// What a hop writes into a mode's line model: it clears it first, then drops its `drop` oldest
/// rows and appends `rows`. The model then holds exactly the console's lines.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LogPatch {
    clear: bool,
    drop: usize,
    rows: Vec<LogRow>,
    /// The badge, when the error count may have changed.
    badge: Option<LogBadge>,
}

/// A mode's console, as log-view.js keeps one: the lines the core keeps from `first` on, under
/// its `generation`, and what the model still needs to show them.
#[derive(Debug)]
pub(crate) struct Console {
    generation: u64,
    /// The absolute index of `texts[0]`.
    first: u64,
    /// Each line's raw text, for Copy.
    texts: VecDeque<String>,
    /// The batches published before `get_log` answered, or `None` once it did.
    waiting: Option<Vec<LogBatch>>,
    patch: LogPatch,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            generation: 0,
            first: 0,
            texts: VecDeque::new(),
            waiting: Some(Vec::new()),
            patch: LogPatch::default(),
        }
    }
}

impl Console {
    /// Draws `batch`, or keeps it until `get_log` answered.
    pub(crate) fn apply(&mut self, batch: &LogBatch) {
        match &mut self.waiting {
            Some(waiting) => waiting.push(batch.clone()),
            None => self.draw(batch),
        }
    }

    /// Draws `view` (`get_log`'s answer, `None` when it failed), then the batches that waited for
    /// it: a line the answer already holds is not drawn twice. Only the first load counts:
    /// returns whether this one did.
    pub(crate) fn load(&mut self, view: Option<LogView>) -> bool {
        let Some(waiting) = self.waiting.take() else {
            return false;
        };
        if let Some(view) = view {
            self.draw(&LogBatch {
                mode: view.mode,
                generation: view.generation,
                first: view.first,
                keep_from: view.first,
                errors: view.errors,
                lines: view.lines,
            });
        }
        for batch in &waiting {
            self.draw(batch);
        }
        true
    }

    /// log-view.js `draw`: a newer generation empties the console, the lines it does not hold
    /// yet are appended, and every line below `keep_from` goes.
    fn draw(&mut self, batch: &LogBatch) {
        if batch.generation < self.generation {
            return;
        }
        let newer = batch.generation > self.generation;
        if newer {
            self.generation = batch.generation;
            self.first = batch.first;
            self.texts.clear();
            self.patch = LogPatch {
                clear: true,
                ..LogPatch::default()
            };
        }
        let held = self.texts.len() as u64;
        // A gap means lines were never sent here: the kept lines end right before this batch.
        if batch.first > self.first + held {
            self.first = batch.first - held;
        }
        let skip = usize::try_from(self.first + held - batch.first).unwrap_or(usize::MAX);
        let fresh = batch.lines.get(skip..).unwrap_or_default();
        for line in fresh {
            self.texts.push_back(line.text.clone());
            self.patch.rows.push(map_line(line));
        }
        // The core keeps nothing below `keep_from`: the model's rows go first, then the rows
        // not written yet.
        let drop = usize::try_from(batch.keep_from.saturating_sub(self.first))
            .unwrap_or(usize::MAX)
            .min(self.texts.len());
        if drop > 0 {
            let shown = self.texts.len() - self.patch.rows.len();
            let from_model = drop.min(shown);
            self.patch.drop += from_model;
            self.patch.rows.drain(..drop - from_model);
            self.texts.drain(..drop);
            self.first += drop as u64;
        }
        // A batch that adds nothing (one the load replays) keeps the newer count.
        if newer || !fresh.is_empty() {
            self.patch.badge = Some(badge(batch.errors));
        }
    }

    /// What Copy writes: every kept line's raw text, real paths and all.
    pub(crate) fn text(&self) -> String {
        let lines: Vec<&str> = self.texts.iter().map(String::as_str).collect();
        lines.join("\n")
    }

    /// The patch for the next hop; the model holds the console's lines once it is written.
    pub(crate) fn take_patch(&mut self) -> LogPatch {
        std::mem::take(&mut self.patch)
    }
}

/// The index of `mode`'s console.
pub(crate) fn slot(mode: PipelineMode) -> usize {
    match mode {
        PipelineMode::Remux => 0,
        PipelineMode::Extract => 1,
    }
}

/// Writes `patch` into `mode`'s line model, its serials (which its console follows on) and its
/// badge, on the event loop. Without the model nothing is written, so no patch half-applies.
pub(crate) fn apply(app: &App, mode: Mode, patch: LogPatch) {
    let (lines, mut serials) = match mode {
        Mode::Remux => (app.get_remux_log(), app.get_remux_log_serials()),
        Mode::Extract => (app.get_extract_log(), app.get_extract_log_serials()),
    };
    let Some(model) = lines.as_any().downcast_ref::<VecModel<LogRow>>() else {
        tracing::error!("[run_log] the line model is not installed; a log patch is lost");
        return;
    };
    let wrote = patch.clear || !patch.rows.is_empty();
    if patch.clear {
        model.clear();
        serials.cleared = serials.cleared.wrapping_add(1);
    }
    // ponytail: one removal per line, each a Vec shift of at most RUN_LOG_CAP rows; a burst
    // larger than the cap pays that once. Rebuild with `set_vec` if a profile ever shows it.
    for _ in 0..patch.drop.min(model.row_count()) {
        model.remove(0);
    }
    if !patch.rows.is_empty() {
        model.extend(patch.rows);
        serials.appended = serials.appended.wrapping_add(1);
    }
    // Each hop that wrote lines bumps one: the console follows once, after both the clear and the
    // lines are in.
    if wrote {
        match mode {
            Mode::Remux => app.set_remux_log_serials(serials),
            Mode::Extract => app.set_extract_log_serials(serials),
        }
    }
    if let Some(badge) = patch.badge {
        match mode {
            Mode::Remux => app.set_remux_log_badge(badge),
            Mode::Extract => app.set_extract_log_badge(badge),
        }
    }
}

/// Reads each mode's whole Run Log into its console, once, at boot (app.js `boot`). Each is asked
/// at call time, so a batch published after it waits for its answer and one published before is
/// in it.
pub(crate) fn load(
    core: &CoreHandle,
    publisher: &SlintPublisher,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let asks = [PipelineMode::Remux, PipelineMode::Extract].map(|mode| (mode, core.log(mode)));
    let publisher = publisher.clone();
    async move {
        for (mode, ask) in asks {
            let view = ask
                .await
                .inspect_err(|error| tracing::error!("[get_log] failed: {error}"))
                .ok();
            publisher.load_log(mode, view);
        }
    }
}

/// Writes text to the system clipboard; `Err` says why it could not.
pub(crate) type Clipboard = Box<dyn FnMut(&str) -> Result<(), String>>;

/// The system clipboard (arboard, which Slint's winit backend uses too: Slint has no app-level
/// clipboard call and drops the write's error). It is opened at the first Copy and kept, since on
/// X11 the copied text lives as long as its owner. A failed write drops it, so the next Copy
/// opens a fresh one (a lost connection does not fail every Copy after it).
pub(crate) fn system_clipboard() -> Clipboard {
    let mut board: Option<arboard::Clipboard> = None;
    Box::new(move |text| {
        let open = match &mut board {
            Some(open) => open,
            None => board.insert(arboard::Clipboard::new().map_err(|error| error.to_string())?),
        };
        let written = open.set_text(text).map_err(|error| error.to_string());
        if written.is_err() {
            board = None;
        }
        written
    })
}

/// Gives `app` each mode's line model; call it once, before the first publication.
pub(crate) fn install_models(app: &App) {
    app.set_remux_log(Rc::new(VecModel::<LogRow>::default()).into());
    app.set_extract_log(Rc::new(VecModel::<LogRow>::default()).into());
}

/// Wires Copy: the shown mode's raw lines go to `clipboard`, and Copy says "Copied" or "Copy
/// failed" for [`COPY_LABEL_FOR`]; a second Copy meanwhile starts the wait again.
pub(crate) fn wire(app: &App, publisher: &SlintPublisher, mut clipboard: Clipboard) {
    let (publisher, shown) = (publisher.clone(), app.as_weak());
    let reset = Rc::new(Timer::default());
    app.on_copy_log(move || {
        let Some(app) = shown.upgrade() else {
            return;
        };
        let text = publisher.log_text(crate::intents::pipeline(app.get_mode()));
        let label = match clipboard(&text) {
            Ok(()) => "Copied",
            Err(error) => {
                tracing::error!("[log] copy failed: {error}");
                "Copy failed"
            }
        };
        app.set_log_copy_label(label.into());
        let window = shown.clone();
        reset.start(TimerMode::SingleShot, COPY_LABEL_FOR, move || {
            if let Some(app) = window.upgrade() {
                app.set_log_copy_label("Copy".into());
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::tests::{Episodes, NoJobs, NoSamples};
    use crate::jobs::tests::{button, key, runtime, ticks, wired};
    use crate::publisher::install_models;
    use i_slint_backend_testing::{mock_elapsed_time, ElementHandle};
    use mkv_anime_batch_slint_ui::Sheet;
    use mkv_desktop_core::actor::{Core, Intent, Ports};
    use slint::platform::Key;
    use std::cell::{Cell, RefCell};

    const REMUX: PipelineMode = PipelineMode::Remux;

    fn line(kind: LogKind, text: &str) -> ShownLine {
        let display = mkv_desktop_core::log::log_display_text(text);
        ShownLine {
            kind,
            display: (display != text).then(|| display.into_owned()),
            text: text.into(),
        }
    }

    fn batch(generation: u64, first: u64, keep_from: u64, lines: &[(LogKind, &str)]) -> LogBatch {
        let lines: Vec<_> = lines.iter().map(|&(kind, text)| line(kind, text)).collect();
        let errors = lines.iter().filter(|l| l.kind == LogKind::Error).count();
        LogBatch {
            mode: REMUX,
            generation,
            first,
            keep_from,
            errors,
            lines,
        }
    }

    fn row(tone: LogTone, text: &str) -> LogRow {
        LogRow {
            text: text.into(),
            tone,
        }
    }

    /// log-view.js's rules: batches wait for the load, which a line is not drawn twice across;
    /// `keep_from` drops the model's rows first, then rows not written yet; a newer generation
    /// clears, an older one is ignored, and a gap starts the console at the batch.
    #[test]
    fn the_console_follows_the_load_generation_and_keep_from() {
        use LogKind::*;
        let mut console = Console::default();
        let a = (Note, "a");
        let b = (Error, "ERROR: b");
        console.apply(&batch(0, 0, 0, &[a]));
        console.apply(&batch(0, 1, 0, &[b]));
        assert_eq!(
            console.take_patch(),
            LogPatch::default(),
            "waits for the load"
        );

        // The answer holds `a`: the first batch adds nothing, the second `b`.
        let view = batch(0, 0, 0, &[a]);
        assert!(console.load(Some(LogView {
            mode: REMUX,
            generation: 0,
            first: 0,
            errors: 0,
            lines: view.lines,
        })));
        assert_eq!(
            console.take_patch(),
            LogPatch {
                rows: vec![row(LogTone::Plain, "a"), row(LogTone::Err, "ERROR: b")],
                badge: Some(badge(1)),
                ..LogPatch::default()
            }
        );
        // A second load is ignored.
        assert!(!console.load(None));
        assert_eq!(console.take_patch(), LogPatch::default());

        // `a` is below keep_from: one model row goes; the display text is Finder-style.
        console.apply(&batch(0, 2, 1, &[(Step, "Remuxing /x/y.mkv")]));
        assert_eq!(
            console.take_patch(),
            LogPatch {
                drop: 1,
                rows: vec![row(LogTone::Info, "Remuxing x › y.mkv")],
                badge: Some(LogBadge::default()),
                ..LogPatch::default()
            }
        );
        assert_eq!(
            console.text(),
            "ERROR: b\nRemuxing /x/y.mkv",
            "Copy's raw text"
        );

        // Two batches before a hop: the second trims `b` (shown) and `c`, `d` (not written yet).
        console.apply(&batch(0, 3, 1, &[(Succeeded, "c"), (Warning, "d")]));
        console.apply(&batch(0, 5, 5, &[(Skipped, "e"), (Cache, "f")]));
        let patch = console.take_patch();
        assert_eq!(patch.drop, 2);
        assert_eq!(
            patch.rows,
            [row(LogTone::Skip, "e"), row(LogTone::Done, "f")]
        );
        assert_eq!(console.text(), "e\nf");

        // A newer generation clears; an older one is then ignored.
        console.apply(&batch(1, 0, 0, &[]));
        console.apply(&batch(0, 7, 5, &[(Error, "ERROR: late")]));
        assert_eq!(
            console.take_patch(),
            LogPatch {
                clear: true,
                badge: Some(LogBadge::default()),
                ..LogPatch::default()
            }
        );
        assert_eq!(console.text(), "");

        // A gap: the console starts at the batch, and keep_from counts from there.
        console.apply(&batch(1, 5, 5, &[(Note, "g")]));
        console.apply(&batch(1, 6, 6, &[(Note, "h")]));
        let patch = console.take_patch();
        assert_eq!((patch.drop, patch.rows.len()), (0, 1));
        assert_eq!(console.text(), "h");
    }

    fn labels(app: &App, label: &str) -> usize {
        ElementHandle::find_by_accessible_label(app, label).count()
    }

    /// The drawer shows the mode's console and its badge, Copy writes the raw text and says so
    /// for 1.5 s, and Esc closes it unless the dialog or the sheet takes the key.
    #[test]
    fn the_drawer_shows_copies_and_closes() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
        let publisher = SlintPublisher::new(&app);
        let copied = Rc::new(RefCell::new(Vec::new()));
        let fail = Rc::new(Cell::new(false));
        let (into, failing) = (Rc::clone(&copied), Rc::clone(&fail));
        let clipboard: Clipboard = Box::new(move |text: &str| {
            if failing.get() {
                return Err("no clipboard".into());
            }
            into.borrow_mut().push(text.to_string());
            Ok(())
        });
        wire(&app, &publisher, clipboard);
        publisher.show_load(REMUX, None);
        publisher.show_load(PipelineMode::Extract, None);
        use LogKind::*;
        let lines = [(Step, "Remuxing /a/b.mkv"), (Error, "ERROR: broke")];
        publisher.show_logs(&[batch(0, 0, 0, &lines)]);
        assert_eq!(app.get_remux_log().row_count(), 2);
        assert_eq!(app.get_remux_log_badge().count, "1");
        assert_eq!(app.get_extract_log_badge().count, "");
        let toggle = button(&app, "Log");
        assert_eq!(toggle.accessible_description().as_deref(), Some("1 error"));
        assert_eq!(badge(3).description, "3 errors");

        toggle.invoke_accessible_default_action();
        ticks();
        assert!(app.get_log_open());
        assert_eq!(labels(&app, "Remuxing a › b.mkv"), 1, "the display text");

        // Copy: the raw text, then "Copied" for 1.5 s.
        button(&app, "Copy").invoke_accessible_default_action();
        assert_eq!(*copied.borrow(), ["Remuxing /a/b.mkv\nERROR: broke"]);
        assert_eq!(app.get_log_copy_label(), "Copied");
        mock_elapsed_time(Duration::from_millis(1499));
        assert_eq!(app.get_log_copy_label(), "Copied");
        mock_elapsed_time(Duration::from_millis(1));
        assert_eq!(app.get_log_copy_label(), "Copy");
        fail.set(true);
        button(&app, "Copy").invoke_accessible_default_action();
        assert_eq!(app.get_log_copy_label(), "Copy failed");
        mock_elapsed_time(COPY_LABEL_FOR);
        assert_eq!(app.get_log_copy_label(), "Copy");
        fail.set(false);

        // The extract console is its own: empty, with the prompt; Copy copies nothing.
        app.set_mode(Mode::Extract);
        ticks();
        assert_eq!(
            labels(&app, "No output yet. Run a batch to see its output here."),
            1
        );
        button(&app, "Copy").invoke_accessible_default_action();
        assert_eq!(copied.borrow().last().map(String::as_str), Some(""));
        app.set_mode(Mode::Remux);

        // Esc closes the drawer.
        key(&app, Key::Escape);
        assert!(!app.get_log_open());

        // With the sheet and its dialog open, Esc closes the dialog, then the sheet, then the
        // drawer.
        toggle.invoke_accessible_default_action();
        app.invoke_open_sheet(Mode::Remux, "no_cache".into());
        ticks();
        app.global::<Sheet>().invoke_clear_cache();
        ticks();
        assert!(app.get_dialog_open());
        key(&app, Key::Escape);
        assert!(!app.get_dialog_open() && app.get_sheet_open() && app.get_log_open());
        key(&app, Key::Escape);
        assert!(!app.get_sheet_open() && app.get_log_open());
        key(&app, Key::Escape);
        assert!(!app.get_log_open());

        // Close closes it; a new generation (a Run's start) empties the console and the badge.
        toggle.invoke_accessible_default_action();
        ticks();
        button(&app, "Close").invoke_accessible_default_action();
        assert!(!app.get_log_open());
        publisher.show_logs(&[batch(1, 0, 0, &[])]);
        assert_eq!(app.get_remux_log().row_count(), 0);
        assert_eq!(app.get_remux_log_badge().count, "");
        app.hide().unwrap();
    }

    /// At boot each console reads the core's Run Log, and a batch published before the answer is
    /// not drawn twice; one after it is drawn.
    #[test]
    fn get_log_at_boot_reads_the_run_log_into_each_console() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let app = App::new().unwrap();
        install_models(&app);
        let publisher = SlintPublisher::new(&app);
        let ports = Ports {
            inspector: Episodes,
            launcher: NoJobs,
            sampler: NoSamples,
        };
        let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
        // A Run refused for its empty input folder writes its error before the load.
        let refused = runtime.block_on(core.dispatch(Intent::Run { mode: REMUX }));
        assert!(refused.is_err());
        runtime.block_on(load(&core, &publisher));
        let cleared = runtime.block_on(core.dispatch(Intent::ClearCache { mode: REMUX }));
        assert_eq!(cleared, Ok(true));
        // The hops cannot run without an event loop: what they would take is still pending.
        publisher.flush();

        let lines = app.get_remux_log();
        let tones: Vec<_> = lines.iter().map(|line| line.tone).collect();
        assert_eq!(tones, [LogTone::Err, LogTone::Done], "each line once");
        assert_eq!(app.get_remux_log_badge().count, "1");
        assert_eq!(app.get_extract_log().row_count(), 0);
        let text = publisher.log_text(REMUX);
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with("ERROR: "), "{text}");
    }

    /// Lines added while the drawer is closed: it opens on the newest.
    #[test]
    fn a_closed_drawer_opens_on_the_newest_line() {
        i_slint_backend_testing::init_no_event_loop();
        let runtime = runtime();
        let (app, _core) = wired(&runtime, NoJobs);
        app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
        let publisher = SlintPublisher::new(&app);
        publisher.show_load(REMUX, None);
        let texts: Vec<_> = (0..200).map(|n| format!("line {n}")).collect();
        let lines: Vec<_> = texts
            .iter()
            .map(|text| (LogKind::Note, text.as_str()))
            .collect();
        publisher.show_logs(&[batch(0, 0, 0, &lines)]);
        ticks();
        assert!(!app.get_log_open());
        app.set_log_open(true);
        ticks();
        assert_eq!(labels(&app, "line 199"), 1, "the newest line is shown");
        assert_eq!(labels(&app, "line 0"), 0, "the oldest is scrolled out");
        app.hide().unwrap();
    }

    /// `count` notes from `line {from}` on, as one batch of `generation` starting at `first`.
    fn notes(generation: u64, first: u64, keep_from: u64, from: usize, count: usize) -> LogBatch {
        let texts: Vec<_> = (from..from + count).map(|n| format!("line {n}")).collect();
        let lines: Vec<_> = texts.iter().map(|t| (LogKind::Note, t.as_str())).collect();
        batch(generation, first, keep_from, &lines)
    }

    /// A wired window at 920x640 with remux's console loaded, its drawer open. It renders, so a
    /// frame lays the console out as the event loop's would.
    fn open_console(runtime: &tokio::runtime::Runtime) -> (App, CoreHandle, SlintPublisher) {
        use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
        slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        })))
        .unwrap();
        let (app, core) = wired(runtime, NoJobs);
        app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
        let publisher = SlintPublisher::new(&app);
        publisher.show_load(REMUX, None);
        app.set_log_open(true);
        (app, core, publisher)
    }

    /// A few frames, each as the event loop runs one: the change handlers, then a render.
    fn frames(app: &App) {
        for _ in 0..3 {
            mock_elapsed_time(Duration::from_millis(1));
            app.window().take_snapshot().unwrap();
        }
    }

    /// Scrolls the open console up, well off its newest lines.
    fn scroll_up(app: &App) {
        use i_slint_backend_testing::AccessibleLiveness;
        use slint::platform::WindowEvent;
        let list = ElementHandle::find_by_accessible_label(app, "Log")
            .find(|element| element.accessible_live_region() == Some(AccessibleLiveness::Polite))
            .unwrap();
        let (at, size) = (list.absolute_position(), list.size());
        let position =
            slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        for _ in 0..20 {
            app.window().dispatch_event(WindowEvent::PointerScrolled {
                position,
                delta_x: 0.0,
                delta_y: 400.0,
            });
            frames(app);
        }
    }

    /// A console scrolled up stops following; a new generation (a Run's start) empties it, and
    /// it follows the next Run's lines again.
    #[test]
    fn a_new_generation_follows_again_after_a_scroll_up() {
        let runtime = runtime();
        let (app, _core, publisher) = open_console(&runtime);
        let shown = |text: &str| labels(&app, text) == 1;
        publisher.show_logs(&[notes(0, 0, 0, 0, 200)]);
        frames(&app);
        assert!(shown("line 199"), "it follows");

        // Scrolled up over the console: the newest lines are out of view and stay so.
        scroll_up(&app);
        publisher.show_logs(&[notes(0, 200, 0, 200, 200)]);
        frames(&app);
        assert!(!shown("line 399"), "scrolled up, it does not follow");

        // A new Run: emptied, then its lines are followed.
        publisher.show_logs(&[notes(1, 0, 0, 0, 0)]);
        frames(&app);
        publisher.show_logs(&[notes(1, 0, 0, 1000, 200)]);
        frames(&app);
        assert!(shown("line 1199"), "the next Run is followed");
        app.hide().unwrap();
    }

    /// A new generation's clear that lands in one hop with its first lines (the console never
    /// shows empty) still follows them.
    #[test]
    fn a_coalesced_clear_and_lines_follow_after_a_scroll_up() {
        let runtime = runtime();
        let (app, _core, publisher) = open_console(&runtime);
        publisher.show_logs(&[notes(0, 0, 0, 0, 200)]);
        frames(&app);
        scroll_up(&app);
        assert_eq!(labels(&app, "line 199"), 0, "scrolled up");

        publisher.show_logs(&[notes(1, 0, 0, 0, 0), notes(1, 0, 0, 1000, 200)]);
        frames(&app);
        assert_eq!(labels(&app, "line 1199"), 1, "the next Run is followed");
        app.hide().unwrap();
    }

    /// At the core's cap each batch drops as many lines as it adds, so the content keeps its
    /// height: the newest lines are followed all the same.
    #[test]
    fn appends_at_the_cap_are_followed() {
        use mkv_desktop_core::log::RUN_LOG_CAP;
        let cap = RUN_LOG_CAP as u64;
        let runtime = runtime();
        let (app, _core, publisher) = open_console(&runtime);
        publisher.show_logs(&[notes(0, 0, 0, 0, RUN_LOG_CAP)]);
        frames(&app);
        assert_eq!(labels(&app, &format!("line {}", RUN_LOG_CAP - 1)), 1);

        for step in 0..3u64 {
            let first = cap + step * 5;
            let from = usize::try_from(first).unwrap();
            publisher.show_logs(&[notes(0, first, first + 5 - cap, from, 5)]);
            frames(&app);
            assert_eq!(app.get_remux_log().row_count(), RUN_LOG_CAP);
            let newest = format!("line {}", from + 4);
            assert_eq!(labels(&app, &newest), 1, "{newest} is shown");
        }
        app.hide().unwrap();
    }
}
